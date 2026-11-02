//! Usage-limit routing grants (`account_route_grants`): a session moved to
//! another login until the grant expires.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, Provider, SpawnTarget};
use crate::session_store::SessionStore;

/// The schema refuses a grant that ends before it starts and one whose
/// logins do not exist. How long a grant lasts is its writer's call (ADE-176).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteGrant {
    pub(crate) session_id: String,
    pub(crate) provider: Provider,
    pub(crate) from_account_id: String,
    pub(crate) to_account_id: String,
    pub(crate) granted_at: i64,
    pub(crate) expires_at: i64,
}

pub(crate) fn grant(store: &SessionStore, session_id: &str) -> Result<Option<RouteGrant>, String> {
    with_conn(store, |conn| {
        conn.query_row(
            "SELECT session_id, provider, from_account_id, to_account_id, granted_at, expires_at
               FROM account_route_grants WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok(RouteGrant {
                    session_id: row.get(0)?,
                    provider: row.get(1)?,
                    from_account_id: row.get(2)?,
                    to_account_id: row.get(3)?,
                    granted_at: row.get(4)?,
                    expires_at: row.get(5)?,
                })
            },
        )
        .optional()
    })
}

pub(crate) fn save_grant(store: &SessionStore, grant: &RouteGrant) -> Result<(), String> {
    with_conn(store, |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO account_route_grants
               (session_id, provider, from_account_id, to_account_id, granted_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                grant.session_id,
                grant.provider,
                grant.from_account_id,
                grant.to_account_id,
                grant.granted_at,
                grant.expires_at
            ],
        )
    })?;
    Ok(())
}

pub(crate) fn remove_grant(store: &SessionStore, session_id: &str) -> Result<(), String> {
    with_conn(store, |conn| {
        conn.execute(
            "DELETE FROM account_route_grants WHERE session_id = ?1",
            [session_id],
        )
    })?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_grant_get(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<Option<RouteGrant>, String> {
    grant(&store, &session_id)
}

#[tauri::command(async)]
pub fn spaces_grant_save(store: State<'_, SessionStore>, grant: RouteGrant) -> Result<(), String> {
    save_grant(&store, &grant)
}

#[tauri::command(async)]
pub fn spaces_grant_remove(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<(), String> {
    remove_grant(&store, &session_id)
}

/// The login a session's grant routes it to at `now`. Today: no routing.
pub(super) fn route(
    _store: &SessionStore,
    _session_id: &str,
    _now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
