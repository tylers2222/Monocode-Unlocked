//! Logins (`provider_accounts`) and the login each Space uses per provider
//! (`space_accounts`).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, Provider, SpawnTarget};
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

pub(crate) fn accounts(conn: &Connection) -> rusqlite::Result<Vec<ProviderAccount>> {
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
}

pub(crate) fn save_account(
    conn: &Connection,
    account: &ProviderAccount,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO provider_accounts (provider, id, label, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (provider, id) DO UPDATE SET label = excluded.label",
        params![account.provider, account.id, account.label, now],
    )?;
    Ok(())
}

pub(crate) fn remove_account(
    conn: &Connection,
    provider: Provider,
    id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM provider_accounts WHERE provider = ?1 AND id = ?2",
        params![provider, id],
    )?;
    Ok(())
}

pub(crate) fn logins(conn: &Connection) -> rusqlite::Result<Vec<SpaceLogin>> {
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
}

pub(crate) fn set_login(conn: &Connection, login: &SpaceLogin) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO space_accounts (space_id, provider, account_id) VALUES (?1, ?2, ?3)
         ON CONFLICT (space_id, provider) DO UPDATE SET account_id = excluded.account_id",
        params![login.space_id, login.provider, login.account_id],
    )?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_accounts_list(
    store: State<'_, SessionStore>,
) -> Result<Vec<ProviderAccount>, String> {
    with_conn(&store, accounts)
}

#[tauri::command(async)]
pub fn spaces_account_save(
    store: State<'_, SessionStore>,
    account: ProviderAccount,
) -> Result<(), String> {
    with_conn(&store, |conn| save_account(conn, &account, now_millis()))
}

#[tauri::command(async)]
pub fn spaces_account_remove(
    store: State<'_, SessionStore>,
    provider: Provider,
    id: String,
) -> Result<(), String> {
    with_conn(&store, |conn| remove_account(conn, provider, &id))
}

#[tauri::command(async)]
pub fn spaces_logins_list(store: State<'_, SessionStore>) -> Result<Vec<SpaceLogin>, String> {
    with_conn(&store, logins)
}

#[tauri::command(async)]
pub fn spaces_login_set(store: State<'_, SessionStore>, login: SpaceLogin) -> Result<(), String> {
    with_conn(&store, |conn| set_login(conn, &login))
}

/// The login a child starts with. Today: the one its caller passed.
pub(super) fn resolve(
    _store: &SessionStore,
    _session_id: &str,
    _provider: Option<&str>,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
