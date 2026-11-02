//! Checkout leases (`checkout_leases`): each working copy has one owner Space.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, SpawnTarget};
use crate::session_store::{now_millis, SessionStore};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutLease {
    pub(crate) checkout_path: String,
    pub(crate) space_id: String,
    pub(crate) leased_at: i64,
}

pub(crate) fn leases(conn: &Connection) -> rusqlite::Result<Vec<CheckoutLease>> {
    conn.prepare(
        "SELECT checkout_path, space_id, leased_at FROM checkout_leases ORDER BY checkout_path",
    )?
    .query_map([], |row| {
        Ok(CheckoutLease {
            checkout_path: row.get(0)?,
            space_id: row.get(1)?,
            leased_at: row.get(2)?,
        })
    })?
    .collect()
}

/// Records `space_id` as the checkout's owner. A checkout another Space
/// already owns stays with it: the insert fails on the primary key.
pub(crate) fn take_lease(
    conn: &Connection,
    checkout_path: &str,
    space_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO checkout_leases (checkout_path, space_id, leased_at) VALUES (?1, ?2, ?3)",
        params![checkout_path, space_id, now],
    )?;
    Ok(())
}

pub(crate) fn release_lease(conn: &Connection, checkout_path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM checkout_leases WHERE checkout_path = ?1",
        [checkout_path],
    )?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_leases_list(store: State<'_, SessionStore>) -> Result<Vec<CheckoutLease>, String> {
    with_conn(&store, leases)
}

#[tauri::command(async)]
pub fn spaces_lease_take(
    store: State<'_, SessionStore>,
    checkout_path: String,
    space_id: String,
) -> Result<(), String> {
    with_conn(&store, |conn| {
        take_lease(conn, &checkout_path, &space_id, now_millis())
    })
}

#[tauri::command(async)]
pub fn spaces_lease_release(
    store: State<'_, SessionStore>,
    checkout_path: String,
) -> Result<(), String> {
    with_conn(&store, |conn| release_lease(conn, &checkout_path))
}

/// The folder a child starts in. Today: the one its caller passed.
pub(super) fn claim(
    _store: &SessionStore,
    _session_id: &str,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
