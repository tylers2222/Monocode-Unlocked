//! Logins (`provider_accounts`) and the login each Space uses per provider
//! (`space_accounts`).

use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{membership, with_conn, Provider, SpawnTarget, FIRST_SPACE};
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

/// The project a session with no row runs in, as its saved `cwd` would name
/// it: `~` (no project) for the home folder, the main checkout's folder for
/// a linked work copy's (`worktrees::main_checkout_folder`), else the folder
/// as its caller spelled it.
fn unsaved_project(folder: &Path) -> String {
    if folder == crate::fs::expand_home("~") {
        return "~".into();
    }
    crate::worktrees::main_checkout_folder(folder)
        .unwrap_or_else(|| folder.to_path_buf())
        .to_string_lossy()
        .into_owned()
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
/// else its project's own login in its Space, else its Space's login for
/// that CLI (`project_login`). The project is the session's folder; for a
/// session with no row, its automation's folder, else the project its
/// child's folder stands for ([`unsaved_project`]). Space 1 with none set keeps the
/// CLI's own login, as every session did before Spaces; another Space
/// refuses. A session with no row yet (never saved, or its first save not
/// committed) is in the Space it would join. A child whose CLI keeps no
/// login profile starts as it was asked.
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
    let (space_id, name, project) = with_conn(store, |conn| {
        let space_id = membership::space_for_row(conn, session_id, None)?;
        let (name, folder): (String, Option<String>) = conn.query_row(
            "SELECT COALESCE((SELECT name FROM spaces WHERE id = ?1), ?1),
                    (SELECT cwd FROM sessions WHERE id = ?2)",
            params![space_id, session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let project = match folder {
            Some(folder) => Some(folder),
            None => membership::automation_project(conn, session_id)?,
        };
        Ok((space_id, name, project))
    })?;
    let project = project.unwrap_or_else(|| unsaved_project(&target.workdir));
    let login = super::project_login(store, &space_id, &project, provider)?;
    match login {
        Some(id) => Ok(SpawnTarget {
            account: Some(HarnessAccount {
                provider: provider.as_str().into(),
                id,
            }),
            ..target
        }),
        None if space_id == FIRST_SPACE => Ok(target),
        None => Err(format!("{name} has no {} login", cli_name(provider))),
    }
}
