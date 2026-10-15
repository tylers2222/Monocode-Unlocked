//! The Mono roster in `monocode.db`: one row per Mono, in rail order.
//!
//! The columns other code queries (`name`, `role`, `sort_order`) are columns;
//! the rest of the web app's `Mono` record is `data_json`. Every write bumps a
//! revision and broadcasts the whole roster, so each window's cache converges
//! on the database whatever order the events arrive in.

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, State};

use crate::session_store::{now_millis, validate_id, SessionStore};

pub const CHANGED_EVENT: &str = "monos_changed";
const NAME_MAX: usize = 256;
const DATA_MAX: usize = 256 * 1024;
const BATCH_MAX: usize = 1000;

/// Raised under the connection lock after each committed write, so a higher
/// revision always describes a later roster.
static REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonoRole {
    Giga,
    Mono,
}

impl MonoRole {
    fn as_str(self) -> &'static str {
        match self {
            MonoRole::Giga => "giga",
            MonoRole::Mono => "mono",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "giga" => Some(MonoRole::Giga),
            "mono" => Some(MonoRole::Mono),
            _ => None,
        }
    }
}

/// One Mono as a window sends it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MonoWrite {
    pub id: String,
    pub name: String,
    pub role: MonoRole,
    pub data: Map<String, Value>,
    pub sort_order: u32,
}

/// A write that passed [`validate`]; only these reach the database.
pub struct ValidMono {
    id: String,
    name: String,
    role: MonoRole,
    data_json: String,
    sort_order: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonoRow {
    pub id: String,
    pub name: String,
    pub role: MonoRole,
    pub data: Value,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Roster {
    pub revision: u64,
    pub monos: Vec<MonoRow>,
}

pub fn validate(mono: MonoWrite) -> Result<ValidMono, String> {
    validate_id(&mono.id, "Mono")?;
    let name = mono.name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err("Mono name must be 1 to 256 characters".into());
    }
    if mono.data.contains_key("id") || mono.data.contains_key("giga") {
        return Err("Mono data must not repeat its id or role".into());
    }
    let data_json = serde_json::to_string(&mono.data).map_err(|e| e.to_string())?;
    if data_json.len() > DATA_MAX {
        return Err("Mono data is too large".into());
    }
    Ok(ValidMono {
        id: mono.id,
        name: name.to_string(),
        role: mono.role,
        data_json,
        sort_order: i64::from(mono.sort_order),
    })
}

fn validate_batch(monos: Vec<MonoWrite>) -> Result<Vec<ValidMono>, String> {
    if monos.len() > BATCH_MAX {
        return Err("Too many Monos in one write".into());
    }
    let valid = monos
        .into_iter()
        .map(validate)
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids: Vec<&str> = valid.iter().map(|mono| mono.id.as_str()).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("The same Mono appears twice in one write".into());
    }
    Ok(valid)
}

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS monos (
           id TEXT PRIMARY KEY,
           name TEXT NOT NULL,
           role TEXT NOT NULL CHECK (role IN ('giga', 'mono')),
           data_json TEXT NOT NULL,
           sort_order INTEGER NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS monos_legacy_import (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           imported_at INTEGER NOT NULL
         );",
    )
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<MonoRow>> {
    conn.prepare(
        "SELECT id, name, role, data_json, sort_order, created_at, updated_at
           FROM monos ORDER BY sort_order, created_at, id",
    )?
    .query_map([], |row| {
        let role: String = row.get(2)?;
        let data: String = row.get(3)?;
        Ok(MonoRow {
            id: row.get(0)?,
            name: row.get(1)?,
            role: MonoRole::parse(&role).ok_or_else(|| invalid_column(2, "role"))?,
            data: serde_json::from_str(&data).map_err(|_| invalid_column(3, "data_json"))?,
            sort_order: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?
    .collect()
}

fn invalid_column(index: usize, name: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("invalid monos.{name}").into(),
    )
}

/// Inserts or replaces each Mono in one transaction. `created_at` is kept.
pub fn upsert_many(conn: &Connection, monos: &[ValidMono]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    write_rows(&tx, monos)?;
    tx.commit()
}

fn write_rows(conn: &Connection, monos: &[ValidMono]) -> rusqlite::Result<()> {
    let now = now_millis();
    let mut statement = conn.prepare(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           role = excluded.role,
           data_json = excluded.data_json,
           sort_order = excluded.sort_order,
           updated_at = excluded.updated_at",
    )?;
    for mono in monos {
        statement.execute(params![
            mono.id,
            mono.name,
            mono.role.as_str(),
            mono.data_json,
            mono.sort_order,
            now
        ])?;
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM monos WHERE id = ?1", params![id])?;
    Ok(())
}

/// The one-time move from WebKit storage. Imports only into an empty table and
/// only once: after it has run, an empty table means every Mono was deleted.
pub fn import_legacy(conn: &Connection, monos: &[ValidMono]) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let done = tx
        .query_row("SELECT 1 FROM monos_legacy_import", [], |_| Ok(()))
        .optional()?
        .is_some();
    let empty: bool = tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM monos)", [], |row| {
        row.get(0)
    })?;
    let import = !done && empty;
    if import {
        write_rows(&tx, monos)?;
    }
    if !done {
        tx.execute(
            "INSERT INTO monos_legacy_import (id, imported_at) VALUES (1, ?1)",
            params![now_millis()],
        )?;
    }
    tx.commit()?;
    Ok(import)
}

fn snapshot(conn: &Connection, revision: u64) -> Result<Roster, String> {
    Ok(Roster {
        revision,
        monos: list(conn).map_err(|e| e.to_string())?,
    })
}

/// Runs a write under the lock. When it changed the roster, raises the
/// revision and tells every window the new roster.
fn write(
    app: &AppHandle,
    store: &SessionStore,
    apply: impl FnOnce(&Connection) -> rusqlite::Result<bool>,
) -> Result<Roster, String> {
    let (roster, changed) = {
        let conn = store.lock_conn()?;
        let changed = apply(&conn).map_err(|e| e.to_string())?;
        let revision = if changed {
            REVISION.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            REVISION.load(Ordering::SeqCst)
        };
        (snapshot(&conn, revision)?, changed)
    };
    if changed {
        if let Err(err) = app.emit(CHANGED_EVENT, &roster) {
            eprintln!("monocode: monos changed event: {err}");
        }
    }
    Ok(roster)
}

#[tauri::command(async)]
pub fn monos_list(store: State<'_, SessionStore>) -> Result<Roster, String> {
    let conn = store.lock_conn()?;
    snapshot(&conn, REVISION.load(Ordering::SeqCst))
}

#[tauri::command(async)]
pub fn monos_upsert(
    app: AppHandle,
    store: State<'_, SessionStore>,
    mono: MonoWrite,
) -> Result<Roster, String> {
    let mono = validate(mono)?;
    write(&app, &store, |conn| {
        upsert_many(conn, &[mono]).map(|()| true)
    })
}

#[tauri::command(async)]
pub fn monos_upsert_many(
    app: AppHandle,
    store: State<'_, SessionStore>,
    monos: Vec<MonoWrite>,
) -> Result<Roster, String> {
    let monos = validate_batch(monos)?;
    write(&app, &store, |conn| {
        upsert_many(conn, &monos).map(|()| true)
    })
}

#[tauri::command(async)]
pub fn monos_delete(
    app: AppHandle,
    store: State<'_, SessionStore>,
    id: String,
) -> Result<Roster, String> {
    validate_id(&id, "Mono")?;
    write(&app, &store, |conn| delete(conn, &id).map(|()| true))
}

#[tauri::command(async)]
pub fn monos_import_legacy(
    app: AppHandle,
    store: State<'_, SessionStore>,
    monos: Vec<MonoWrite>,
) -> Result<Roster, String> {
    let monos = validate_batch(monos)?;
    write(&app, &store, |conn| import_legacy(conn, &monos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mono(id: &str, order: u32, data: Value) -> ValidMono {
        let write: MonoWrite = serde_json::from_value(json!({
            "id": id,
            "name": format!("Mono {id}"),
            "role": if id.starts_with('g') { "giga" } else { "mono" },
            "data": data,
            "sortOrder": order,
        }))
        .unwrap();
        validate(write).unwrap()
    }

    fn legacy() -> Vec<ValidMono> {
        vec![
            mono(
                "g1",
                0,
                json!({"mascot": "skull", "color": "#f00", "projects": ["/a"], "gigaSlug": "skull", "sessionId": "s1"}),
            ),
            mono(
                "m2",
                1,
                json!({"mascot": "crab", "color": "#0f0", "projects": []}),
            ),
            mono(
                "m3",
                2,
                json!({"mascot": "ghost", "color": "#00f", "projects": ["/b", "/c"], "name": "Boo"}),
            ),
        ]
    }

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn legacy_import_keeps_every_field_and_the_order_and_runs_once() {
        let conn = conn();
        assert!(import_legacy(&conn, &legacy()).unwrap());
        let first = list(&conn).unwrap();
        let summary: Vec<_> = first
            .iter()
            .map(|row| (row.id.as_str(), row.name.as_str(), row.role, row.sort_order))
            .collect();
        assert_eq!(
            summary,
            [
                ("g1", "Mono g1", MonoRole::Giga, 0),
                ("m2", "Mono m2", MonoRole::Mono, 1),
                ("m3", "Mono m3", MonoRole::Mono, 2),
            ]
        );
        assert_eq!(
            first[0].data,
            json!({"mascot": "skull", "color": "#f00", "projects": ["/a"], "gigaSlug": "skull", "sessionId": "s1"})
        );
        assert_eq!(
            first[2].data,
            json!({"mascot": "ghost", "color": "#00f", "projects": ["/b", "/c"], "name": "Boo"})
        );

        // A second start sends the same storage again; nothing changes.
        let changed = vec![mono("m9", 0, json!({"mascot": "crab"}))];
        assert!(!import_legacy(&conn, &changed).unwrap());
        assert_eq!(list(&conn).unwrap(), first);
    }

    #[test]
    fn deleting_every_mono_does_not_bring_them_back_from_storage() {
        let conn = conn();
        import_legacy(&conn, &legacy()).unwrap();
        for row in list(&conn).unwrap() {
            delete(&conn, &row.id).unwrap();
        }
        assert!(!import_legacy(&conn, &legacy()).unwrap());
        assert_eq!(list(&conn).unwrap(), []);
    }

    #[test]
    fn a_table_that_already_has_monos_is_never_merged_with_storage() {
        let conn = conn();
        upsert_many(&conn, &[mono("m7", 0, json!({"mascot": "crab"}))]).unwrap();
        assert!(!import_legacy(&conn, &legacy()).unwrap());
        let ids: Vec<_> = list(&conn).unwrap().into_iter().map(|row| row.id).collect();
        assert_eq!(ids, ["m7"]);
    }

    #[test]
    fn upsert_replaces_the_record_and_keeps_when_it_was_created() {
        let conn = conn();
        upsert_many(&conn, &legacy()).unwrap();
        conn.execute("UPDATE monos SET created_at = 5", []).unwrap();
        upsert_many(
            &conn,
            &[
                mono("m3", 0, json!({"mascot": "ghost", "projects": []})),
                mono("g1", 2, json!({"mascot": "skull"})),
            ],
        )
        .unwrap();
        let rows = list(&conn).unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["m3", "m2", "g1"]);
        assert_eq!(rows[0].data, json!({"mascot": "ghost", "projects": []}));
        assert!(rows.iter().all(|row| row.created_at == 5));
    }

    #[test]
    fn a_failing_row_rolls_back_the_whole_batch() {
        let conn = conn();
        conn.execute_batch(
            "CREATE TRIGGER refuse_m3 BEFORE INSERT ON monos WHEN NEW.id = 'm3'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
        assert!(upsert_many(&conn, &legacy()).is_err());
        assert_eq!(list(&conn).unwrap(), []);
    }

    #[test]
    fn the_boundary_refuses_malformed_monos() {
        let write = |value: Value| serde_json::from_value::<MonoWrite>(value);
        let base = json!({"id": "m1", "name": "Crab", "role": "mono", "data": {}, "sortOrder": 0});
        let with = |key: &str, value: Value| {
            let mut next = base.clone();
            next[key] = value;
            next
        };
        assert!(validate(write(base.clone()).unwrap()).is_ok());
        assert!(write(with("role", json!("admin"))).is_err());
        assert!(write(with("data", json!([]))).is_err());
        assert!(write(with("sortOrder", json!(-1))).is_err());
        assert!(write(with("extra", json!(1))).is_err());
        for bad in [
            with("id", json!("../x")),
            with("id", json!("")),
            with("name", json!("   ")),
            with("name", json!("x".repeat(257))),
            with("data", json!({"giga": true})),
            with("data", json!({"id": "m2"})),
            with("data", json!({"instructions": "x".repeat(DATA_MAX)})),
        ] {
            assert!(validate(write(bad.clone()).unwrap()).is_err(), "{bad}");
        }
        let twice = vec![write(base.clone()).unwrap(), write(base).unwrap()];
        assert!(validate_batch(twice).is_err());
    }
}
