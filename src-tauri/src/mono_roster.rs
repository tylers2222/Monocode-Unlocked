//! The Mono roster in `monocode.db`: one row per Mono, in rail order.
//!
//! The columns other code queries (`name`, `role`, `sort_order`) are columns;
//! the rest of the web app's `Mono` record is `data_json`. Windows send
//! operations, not copies of rows: a create never replaces, an update never
//! inserts and carries only what changed, so a window whose copy is stale
//! cannot undo another window's change. Every write bumps a revision and
//! broadcasts the whole roster, so each window's cache converges on the
//! database whatever order the events arrive in.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, State};

use crate::session_store::{now_millis, SessionStore};

pub const CHANGED_EVENT: &str = "monos_changed";
/// Refused updates start with this, so a window knows to reload the roster.
pub const UNKNOWN_MONO: &str = "unknown_mono";
const ID_MAX: usize = 64;
const NAME_MAX: usize = 256;
const MASCOT_MAX: usize = 64;
const DATA_MAX: usize = 256 * 1024;
const BATCH_MAX: usize = 1000;

/// Raised under the connection lock after each committed change, so a higher
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

/// A new Mono as a window sends it. No name: `monos.name` comes only from
/// `data`, see [`display_name`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MonoCreate {
    pub id: String,
    pub role: MonoRole,
    pub data: Map<String, Value>,
    pub sort_order: u32,
}

/// What changed on one Mono. `patch` is a JSON merge patch (RFC 7396) of its
/// data.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MonoUpdate {
    pub id: String,
    #[serde(default)]
    pub role: Option<MonoRole>,
    #[serde(default)]
    pub patch: Option<Map<String, Value>>,
    #[serde(default)]
    pub sort_order: Option<u32>,
}

/// A create that passed [`validate_create`].
pub struct ValidCreate {
    id: String,
    name: String,
    role: MonoRole,
    data: Map<String, Value>,
    sort_order: i64,
}

/// An update that passed [`validate_update`].
pub struct ValidUpdate {
    id: String,
    role: Option<MonoRole>,
    patch: Option<Map<String, Value>>,
    sort_order: Option<i64>,
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

/// The one Mono id rule, shared with `parseMono` in `mono.ts`:
/// `^[A-Za-z0-9-]{1,64}$`.
pub fn validate_mono_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > ID_MAX
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Invalid Mono id".into());
    }
    Ok(())
}

fn validate_data_keys(data: &Map<String, Value>) -> Result<(), String> {
    if data.contains_key("id") || data.contains_key("giga") {
        return Err("Mono data must not repeat its id or role".into());
    }
    Ok(())
}

fn data_json(data: &Map<String, Value>) -> Result<String, String> {
    let json = serde_json::to_string(data).map_err(|e| e.to_string())?;
    if json.len() > DATA_MAX {
        return Err("Mono data is too large".into());
    }
    Ok(json)
}

/// The name a Mono goes by until the user names it: "MonoCrab", "MonoShroom".
/// The same rule as `defaultMonoName` in `mono.ts`; both are checked against
/// `monoDefaultNames.json`.
fn default_mono_name(mascot: &str) -> String {
    let short = match mascot {
        "mushroom" => "Shroom".to_string(),
        _ => {
            let mut chars = mascot.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    format!("Mono{short}")
}

/// `monos.name`, from the data alone: the custom name when there is one, else
/// the mascot's default name. A window never sends it, so a window with a
/// stale copy cannot leave it on an old mascot.
fn display_name(data: &Map<String, Value>) -> Result<String, String> {
    if let Some(name) = data.get("name").and_then(Value::as_str).map(str::trim) {
        if !name.is_empty() {
            if name.chars().count() > NAME_MAX {
                return Err("Mono name must be 1 to 256 characters".into());
            }
            return Ok(name.to_string());
        }
    }
    match data.get("mascot").and_then(Value::as_str) {
        Some(mascot) if !mascot.is_empty() && mascot.chars().count() <= MASCOT_MAX => {
            Ok(default_mono_name(mascot))
        }
        _ => Err("Mono data needs a mascot of 1 to 64 characters".into()),
    }
}

pub fn validate_create(mono: MonoCreate) -> Result<ValidCreate, String> {
    validate_mono_id(&mono.id)?;
    validate_data_keys(&mono.data)?;
    data_json(&mono.data)?;
    Ok(ValidCreate {
        name: display_name(&mono.data)?,
        id: mono.id,
        role: mono.role,
        data: mono.data,
        sort_order: i64::from(mono.sort_order),
    })
}

pub fn validate_update(update: MonoUpdate) -> Result<ValidUpdate, String> {
    validate_mono_id(&update.id)?;
    if let Some(patch) = &update.patch {
        validate_data_keys(patch)?;
    }
    Ok(ValidUpdate {
        id: update.id,
        role: update.role,
        patch: update.patch,
        sort_order: update.sort_order.map(i64::from),
    })
}

fn validate_updates(updates: Vec<MonoUpdate>) -> Result<Vec<ValidUpdate>, String> {
    if updates.len() > BATCH_MAX {
        return Err("Too many Monos in one write".into());
    }
    let mut seen = HashSet::new();
    updates
        .into_iter()
        .map(|update| {
            if !seen.insert(update.id.clone()) {
                return Err("The same Mono appears twice in one write".into());
            }
            validate_update(update)
        })
        .collect()
}

/// The stored roster's rows that pass the boundary, first of each id. One bad
/// row must not keep every other Mono out of the database.
fn valid_legacy(monos: Vec<MonoCreate>) -> (Vec<ValidCreate>, Vec<String>) {
    let mut seen = HashSet::new();
    let mut valid = Vec::new();
    let mut refused = Vec::new();
    for (index, mono) in monos.into_iter().enumerate() {
        let id = mono.id.clone();
        if index >= BATCH_MAX {
            refused.push(format!("{id}: past the first {BATCH_MAX} Monos"));
            continue;
        }
        match validate_create(mono) {
            Ok(mono) if seen.insert(mono.id.clone()) => valid.push(mono),
            Ok(_) => refused.push(format!("{id}: appears twice")),
            Err(err) => refused.push(format!("{id}: {err}")),
        }
    }
    (valid, refused)
}

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS monos (
           id TEXT PRIMARY KEY
             CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^A-Za-z0-9-]*'),
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

/// Inserts each Mono. Fails, and inserts none, when one id already exists.
fn insert_rows(conn: &Connection, monos: &[ValidCreate]) -> Result<(), String> {
    let now = now_millis();
    let mut statement = conn
        .prepare(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        )
        .map_err(|e| e.to_string())?;
    for mono in monos {
        statement
            .execute(params![
                mono.id,
                mono.name,
                mono.role.as_str(),
                data_json(&mono.data)?,
                mono.sort_order,
                now
            ])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn create(conn: &Connection, mono: &ValidCreate) -> Result<(), String> {
    insert_rows(conn, std::slice::from_ref(mono))
}

/// RFC 7396: `null` removes a key, an object merges, anything else replaces.
fn merge_patch(target: &mut Value, patch: Value) {
    let Value::Object(patch) = patch else {
        *target = patch;
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(&key);
            } else {
                merge_patch(target.entry(key).or_insert(Value::Null), value);
            }
        }
    }
}

/// Applies every update in one transaction. An update to a Mono that is gone
/// fails the whole batch with [`UNKNOWN_MONO`]; it never inserts.
pub fn update_many(conn: &Connection, updates: &[ValidUpdate]) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let now = now_millis();
    for update in updates {
        let stored: Option<String> = tx
            .query_row(
                "SELECT data_json FROM monos WHERE id = ?1",
                params![update.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(stored) = stored else {
            return Err(format!("{UNKNOWN_MONO}: {}", update.id));
        };
        let mut data: Value = serde_json::from_str(&stored).map_err(|e| e.to_string())?;
        if let Some(patch) = &update.patch {
            merge_patch(&mut data, Value::Object(patch.clone()));
        }
        let Value::Object(data) = data else {
            return Err("Stored Mono data is not an object".into());
        };
        let name = display_name(&data)?;
        tx.execute(
            "UPDATE monos SET
               name = ?2,
               data_json = ?3,
               role = COALESCE(?4, role),
               sort_order = COALESCE(?5, sort_order),
               updated_at = ?6
             WHERE id = ?1",
            params![
                update.id,
                name,
                data_json(&data)?,
                update.role.map(MonoRole::as_str),
                update.sort_order,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Whether a Mono was deleted.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM monos WHERE id = ?1", params![id])? > 0)
}

/// The one-time move from WebKit storage. Imports only into an empty table and
/// only once: after it has run, an empty table means every Mono was deleted.
pub fn import_legacy(conn: &Connection, monos: &[ValidCreate]) -> Result<bool, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let done = tx
        .query_row("SELECT 1 FROM monos_legacy_import", [], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    let empty: bool = tx
        .query_row("SELECT NOT EXISTS(SELECT 1 FROM monos)", [], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;
    let import = !done && empty;
    if import {
        insert_rows(&tx, monos)?;
    }
    if !done {
        tx.execute(
            "INSERT INTO monos_legacy_import (id, imported_at) VALUES (1, ?1)",
            params![now_millis()],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
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
    apply: impl FnOnce(&Connection) -> Result<bool, String>,
) -> Result<Roster, String> {
    let (roster, changed) = {
        let conn = store.lock_conn()?;
        let changed = apply(&conn)?;
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
pub fn monos_create(
    app: AppHandle,
    store: State<'_, SessionStore>,
    mono: MonoCreate,
) -> Result<Roster, String> {
    let mono = validate_create(mono)?;
    write(&app, &store, |conn| create(conn, &mono).map(|()| true))
}

#[tauri::command(async)]
pub fn monos_update(
    app: AppHandle,
    store: State<'_, SessionStore>,
    updates: Vec<MonoUpdate>,
) -> Result<Roster, String> {
    let updates = validate_updates(updates)?;
    write(&app, &store, |conn| {
        update_many(conn, &updates).map(|()| !updates.is_empty())
    })
}

#[tauri::command(async)]
pub fn monos_delete(
    app: AppHandle,
    store: State<'_, SessionStore>,
    id: String,
) -> Result<Roster, String> {
    validate_mono_id(&id)?;
    write(&app, &store, |conn| {
        delete(conn, &id).map_err(|e| e.to_string())
    })
}

#[tauri::command(async)]
pub fn monos_import_legacy(
    app: AppHandle,
    store: State<'_, SessionStore>,
    monos: Vec<MonoCreate>,
) -> Result<Roster, String> {
    let (monos, refused) = valid_legacy(monos);
    write(&app, &store, |conn| {
        let imported = import_legacy(conn, &monos)?;
        // Every start sends the stored roster again; say what was left out
        // only on the start that imported.
        if imported && !refused.is_empty() {
            eprintln!("monocode: stored Monos left out of the import: {refused:?}");
        }
        Ok(imported)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_input(id: &str, order: u32, data: Value) -> MonoCreate {
        serde_json::from_value(json!({
            "id": id,
            "role": if id.starts_with('g') { "giga" } else { "mono" },
            "data": data,
            "sortOrder": order,
        }))
        .unwrap()
    }

    fn mono(id: &str, order: u32, data: Value) -> ValidCreate {
        validate_create(create_input(id, order, data)).unwrap()
    }

    fn update(value: Value) -> ValidUpdate {
        validate_update(serde_json::from_value(value).unwrap()).unwrap()
    }

    fn legacy() -> Vec<ValidCreate> {
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

    fn ids(conn: &Connection) -> Vec<String> {
        list(conn).unwrap().into_iter().map(|row| row.id).collect()
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
                ("g1", "MonoSkull", MonoRole::Giga, 0),
                ("m2", "MonoCrab", MonoRole::Mono, 1),
                ("m3", "Boo", MonoRole::Mono, 2),
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
    fn a_table_that_already_has_monos_is_never_merged_with_storage_then_or_later() {
        let conn = conn();
        create(&conn, &mono("m7", 0, json!({"mascot": "crab"}))).unwrap();
        assert!(!import_legacy(&conn, &legacy()).unwrap());
        assert_eq!(ids(&conn), ["m7"]);
        // The first import found Monos already there; it still counts as run.
        delete(&conn, "m7").unwrap();
        assert!(!import_legacy(&conn, &legacy()).unwrap());
        assert_eq!(list(&conn).unwrap(), []);
    }

    #[test]
    fn one_stored_mono_that_fails_the_boundary_does_not_keep_the_others_out() {
        let long = create_input(
            "m2",
            1,
            json!({"mascot": "crab", "name": "x".repeat(NAME_MAX + 1)}),
        );
        let (valid, refused) = valid_legacy(vec![
            create_input("g1", 0, json!({"mascot": "skull"})),
            long,
            create_input("mono_3", 2, json!({"mascot": "ghost"})),
            create_input("g1", 3, json!({"mascot": "crab"})),
            create_input("m4", 4, json!({"mascot": "ghost"})),
        ]);
        let conn = conn();
        assert!(import_legacy(&conn, &valid).unwrap());
        assert_eq!(ids(&conn), ["g1", "m4"]);
        assert_eq!(list(&conn).unwrap()[0].data, json!({"mascot": "skull"}));
        assert_eq!(refused.len(), 3);
    }

    #[test]
    fn stored_monos_past_the_limit_are_reported_as_left_out() {
        let monos = (0..=BATCH_MAX)
            .map(|index| create_input(&format!("m{index}"), 0, json!({"mascot": "crab"})))
            .collect();
        let (valid, refused) = valid_legacy(monos);
        assert_eq!(valid.len(), BATCH_MAX);
        assert_eq!(
            refused,
            [format!("m{BATCH_MAX}: past the first {BATCH_MAX} Monos")]
        );
    }

    #[test]
    fn create_never_replaces_an_existing_mono() {
        let conn = conn();
        create(
            &conn,
            &mono("m2", 0, json!({"mascot": "crab", "name": "Pinch"})),
        )
        .unwrap();
        assert!(create(&conn, &mono("m2", 1, json!({"mascot": "ghost"}))).is_err());
        assert_eq!(
            list(&conn).unwrap()[0].data,
            json!({"mascot": "crab", "name": "Pinch"})
        );
    }

    #[test]
    fn an_update_never_brings_back_a_deleted_mono() {
        let conn = conn();
        create(&conn, &mono("m2", 0, json!({"mascot": "crab"}))).unwrap();
        delete(&conn, "m2").unwrap();
        let err = update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"sessionId": "s9"}}))],
        )
        .unwrap_err();
        assert!(err.starts_with(UNKNOWN_MONO), "{err}");
        assert_eq!(list(&conn).unwrap(), []);
    }

    #[test]
    fn two_windows_that_change_different_fields_both_keep_their_change() {
        let conn = conn();
        import_legacy(&conn, &legacy()).unwrap();
        conn.execute("UPDATE monos SET created_at = 5", []).unwrap();
        // One window renames m2; another, which never saw the rename, gives it
        // a chat, removes its color and moves it first.
        update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"name": "Pinch"}}))],
        )
        .unwrap();
        update_many(
            &conn,
            &[
                update(json!({"id": "m2", "patch": {"sessionId": "s2", "color": null}, "sortOrder": 0, "role": "giga"})),
                update(json!({"id": "g1", "sortOrder": 1})),
            ],
        )
        .unwrap();
        let rows = list(&conn).unwrap();
        assert_eq!(rows[0].id, "m2");
        assert_eq!(rows[0].name, "Pinch");
        assert_eq!(rows[0].role, MonoRole::Giga);
        assert_eq!(
            rows[0].data,
            json!({"mascot": "crab", "projects": [], "name": "Pinch", "sessionId": "s2"})
        );
        assert_eq!(rows[1].id, "g1");
        assert!(rows.iter().all(|row| row.created_at == 5));
    }

    #[test]
    fn the_name_comes_from_the_data_even_when_two_windows_race() {
        let conn = conn();
        create(
            &conn,
            &mono("m2", 0, json!({"mascot": "crab", "name": "Pinch"})),
        )
        .unwrap();
        // Window A changes the mascot. Window B, which has not seen that,
        // clears the custom name.
        update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"mascot": "ghost"}}))],
        )
        .unwrap();
        assert_eq!(list(&conn).unwrap()[0].name, "Pinch");
        update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"name": null}}))],
        )
        .unwrap();
        assert_eq!(list(&conn).unwrap()[0].name, "MonoGhost");
        // Without a custom name, a mascot change moves the name with it.
        update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"mascot": "mushroom"}}))],
        )
        .unwrap();
        assert_eq!(list(&conn).unwrap()[0].name, "MonoShroom");
        // A Mono left with neither a name nor a mascot is refused.
        assert!(update_many(
            &conn,
            &[update(json!({"id": "m2", "patch": {"mascot": null}}))]
        )
        .is_err());
        assert_eq!(list(&conn).unwrap()[0].name, "MonoShroom");
    }

    #[test]
    fn default_names_match_the_shared_table() {
        let table: Map<String, Value> = serde_json::from_str(include_str!(
            "../../src/features/monos/model/monoDefaultNames.json"
        ))
        .unwrap();
        assert!(!table.is_empty());
        for (mascot, name) in table {
            assert_eq!(Value::String(default_mono_name(&mascot)), name, "{mascot}");
        }
    }

    #[test]
    fn a_failing_update_rolls_back_the_whole_batch() {
        let conn = conn();
        import_legacy(&conn, &legacy()).unwrap();
        let before = list(&conn).unwrap();
        let result = update_many(
            &conn,
            &[
                update(json!({"id": "g1", "patch": {"mascot": "crab"}, "sortOrder": 9})),
                update(json!({"id": "gone", "sortOrder": 0})),
            ],
        );
        assert!(result.is_err());
        assert_eq!(list(&conn).unwrap(), before);
    }

    #[test]
    fn the_boundary_refuses_malformed_monos() {
        let write = |value: Value| serde_json::from_value::<MonoCreate>(value);
        let base = json!({"id": "m1", "role": "mono", "data": {"mascot": "crab"}, "sortOrder": 0});
        let with = |key: &str, value: Value| {
            let mut next = base.clone();
            next[key] = value;
            next
        };
        assert!(validate_create(write(base.clone()).unwrap()).is_ok());
        assert!(validate_create(write(with("id", json!("a".repeat(ID_MAX)))).unwrap()).is_ok());
        assert!(write(with("role", json!("admin"))).is_err());
        assert!(write(with("data", json!([]))).is_err());
        assert!(write(with("sortOrder", json!(-1))).is_err());
        // The name comes from the data; a window cannot send one.
        assert!(write(with("name", json!("Crab"))).is_err());
        assert!(write(with("extra", json!(1))).is_err());
        for bad in [
            with("id", json!("../x")),
            with("id", json!("")),
            with("id", json!("mono_1")),
            with("id", json!("a".repeat(ID_MAX + 1))),
            with("data", json!({})),
            with("data", json!({"mascot": ""})),
            with("data", json!({"mascot": "crab", "name": "x".repeat(257)})),
            with("data", json!({"mascot": "crab", "giga": true})),
            with("data", json!({"mascot": "crab", "id": "m2"})),
            with(
                "data",
                json!({"mascot": "crab", "instructions": "x".repeat(DATA_MAX)}),
            ),
        ] {
            assert!(
                validate_create(write(bad.clone()).unwrap()).is_err(),
                "{bad}"
            );
        }

        let change = |value: Value| serde_json::from_value::<MonoUpdate>(value);
        assert!(change(json!({"id": "m1", "data": {}})).is_err());
        assert!(change(json!({"id": "m1", "name": "Crab"})).is_err());
        for bad in [
            json!({"id": "mono_1"}),
            json!({"id": "m1", "patch": {"giga": true}}),
            json!({"id": "m1", "patch": {"id": "m2"}}),
        ] {
            assert!(
                validate_update(change(bad.clone()).unwrap()).is_err(),
                "{bad}"
            );
        }
        let twice = vec![
            change(json!({"id": "m1"})).unwrap(),
            change(json!({"id": "m1"})).unwrap(),
        ];
        assert!(validate_updates(twice).is_err());
        assert!(validate_mono_id("mono_1").is_err());
    }
}
