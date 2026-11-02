//! Logins (`provider_accounts`), the login each Space uses per provider
//! (`space_accounts`), the Space a new session joins and the login its
//! children start with.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, Provider, SpawnTarget, FIRST_SPACE};
use crate::harness::{HarnessAccount, DEFAULT_PROVIDER_ACCOUNT_ID};
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

/// A login's label as TS `cleanLabel` writes it: whitespace collapsed,
/// trimmed, at most 48 characters, and not empty.
fn clean_label(label: &str) -> Result<String, String> {
    let label: String = label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect();
    if label.is_empty() {
        return Err("A provider account needs a label".into());
    }
    Ok(label)
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
    let label = clean_label(&account.label)?;
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO provider_accounts (provider, id, label, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (provider, id) DO UPDATE SET label = excluded.label",
            params![account.provider, account.id, label, now],
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

/// The Space a new session row starts in, never one its caller names: its
/// parent's (the lead that started it, the Mono whose chat it is, or the
/// session its `app-<parent>-<request>` id names, as a Mono chat names its
/// leads and habit runs), else the active Space.
pub(crate) fn new_session_space(
    conn: &Connection,
    id: &str,
    lead: Option<&str>,
) -> rusqlite::Result<String> {
    for parent in lead.into_iter().chain([id]).chain(named_parents(id)) {
        let space = conn
            .query_row(
                "SELECT space_id FROM sessions WHERE id = ?1
                 UNION ALL
                 SELECT space_id FROM monos WHERE json_extract(data_json, '$.sessionId') = ?1
                 LIMIT 1",
                [parent],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(space) = space {
            return Ok(space);
        }
    }
    conn.query_row(
        "SELECT active_space_id FROM space_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// The parents an `app-<parent>-<request>` id may name, longest first: a
/// parent id holds dashes of its own, so each dash may end it.
fn named_parents(id: &str) -> impl Iterator<Item = &str> {
    let rest = id.strip_prefix("app-").unwrap_or_default();
    rest.rmatch_indices('-').map(move |(end, _)| &rest[..end])
}

fn cli_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
        Provider::Devin => "Devin",
    }
}

/// The login a session's child starts with: the session's own (any but the
/// CLI's `default`, which the front end sends for a session without one),
/// else its Space's login for that CLI. Space 1 with none set keeps the
/// CLI's own login, as every session did before Spaces; another Space
/// refuses. A child that is no session (a model probe, a sign-in) or whose
/// CLI keeps no login profile starts as it was asked.
pub(super) fn resolve(
    store: &SessionStore,
    session_id: &str,
    _provider: Option<Provider>,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    let Some(provider) = target
        .account
        .as_ref()
        .filter(|account| account.id == DEFAULT_PROVIDER_ACCOUNT_ID)
        .and_then(|account| Provider::parse(&account.provider))
    else {
        return Ok(target);
    };
    let space: Option<(String, String, Option<String>)> = with_conn(store, |conn| {
        conn.query_row(
            "SELECT s.space_id, COALESCE(sp.name, s.space_id), sa.account_id
               FROM sessions s
               LEFT JOIN spaces sp ON sp.id = s.space_id
               LEFT JOIN space_accounts sa ON sa.space_id = s.space_id AND sa.provider = ?2
              WHERE s.id = ?1",
            params![session_id, provider],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
    })?;
    match space {
        None => Ok(target),
        Some((_, _, Some(id))) => Ok(SpawnTarget {
            account: Some(HarnessAccount {
                provider: provider.as_str().into(),
                id,
            }),
            ..target
        }),
        Some((space_id, _, None)) if space_id == FIRST_SPACE => Ok(target),
        Some((_, name, None)) => Err(format!("{name} has no {} login", cli_name(provider))),
    }
}
