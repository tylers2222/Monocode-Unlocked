//! The Mono completion results a window still owes (ADE-89). Every workspace
//! window boots from the one saved workspace row, so a result kept there
//! would reach each window that opens. Here each result is a row that one
//! window owns: a boot claims its own rows, in one `BEGIN IMMEDIATE`
//! transaction, and only the window that restores the earlier run takes the
//! rows no live window holds. A row whose result is in a notice names
//! that notice; the transaction that saves a Mono chat holding the notice
//! marks it delivered and deletes its rows. A row lives only while a Mono
//! chat save can still settle it: the window deletes the rows of a session
//! its Mono stopped watching, and a claim deletes the rows of a Mono that has
//! no chat. The row is the result's `SavedMonoCompletion` JSON, opaque to
//! Rust except for its request id, its Mono, its session and its notice.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use tauri::{Manager, Runtime, State, WebviewWindow};

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

/// A delivery mark only has to outlive a retry of the same notice: 30 days.
const DELIVERY_MARK_MS: i64 = 2_592_000_000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_completions (
           request_id   TEXT PRIMARY KEY,
           saved_json   TEXT NOT NULL,
           window_label TEXT NOT NULL,
           window_run   TEXT NOT NULL
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

/// A window's boot: it returns the rows it holds, and when `earlier` (the
/// window restores the earlier run, ADE-93) it first takes every row no open
/// window holds (`live` names the open ones): an earlier run's, and this
/// run's rows of a closed window. Rows another open window holds stay its
/// own. A notice opens its Mono chat in the window that holds the result, and
/// ADE-93 gives the Mono chats to that window only, so no other window takes
/// a row it does not hold. A row whose notice a Mono chat already saved is
/// delivered first, and a taken row whose Mono has no chat is deleted.
pub(crate) fn claim(
    conn: &Connection,
    owner: Owner,
    earlier: bool,
    live: &[&str],
    now: i64,
) -> rusqlite::Result<Vec<Value>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    adopt_workspace_row(&tx)?;
    tx.execute(
        "DELETE FROM mono_completion_deliveries WHERE delivered_at < ?1",
        params![now - DELIVERY_MARK_MS],
    )?;
    settle(&tx, None, now)?;
    tx.execute(
        "UPDATE mono_completions SET window_label = ?1, window_run = ?2
         WHERE ?4 AND (window_run <> ?2
           OR window_label NOT IN (SELECT value FROM json_each(?3)))",
        params![
            owner.window,
            owner.run,
            serde_json::to_string(live).unwrap_or_default(),
            earlier
        ],
    )?;
    tx.execute(
        "DELETE FROM mono_completions WHERE window_label = ?1 AND window_run = ?2
           AND json_extract(saved_json, '$.origin.monoId') NOT IN (SELECT id FROM sessions)",
        params![owner.window, owner.run],
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

/// The rows of Mono chat `mono` (every Mono when None) whose notice that
/// chat's saved queue or transcript holds: the notice is delivered, marked
/// and the rows deleted in the caller's transaction.
pub(crate) fn settle(conn: &Connection, mono: Option<&str>, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO mono_completion_deliveries (group_id, delivered_at)
         SELECT DISTINCT c.notice, ?2 FROM (
           SELECT json_extract(saved_json, '$.notice') AS notice,
                  json_extract(saved_json, '$.origin.monoId') AS mono
           FROM mono_completions) c
         WHERE c.notice IS NOT NULL AND (?1 IS NULL OR c.mono = ?1)
           AND (EXISTS (SELECT 1 FROM sessions s, json_each(s.queued_messages_json) q
                        WHERE s.id = c.mono AND json_extract(q.value, '$.id') = c.notice)
             OR EXISTS (SELECT 1 FROM mono_blocks b
                        WHERE b.session_id = c.mono
                          AND json_extract(b.block_json, '$.appRequestId') = c.notice))",
        params![mono, now],
    )?;
    conn.execute(
        "DELETE FROM mono_completions
         WHERE json_extract(saved_json, '$.notice') IN (SELECT group_id FROM mono_completion_deliveries)",
        [],
    )?;
    Ok(())
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
                   (request_id, saved_json, window_label, window_run)
                 VALUES (?1, ?2, '', '')",
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

/// What the window owes now: its rows become exactly `saved`, in one
/// commit, except that a row already in a notice stays until a Mono chat
/// saves that notice. A row another window holds is left as it is.
pub(crate) fn hold(conn: &Connection, owner: Owner, saved: &[Value]) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM mono_completions WHERE window_label = ?1 AND window_run = ?2
           AND json_extract(saved_json, '$.notice') IS NULL",
        params![owner.window, owner.run],
    )
    .map_err(|e| e.to_string())?;
    for entry in saved {
        tx.execute(
            "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (request_id) DO UPDATE SET saved_json = excluded.saved_json
             WHERE window_label = excluded.window_label AND window_run = excluded.window_run",
            params![
                request_id(entry)?,
                entry.to_string(),
                owner.window,
                owner.run
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// The window's Mono `mono` stopped watching `session`: no save of the
/// Mono chat will hold a result for it, so its rows go.
pub(crate) fn dismiss(
    conn: &Connection,
    owner: Owner,
    mono: &str,
    session: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM mono_completions WHERE window_label = ?1 AND window_run = ?2
           AND json_extract(saved_json, '$.origin.monoId') = ?3
           AND json_extract(saved_json, '$.request.sessionId') = ?4",
        params![owner.window, owner.run, mono, session],
    )?;
    Ok(())
}

#[tauri::command(async)]
pub fn mono_completions_claim<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
) -> Result<Vec<Value>, String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    // Under the lock no other claim or hold runs, so the open windows read
    // here are the ones at the claim: a window that opens later holds none.
    let conn = store.lock_conn()?;
    let open = window.webview_windows();
    let live: Vec<&str> = open.keys().map(String::as_str).collect();
    let earlier = store.claim_earlier_run(window.label());
    claim(&conn, owner, earlier, &live, crate::mono_messages::now_ms()).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn mono_completions_hold<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
    saved: Vec<Value>,
) -> Result<(), String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    hold(&*store.lock_conn()?, owner, &saved)
}

#[tauri::command(async)]
pub fn mono_completions_dismiss<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    run: State<'_, Run>,
    mono_id: String,
    session_id: String,
) -> Result<(), String> {
    let owner = Owner {
        window: window.label(),
        run: &run.0,
    };
    dismiss(&*store.lock_conn()?, owner, &mono_id, &session_id).map_err(|e| e.to_string())
}
