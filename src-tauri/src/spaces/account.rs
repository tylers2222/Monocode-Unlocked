//! Logins (`provider_accounts`) and the login each Space uses per provider
//! (`space_accounts`).

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, Provider, SpawnTarget};
use crate::harness::DEFAULT_PROVIDER_ACCOUNT_ID;
use crate::session_store::{now_millis, SessionStore};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccount {
    pub(crate) provider: Provider,
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceLogin {
    pub(crate) space_id: String,
    pub(crate) provider: Provider,
    pub(crate) account_id: String,
}

/// The one rule for a login id, shared with its folder under
/// `provider-accounts` (`harness::provider_account_path`) and `validAccountId`
/// in TS.
pub(crate) fn check_account_id(id: &str) -> Result<(), String> {
    let valid = (1..=80).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err("Invalid provider account id".into())
    }
}

pub(crate) fn accounts(store: &SessionStore) -> Result<Vec<ProviderAccount>, String> {
    with_conn(store, |conn| {
        conn.prepare(
            "SELECT provider, id, label FROM provider_accounts ORDER BY provider, created_at, id",
        )?
        .query_map([], |row| {
            Ok(ProviderAccount {
                provider: row.get(0)?,
                id: row.get(1)?,
                label: row.get(2)?,
            })
        })?
        .collect()
    })
}

pub(crate) fn save_account(
    store: &SessionStore,
    account: &ProviderAccount,
    now: i64,
) -> Result<(), String> {
    check_account_id(&account.id)?;
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO provider_accounts (provider, id, label, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (provider, id) DO UPDATE SET label = excluded.label",
            params![account.provider, account.id, account.label, now],
        )
    })?;
    Ok(())
}

/// Removes a login, and with it every Space's choice of it and every grant
/// from or to it. Each CLI's own `default` login stays.
pub(crate) fn remove_account(
    store: &SessionStore,
    provider: Provider,
    id: &str,
) -> Result<(), String> {
    if id == DEFAULT_PROVIDER_ACCOUNT_ID {
        return Err("The default provider account cannot be removed".into());
    }
    with_conn(store, |conn| {
        conn.execute(
            "DELETE FROM provider_accounts WHERE provider = ?1 AND id = ?2",
            params![provider, id],
        )
    })?;
    Ok(())
}

pub(crate) fn logins(store: &SessionStore) -> Result<Vec<SpaceLogin>, String> {
    with_conn(store, |conn| {
        conn.prepare(
            "SELECT space_id, provider, account_id FROM space_accounts ORDER BY space_id, provider",
        )?
        .query_map([], |row| {
            Ok(SpaceLogin {
                space_id: row.get(0)?,
                provider: row.get(1)?,
                account_id: row.get(2)?,
            })
        })?
        .collect()
    })
}

pub(crate) fn set_login(store: &SessionStore, login: &SpaceLogin) -> Result<(), String> {
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO space_accounts (space_id, provider, account_id) VALUES (?1, ?2, ?3)
             ON CONFLICT (space_id, provider) DO UPDATE SET account_id = excluded.account_id",
            params![login.space_id, login.provider, login.account_id],
        )
    })?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_accounts_list(
    store: State<'_, SessionStore>,
) -> Result<Vec<ProviderAccount>, String> {
    accounts(&store)
}

#[tauri::command(async)]
pub fn spaces_account_save(
    store: State<'_, SessionStore>,
    account: ProviderAccount,
) -> Result<(), String> {
    save_account(&store, &account, now_millis())
}

#[tauri::command(async)]
pub fn spaces_account_remove(
    store: State<'_, SessionStore>,
    provider: Provider,
    id: String,
) -> Result<(), String> {
    remove_account(&store, provider, &id)
}

#[tauri::command(async)]
pub fn spaces_logins_list(store: State<'_, SessionStore>) -> Result<Vec<SpaceLogin>, String> {
    logins(&store)
}

#[tauri::command(async)]
pub fn spaces_login_set(store: State<'_, SessionStore>, login: SpaceLogin) -> Result<(), String> {
    set_login(&store, &login)
}

/// The login a child starts with. Today: the one its caller passed.
pub(super) fn resolve(
    _store: &SessionStore,
    _session_id: &str,
    _provider: Option<Provider>,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
