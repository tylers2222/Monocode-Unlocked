//! The Mono completion results a window still owes (ADE-89). Every workspace
//! window boots from the one saved workspace row, so a result kept there
//! would reach each window that opens. Here each result is a row that one
//! window owns: a boot claims the rows no live window holds, in one
//! `BEGIN IMMEDIATE` transaction, and a delivery deletes its rows in the
//! transaction that marks the notice delivered. The row is the result's
//! `SavedMonoCompletion` JSON, opaque to Rust except for its request id.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::session_store::SessionStore;

/// This app process. A claim made under another run belongs to a window
/// that died with its process.
pub struct Run(String);

impl Default for Run {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// The window that holds a row, in the run that opened it.
#[derive(Clone, Copy)]
pub(crate) struct Owner<'a> {
    pub window: &'a str,
    pub run: &'a str,
}

/// A delivery mark only has to outlive a retry of the same notice.
const DELIVERY_MARK_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_completions (
           request_id   TEXT PRIMARY KEY,
           saved_json   TEXT NOT NULL,
           window_label TEXT NOT NULL,
           window_run   TEXT NOT NULL,
           claimed_at   INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS mono_completion_deliveries (
           group_id     TEXT PRIMARY KEY,
           delivered_at INTEGER NOT NULL
         );",
    )
}

fn request_id(saved: &Value) -> Result<&str, String> {
    saved
        .pointer("/request/requestId")
        .and_then(Value::as_str)
        .ok_or_else(|| "a saved completion has no request.requestId".into())
}

/// A window's boot: it takes every row of an earlier run (or released by a
/// closed window) and the rows its own earlier boot held, and returns all
/// it now holds. Rows another live window of this run holds stay its own.
pub(crate) fn claim(conn: &Connection, owner: Owner, now: i64) -> rusqlite::Result<Vec<Value>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    adopt_workspace_row(&tx)?;
    tx.execute(
        "UPDATE mono_completions SET window_label = ?1, window_run = ?2, claimed_at = ?3
         WHERE window_run <> ?2 OR window_label = ?1",
        params![owner.window, owner.run, now],
    )?;
    tx.execute(
        "DELETE FROM mono_completion_deliveries WHERE delivered_at < ?1",
        params![now - DELIVERY_MARK_MS],
    )?;
    let held: Vec<String> = tx
        .prepare(
            "SELECT saved_json FROM mono_completions
             WHERE window_label = ?1 AND window_run = ?2 ORDER BY rowid",
        )?
        .query_map(params![owner.window, owner.run], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    tx.commit()?;
    Ok(held
        .iter()
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect())
}

/// Before ADE-89 a quit kept the results in the workspace row under
/// `monoCompletions`. The first claim moves them here, unowned, and takes
/// the key out of the row, so the row carries none again.
fn adopt_workspace_row(tx: &Transaction) -> rusqlite::Result<()> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT snapshot_json FROM workspace_snapshot WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(mut snapshot) = raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else {
        return Ok(());
    };
    let Some(kept) = snapshot
        .as_object_mut()
        .and_then(|row| row.remove("monoCompletions"))
    else {
        return Ok(());
    };
    for saved in kept.as_array().into_iter().flatten() {
        if let Ok(id) = request_id(saved) {
            tx.execute(
                "INSERT OR IGNORE INTO mono_completions
                   (request_id, saved_json, window_label, window_run, claimed_at)
                 VALUES (?1, ?2, '', '', 0)",
                params![id, saved.to_string()],
            )?;
        }
    }
    tx.execute(
        "UPDATE workspace_snapshot SET snapshot_json = ?1 WHERE id = 1",
        params![snapshot.to_string()],
    )?;
    Ok(())
}

/// Writes results the window holds: new ones, and ones whose turn ended.
/// A row another window holds is left as it is.
pub(crate) fn save(
    conn: &Connection,
    owner: Owner,
    now: i64,
    saved: &[Value],
) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    for entry in saved {
        tx.execute(
            "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run, claimed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (request_id) DO UPDATE SET saved_json = excluded.saved_json
             WHERE window_label = excluded.window_label AND window_run = excluded.window_run",
            params![request_id(entry)?, entry.to_string(), owner.window, owner.run, now],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// The notice `group_id` carries `request_ids`. True when this call
/// delivers it: no other window holds one of them, and no earlier call
/// marked the group delivered. The mark and the deletion are one commit.
pub(crate) fn deliver(
    conn: &Connection,
    owner: Owner,
    now: i64,
    group_id: &str,
    request_ids: &[String],
) -> rusqlite::Result<bool> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for id in request_ids {
        let elsewhere: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM mono_completions WHERE request_id = ?1
               AND NOT (window_label = ?2 AND window_run = ?3))",
            params![id, owner.window, owner.run],
            |row| row.get(0),
        )?;
        if elsewhere {
            return Ok(false);
        }
    }
    let marked = tx.execute(
        "INSERT OR IGNORE INTO mono_completion_deliveries (group_id, delivered_at) VALUES (?1, ?2)",
        params![group_id, now],
    )? == 1;
    remove_in(&tx, owner, request_ids)?;
    tx.commit()?;
    Ok(marked)
}

/// Results the window will not send: dismissed, or reported in another notice.
pub(crate) fn remove(
    conn: &Connection,
    owner: Owner,
    request_ids: &[String],
) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    remove_in(&tx, owner, request_ids)?;
    tx.commit()
}

fn remove_in(tx: &Transaction, owner: Owner, request_ids: &[String]) -> rusqlite::Result<()> {
    for id in request_ids {
        tx.execute(
            "DELETE FROM mono_completions WHERE request_id = ?1 AND window_label = ?2 AND window_run = ?3",
            params![id, owner.window, owner.run],
        )?;
    }
    Ok(())
}

/// A closed window's rows go back to the next window that boots.
pub(crate) fn release(conn: &Connection, owner: Owner) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_completions SET window_run = '' WHERE window_label = ?1 AND window_run = ?2",
        params![owner.window, owner.run],
    )
    .map(drop)
}

pub fn window_closed(app: &AppHandle, label: &str) {
    let (Some(store), Some(run)) = (app.try_state::<SessionStore>(), app.try_state::<Run>()) else {
        return;
    };
    let released = store.lock_conn().and_then(|conn| {
        release(
            &conn,
            Owner {
                window: label,
                run: &run.0,
            },
        )
        .map_err(|e| e.to_string())
    });
    if let Err(error) = released {
        eprintln!("monocode: release the completion results of {label}: {error}");
    }
}

#[tauri::command(async)]
pub fn mono_completions_claim(
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
) -> Result<Vec<Value>, String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    claim(&*store.lock_conn()?, owner, crate::mono_messages::now_ms()).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn mono_completions_save(
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
    saved: Vec<Value>,
) -> Result<(), String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    save(
        &*store.lock_conn()?,
        owner,
        crate::mono_messages::now_ms(),
        &saved,
    )
}

#[tauri::command(async)]
pub fn mono_completions_deliver(
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
    group_id: String,
    request_ids: Vec<String>,
) -> Result<bool, String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    deliver(
        &*store.lock_conn()?,
        owner,
        crate::mono_messages::now_ms(),
        &group_id,
        &request_ids,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn mono_completions_remove(
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
    request_ids: Vec<String>,
) -> Result<(), String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    remove(&*store.lock_conn()?, owner, &request_ids).map_err(|e| e.to_string())
}
