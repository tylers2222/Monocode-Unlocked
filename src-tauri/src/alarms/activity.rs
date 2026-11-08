//! `agent_activity` (design section 8): one row per session that ran a turn
//! or a tapped child on this build. Only the detector pass writes it: the
//! turn and stdio taps' records (`live`) own disjoint columns, so none
//! overwrites another: turns (`turn_*`), the stdio tap (progress, tools,
//! dialog, todo list) and the detector's own `wake_pending_since_active_ms`.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};

use super::live::Write;
use super::state::{Background, Dialog, DialogKind, Progress, Turn};
use super::taps::{Harness, Snapshot};
use crate::mono_messages::SessionId;

/// `hold_state` and `hold_missing_reason` are the sleep hold's (row H).
pub(crate) fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_activity (
           session_id              TEXT PRIMARY KEY,
           harness                 TEXT CHECK (harness IN ('claude', 'codex', 'pi')),
           turn_state              TEXT NOT NULL DEFAULT 'idle'
                                     CHECK (turn_state IN ('idle', 'running')),
           turn_id                 TEXT,
           turn_started_active_ms  INTEGER,
           turn_ended_active_ms    INTEGER NOT NULL,
           last_progress_active_ms INTEGER,
           last_progress_wall_ms   INTEGER,
           tool_running            INTEGER NOT NULL DEFAULT 0 CHECK (tool_running IN (0, 1)),
           background_running      INTEGER NOT NULL DEFAULT 0 CHECK (background_running IN (0, 1)),
           background_ended_active_ms INTEGER,
           dialog_kind             TEXT CHECK (dialog_kind IN ('permission', 'question', 'plan')),
           dialog_request_id       TEXT,
           dialog_text             TEXT,
           dialog_since_active_ms  INTEGER,
           todo_open               INTEGER NOT NULL DEFAULT 0 CHECK (todo_open IN (0, 1)),
           wake_pending_since_active_ms INTEGER,
           hold_state              TEXT CHECK (hold_state IN ('installed', 'missing')),
           hold_missing_reason     TEXT,
           updated_wall_ms         INTEGER NOT NULL,
           CHECK ((dialog_kind IS NULL) = (dialog_since_active_ms IS NULL))
         );",
    )
}

/// The clock's launch, in the lease holder only: no turn outlives the app,
/// so a turn the last run left running ended by `active_ms`, the stored
/// active time, which a restart does not move. A process without the lease
/// shares the holder's live rows and must leave them.
pub(crate) fn end_all_running(conn: &Connection, active_ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_activity SET turn_state = 'idle', turn_ended_active_ms = ?1
         WHERE turn_state = 'running'",
        [active_ms],
    )?;
    Ok(())
}

/// Each pass, after it wrote the taps' records, every row agrees with
/// `running` (`ControlHost`'s turns, read before the store lock) by `at`,
/// however a record was lost or came out of order (review M2, S1 r3 F3):
/// a row still running a turn `running` does not hold ended, and a row not
/// running the turn `running` holds for it started. `recorded` names the
/// sessions to leave: a turn record of theirs came after `running` was read.
pub(crate) fn reconcile(
    conn: &Connection,
    running: &HashMap<String, String>,
    recorded: &HashSet<&str>,
    at: i64,
    wall: i64,
) -> rusqlite::Result<()> {
    let rows = conn
        .prepare("SELECT session_id, turn_state, turn_id FROM agent_activity")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)? == "running",
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (session, is_running, turn_id) in rows {
        if recorded.contains(session.as_str()) {
            continue;
        }
        match (running.get(&session), is_running) {
            (Some(live), true) if Some(live) == turn_id.as_ref() => {}
            (Some(live), _) => {
                conn.execute(
                    "UPDATE agent_activity SET turn_state = 'running', turn_id = ?2,
                       turn_started_active_ms = ?3, updated_wall_ms = ?4
                     WHERE session_id = ?1",
                    params![session, live, at, wall],
                )?;
            }
            (None, true) => {
                conn.execute(
                    "UPDATE agent_activity SET turn_state = 'idle', turn_ended_active_ms = ?2,
                       updated_wall_ms = ?3
                     WHERE session_id = ?1",
                    params![session, at, wall],
                )?;
            }
            (None, false) => {}
        }
    }
    Ok(())
}

/// One record of the taps, in the order they saw it, by the pass at
/// `now` (active, wall).
pub(crate) fn apply(
    conn: &Connection,
    session: &SessionId,
    write: &Write,
    now: (i64, i64),
) -> rusqlite::Result<()> {
    let wall = now.1;
    let wall_at = |at: i64| crate::active_clock::wall_at(conn, at, now);
    match write {
        Write::TurnStarted { turn_id, at } => turn_started(conn, session, turn_id, *at, wall),
        Write::TurnEnded { turn_id, at } => turn_ended(conn, session, turn_id, *at, wall),
        Write::Spawned { harness, at } => {
            spawned(conn, session, *harness, (*at, wall_at(*at)?), wall)
        }
        Write::Tap(row) => write_tap(conn, session, row, wall_at(row.last_progress_at)?, wall),
    }
}

/// Turn `turn_id` of `session` was authorized at active time `at`. Each
/// turn starts once, so a repeat of the same authorize moves nothing, nor
/// does a start that lands after its own turn's end.
fn turn_started(
    conn: &Connection,
    session: &SessionId,
    turn_id: &str,
    at: i64,
    wall: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_activity (session_id, turn_state, turn_id, turn_started_active_ms,
           turn_ended_active_ms, updated_wall_ms)
         VALUES (?1, 'running', ?2, ?3, ?3, ?4)
         ON CONFLICT (session_id) DO UPDATE SET turn_state = 'running', turn_id = ?2,
           turn_started_active_ms = ?3, updated_wall_ms = ?4
         WHERE turn_id IS NOT ?2",
        params![session.as_str(), turn_id, at, wall],
    )?;
    Ok(())
}

/// Turn `turn_id` of `session` ended at `at`. Only that turn's end counts:
/// a kill's end that lands after the next turn started changes nothing,
/// and a second end of one turn keeps the first. An idle row takes the id,
/// so the start of a turn a kill ended before its start landed (review
/// M2) finds its id ended and moves nothing.
fn turn_ended(
    conn: &Connection,
    session: &SessionId,
    turn_id: &str,
    at: i64,
    wall: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_activity (session_id, turn_state, turn_id, turn_ended_active_ms,
           updated_wall_ms)
         VALUES (?1, 'idle', ?2, ?3, ?4)
         ON CONFLICT (session_id) DO UPDATE SET turn_state = 'idle', turn_id = ?2,
           turn_ended_active_ms = CASE WHEN turn_state = 'running' THEN ?3
                                       ELSE turn_ended_active_ms END,
           updated_wall_ms = ?4
         WHERE (turn_state = 'running' AND turn_id = ?2) OR turn_state = 'idle'",
        params![session.as_str(), turn_id, at, wall],
    )?;
    Ok(())
}

/// A new child of `session` was spawned at `at`: what its tap knows starts
/// over, and `harness` names the tap that reads it (`None`: no tap).
fn spawned(
    conn: &Connection,
    session: &SessionId,
    harness: Option<Harness>,
    (at, at_wall): (i64, i64),
    wall: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_activity (session_id, harness, turn_ended_active_ms,
           last_progress_active_ms, last_progress_wall_ms, updated_wall_ms)
         VALUES (?1, ?2, ?3, ?3, ?5, ?4)
         ON CONFLICT (session_id) DO UPDATE SET harness = ?2, last_progress_active_ms = ?3,
           last_progress_wall_ms = ?5, tool_running = 0, background_running = 0,
           background_ended_active_ms = NULL, todo_open = 0, dialog_kind = NULL,
           dialog_request_id = NULL, dialog_text = NULL, dialog_since_active_ms = NULL,
           updated_wall_ms = ?4",
        params![
            session.as_str(),
            harness.map(Harness::as_str),
            at,
            wall,
            at_wall
        ],
    )?;
    Ok(())
}

/// A snapshot that moved no progress keeps the progress's wall time.
fn write_tap(
    conn: &Connection,
    session: &SessionId,
    row: &Snapshot,
    progress_wall: i64,
    wall: i64,
) -> rusqlite::Result<()> {
    let dialog = row.dialog.as_ref();
    conn.execute(
        "INSERT INTO agent_activity (session_id, harness, turn_ended_active_ms,
           last_progress_active_ms, tool_running, todo_open, dialog_kind, dialog_request_id,
           dialog_text, dialog_since_active_ms, updated_wall_ms, background_running,
           background_ended_active_ms, last_progress_wall_ms)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (session_id) DO UPDATE SET harness = ?2,
           last_progress_wall_ms = CASE WHEN last_progress_active_ms IS ?3
                                        THEN last_progress_wall_ms ELSE ?13 END,
           last_progress_active_ms = ?3, tool_running = ?4, todo_open = ?5, dialog_kind = ?6, dialog_request_id = ?7,
           dialog_text = ?8, dialog_since_active_ms = ?9, updated_wall_ms = ?10,
           background_running = ?11, background_ended_active_ms = ?12",
        params![
            session.as_str(),
            row.harness.as_str(),
            row.last_progress_at,
            row.tool_running,
            row.todo_open,
            dialog.map(|dialog| kind_text(dialog.kind)),
            dialog.and_then(|dialog| dialog.request_id.as_deref()),
            dialog.map(|dialog| dialog.text.as_str()),
            dialog.map(|dialog| dialog.since),
            wall,
            row.background,
            row.background_ended_at,
            progress_wall,
        ],
    )?;
    Ok(())
}

fn kind_text(kind: DialogKind) -> &'static str {
    match kind {
        DialogKind::Permission => "permission",
        DialogKind::Question => "question",
        DialogKind::Plan => "plan",
    }
}

fn parse_kind(text: &str) -> Option<DialogKind> {
    [
        DialogKind::Permission,
        DialogKind::Question,
        DialogKind::Plan,
    ]
    .into_iter()
    .find(|kind| kind_text(*kind) == text)
}

/// A row as the detector reads it.
#[derive(Debug, Clone)]
pub(crate) struct Activity {
    pub session: SessionId,
    pub turn: Turn,
    pub background: Background,
    pub dialog: Option<Dialog>,
    pub dialog_text: Option<String>,
    pub todo_open: bool,
    pub wake_pending_since: Option<i64>,
}

pub(crate) fn all(conn: &Connection) -> rusqlite::Result<Vec<Activity>> {
    let rows = conn
        .prepare(
            "SELECT session_id, harness, turn_state, turn_started_active_ms, turn_ended_active_ms,
               last_progress_active_ms, tool_running, dialog_kind, dialog_since_active_ms,
               dialog_text, todo_open, wake_pending_since_active_ms, background_running,
               background_ended_active_ms
             FROM agent_activity ORDER BY session_id",
        )?
        .query_map([], |row| {
            let Some(session) = SessionId::parse(&row.get::<_, String>(0)?) else {
                return Ok(None);
            };
            let started: Option<i64> = row.get(3)?;
            let turn = match (row.get::<_, String>(2)?.as_str(), started) {
                ("running", Some(started_at)) => Turn::Running {
                    started_at,
                    progress: match row.get::<_, Option<String>>(1)? {
                        Some(_) => Progress::Tapped {
                            last_at: row.get::<_, Option<i64>>(5)?.unwrap_or(started_at),
                            tool_running: row.get(6)?,
                        },
                        None => Progress::Untapped,
                    },
                },
                _ => Turn::Idle {
                    started_at: started,
                    ended_at: row.get(4)?,
                },
            };
            let dialog = match (row.get::<_, Option<String>>(7)?, row.get(8)?) {
                (Some(kind), Some(since)) => parse_kind(&kind).map(|kind| Dialog { kind, since }),
                _ => None,
            };
            let background = match (row.get(12)?, row.get(13)?) {
                (true, _) => Background::Running,
                (false, Some(at)) => Background::Ended { at },
                (false, None) => Background::None,
            };
            Ok(Some(Activity {
                session,
                turn,
                background,
                dialog,
                dialog_text: row.get(9)?,
                todo_open: row.get(10)?,
                wake_pending_since: row.get(11)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

/// The detector's own column: when it first saw a wake waiting.
pub(crate) fn set_wake_pending(
    conn: &Connection,
    session: &SessionId,
    since: Option<i64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_activity SET wake_pending_since_active_ms = ?2 WHERE session_id = ?1",
        params![session.as_str(), since],
    )?;
    Ok(())
}

pub(crate) fn forget(conn: &Connection, session: &SessionId) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM agent_activity WHERE session_id = ?1",
        [session.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_of(conn: &Connection) -> Turn {
        all(conn).unwrap().remove(0).turn
    }

    // Defect (review M2): a kill whose end is written before its turn's
    // start leaves the row running for a turn that is over; or a late end
    // of an old turn idles the turn that replaced it.
    #[test]
    fn a_start_after_its_own_turns_end_moves_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_table(&conn).unwrap();
        let session = SessionId::parse("s-1").unwrap();
        turn_started(&conn, &session, "t1", 10, 0).unwrap();
        turn_ended(&conn, &session, "t1", 20, 0).unwrap();
        turn_ended(&conn, &session, "t2", 30, 0).unwrap();
        turn_started(&conn, &session, "t2", 31, 0).unwrap();
        assert_eq!(
            turn_of(&conn),
            Turn::Idle {
                started_at: Some(10),
                ended_at: 20
            }
        );
        turn_started(&conn, &session, "t3", 40, 0).unwrap();
        turn_ended(&conn, &session, "t2", 50, 0).unwrap();
        assert!(matches!(
            turn_of(&conn),
            Turn::Running { started_at: 40, .. }
        ));
        turn_ended(&conn, &session, "t3", 60, 0).unwrap();
        assert_eq!(
            turn_of(&conn),
            Turn::Idle {
                started_at: Some(40),
                ended_at: 60
            }
        );
    }

    // Defect (S1 r3 F3): a stale start and its re-check end that reach the
    // hub after the next turn's start idle the row while that turn runs,
    // and no pass heals it.
    #[test]
    fn a_row_idled_by_a_stale_turn_runs_the_live_turn_by_the_next_pass() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_table(&conn).unwrap();
        let session = SessionId::parse("s-1").unwrap();
        turn_ended(&conn, &session, "t1", 10, 0).unwrap();
        turn_started(&conn, &session, "t2", 20, 0).unwrap();
        turn_started(&conn, &session, "t1", 21, 0).unwrap();
        turn_ended(&conn, &session, "t1", 22, 0).unwrap();
        assert!(matches!(turn_of(&conn), Turn::Idle { .. }));
        let running = HashMap::from([("s-1".to_string(), "t2".to_string())]);
        reconcile(&conn, &running, &HashSet::from(["s-1"]), 30, 0).unwrap();
        assert!(
            matches!(turn_of(&conn), Turn::Idle { .. }),
            "a record in this take may be newer than the running set"
        );
        reconcile(&conn, &running, &HashSet::new(), 35, 0).unwrap();
        assert!(matches!(
            turn_of(&conn),
            Turn::Running { started_at: 35, .. }
        ));
        reconcile(&conn, &HashMap::new(), &HashSet::new(), 40, 0).unwrap();
        assert!(matches!(turn_of(&conn), Turn::Idle { ended_at: 40, .. }));
    }
}
