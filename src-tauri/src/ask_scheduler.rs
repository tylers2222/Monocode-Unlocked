//! The ask scheduler (Change R plan v2, R8). Each active clock tick turns
//! the open asks that fell due in active time into rows of `mono_outbox`,
//! one row per ask and stage, keyed `reminder:<ask>:<stage>`. Nothing lives
//! in memory, so a restart finds the pending reminders in the database.
//! R9: the window that owns a reminder's chat claims it, queues it and
//! marks it sent once its provider took it. R11: the `why` stage tells the
//! parent and writes the ask's `miss`; the ask stays open.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use tauri::{Runtime, State, WebviewWindow};

use crate::ask_gate::{self, Actor, Delivery};
use crate::control::ControlHost;
use crate::mono_messages::{self, Notices, SessionId};
use crate::session_store::SessionStore;

/// Who a rung's outbox row goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum To {
    Child,
    /// The child missed its deadline: the parent is told, and the rung
    /// writes the ask's `miss` event.
    Parent,
}

/// One stage of an open ask's ladder: its outbox name, when it falls due
/// relative to the ask's deadline, and who it goes to.
struct Rung {
    stage: &'static str,
    from_due_ms: i64,
    to: To,
}

/// Plan section 2: the parent hears of a miss 5 active minutes after the
/// deadline reminder.
const WHY_AFTER_MS: i64 = 5 * 60_000;

/// The ladder in order. `mono_asks.stage` counts the rungs already enqueued:
/// 0 is no reminder yet, 1 is `t5`, 2 is `deadline` and 3 is `why`.
const LADDER: &[Rung] = &[
    Rung {
        stage: "t5",
        from_due_ms: -5 * 60_000,
        to: To::Child,
    },
    Rung {
        stage: "deadline",
        from_due_ms: 0,
        to: To::Child,
    },
    Rung {
        stage: "why",
        from_due_ms: WHY_AFTER_MS,
        to: To::Parent,
    },
];

/// `ask_id` is nullable: a later `resume:<session>:<boot>` row has no ask.
/// Then every open, unpaused, timed ask with no next due and a rung left
/// gets that rung's due time: an ask written before this build (R5 left
/// `next_due_active_ms` NULL), or one at the end of a shorter ladder when a
/// later build appends rungs. Only NULL rows change, so a second open
/// changes nothing.
pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_outbox (
           key          TEXT PRIMARY KEY,
           ask_id       TEXT REFERENCES mono_asks(id),
           stage        TEXT NOT NULL CHECK (stage IN
                          ('t5', 'deadline', 'why', 'blocked', 'resume')),
           recipient    TEXT NOT NULL,
           at_active_ms INTEGER NOT NULL,
           claimed_by   TEXT,
           sent_at      INTEGER
         );",
    )?;
    for (done, rung) in (0_i64..).zip(LADDER) {
        conn.execute(
            "UPDATE mono_asks SET next_due_active_ms = due_active_ms + ?2
              WHERE state = 'open' AND stage = ?1 AND due_active_ms IS NOT NULL
                AND paused_at_active_ms IS NULL AND next_due_active_ms IS NULL",
            params![done, rung.from_due_ms],
        )?;
    }
    Ok(())
}

/// The next rung for an ask that has `done` rungs enqueued.
fn next_rung(done: i64) -> Option<&'static Rung> {
    LADDER.get(usize::try_from(done).ok()?)
}

/// When the next rung of an ask due at `due_active_ms` with `done` rungs
/// enqueued falls due; `None` past the last rung.
pub(crate) fn next_due(due_active_ms: i64, done: i64) -> Option<i64> {
    next_rung(done).map(|rung| due_active_ms + rung.from_due_ms)
}

/// Enqueues the reminder of each open, unpaused ask whose next rung fell due
/// by `active_ms`, and moves the ask to the rung after it. A paused or
/// untimed ask is never selected. A key enqueued before (its stage write
/// lost) keeps its one row; an ask that a later build moved past the end of
/// this ladder is left alone. The `why` rung goes to the parent and writes
/// the ask's one `miss` event, with no reason yet (R11). Names the chat of
/// each recipient, so the window that owns it delivers the reminder (R9).
pub(crate) fn run(conn: &Connection, active_ms: i64, wall_ms: i64) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let fell_due: Vec<(String, i64, i64, String, String)> = tx
        .prepare(
            "SELECT id, stage, due_active_ms, child_actor, parent_actor FROM mono_asks
              WHERE state = 'open' AND paused_at_active_ms IS NULL
                AND next_due_active_ms <= ?1",
        )?
        .query_map([active_ms], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut notices = Notices::default();
    for (id, done, due_active_ms, child, parent) in fell_due {
        let Some(rung) = next_rung(done) else {
            continue;
        };
        let recipient = match rung.to {
            To::Child => child,
            To::Parent => parent,
        };
        let key = format!("reminder:{id}:{}", rung.stage);
        tx.execute(
            "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (key) DO NOTHING",
            params![key, id, rung.stage, recipient, active_ms],
        )?;
        if rung.to == To::Parent {
            tx.execute(
                "INSERT INTO mono_ask_events (ask_id, kind, seq, at_active_ms, at_wall_ms)
                 VALUES (?1, 'miss', 1, ?2, ?3) ON CONFLICT (ask_id, kind, seq) DO NOTHING",
                params![id, active_ms, wall_ms],
            )?;
        }
        tx.execute(
            "UPDATE mono_asks SET stage = ?1, next_due_active_ms = ?2 WHERE id = ?3",
            params![done + 1, next_due(due_active_ms, done + 1), id],
        )?;
        name_chat(&tx, &mut notices, &recipient)?;
    }
    tx.commit()?;
    Ok(notices)
}

/// Plan R8, when `SessionStore` opens: no window outlives the app, so a
/// reminder that a window claimed and did not send is pending again.
pub(crate) fn startup(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_outbox SET claimed_by = NULL
          WHERE sent_at IS NULL AND claimed_by IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// A reminder a window may deliver (R9), as `mono_outbox_pending` lists it.
/// `child` is the `send_to` of the ask's child when the reminder goes to
/// someone else: the parent's `why` notice names who missed (R11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub key: String,
    pub ask_id: Option<String>,
    pub stage: String,
    pub child: Option<String>,
}

/// The chat a reminder to `recipient` reaches: a lead's or worker's
/// session, or a Mono's current chat; `None` for a Mono with no chat.
fn chat_of(conn: &Connection, recipient: &str) -> rusqlite::Result<Option<SessionId>> {
    Ok(match Actor::parse(recipient) {
        Some(Actor::Session(session)) => Some(session),
        Some(Actor::Mono(mono)) => mono_messages::mono_row(conn, &mono)?.and_then(|row| row.chat),
        None => None,
    })
}

fn name_chat(conn: &Connection, notices: &mut Notices, recipient: &str) -> rusqlite::Result<()> {
    if let Some(chat) = chat_of(conn, recipient)? {
        notices.name(chat);
    }
    Ok(())
}

/// The reminders for chat `session` that wait and no window holds, in the
/// order they were enqueued: its own and, when it is a Mono's current chat,
/// that Mono's.
pub(crate) fn pending(conn: &Connection, session: &SessionId) -> rusqlite::Result<Vec<Reminder>> {
    let own = Actor::Session(session.clone()).to_string();
    let mono = mono_messages::mono_of_chat(conn, session)?
        .map_or_else(|| own.clone(), |mono| Actor::Mono(mono).to_string());
    conn.prepare(
        "SELECT o.key, o.ask_id, o.stage,
           CASE WHEN a.child_actor <> o.recipient THEN a.child_actor END
         FROM mono_outbox o LEFT JOIN mono_asks a ON a.id = o.ask_id
          WHERE o.recipient IN (?1, ?2) AND o.sent_at IS NULL AND o.claimed_by IS NULL
          ORDER BY o.at_active_ms, o.key",
    )?
    .query_map(params![own, mono], |row| {
        let child: Option<String> = row.get(3)?;
        Ok(Reminder {
            key: row.get(0)?,
            ask_id: row.get(1)?,
            stage: row.get(2)?,
            child: child
                .as_deref()
                .and_then(Actor::parse)
                .map(|child| child.send_to().to_string()),
        })
    })?
    .collect()
}

/// Whether a reminder waits for chat `session`.
pub(crate) fn waits(conn: &Connection, session: &SessionId) -> rusqlite::Result<bool> {
    Ok(!pending(conn, session)?.is_empty())
}

/// Plan R9, one compare-and-set: window `window` takes reminder `key` with
/// the delivery `delivery_for` picks, unless a window holds it or it was
/// sent (`None`). `running` is each session's turn as `ControlHost` held it
/// before the store lock (design 5.3 lock order). An `AfterTurn` reminder
/// stays unclaimed: the turn end names its chat again.
pub(crate) fn claim(
    conn: &Connection,
    key: &str,
    window: &str,
    running: &HashMap<String, String>,
    steerable: bool,
) -> rusqlite::Result<Option<Delivery>> {
    let recipient: Option<String> = conn
        .query_row(
            "SELECT recipient FROM mono_outbox WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(chat) = recipient
        .map(|recipient| chat_of(conn, &recipient))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    let delivery = ask_gate::delivery_for(running.contains_key(chat.as_str()), steerable);
    let holder = (delivery != Delivery::AfterTurn).then_some(window);
    let free = conn.execute(
        "UPDATE mono_outbox SET claimed_by = ?2
          WHERE key = ?1 AND claimed_by IS NULL AND sent_at IS NULL",
        params![key, holder],
    )? == 1;
    Ok(free.then_some(delivery))
}

/// The provider took reminder `key` from window `window`: it is sent, once.
/// False when the window holds it no more (its claim was freed) or it was
/// sent before.
pub(crate) fn sent(conn: &Connection, key: &str, window: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE mono_outbox SET sent_at = ?3
          WHERE key = ?1 AND claimed_by = ?2 AND sent_at IS NULL",
        params![key, window, now],
    )? == 1)
}

/// Window `label` closed: the reminders it claimed and did not send wait
/// again, for the window that next owns their chats.
pub(crate) fn release(conn: &Connection, label: &str) -> rusqlite::Result<Notices> {
    let recipients = conn
        .prepare(
            "UPDATE mono_outbox SET claimed_by = NULL
              WHERE claimed_by = ?1 AND sent_at IS NULL RETURNING recipient",
        )?
        .query_map([label], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    for recipient in recipients {
        name_chat(conn, &mut notices, &recipient)?;
    }
    Ok(notices)
}

#[tauri::command(async)]
pub fn mono_outbox_pending(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<Vec<Reminder>, String> {
    let session = SessionId::parse(&session_id).ok_or("Invalid session id")?;
    pending(&*store.lock_conn()?, &session).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_outbox_claim<R: Runtime>(
    window: WebviewWindow<R>,
    control: State<'_, ControlHost>,
    store: State<'_, SessionStore>,
    key: String,
    steerable: bool,
) -> Result<Option<Delivery>, String> {
    // Design 5.3 lock order: the control lock is read and let go before the
    // store lock.
    let running = control.running_sessions();
    claim(
        &*store.lock_conn()?,
        &key,
        window.label(),
        &running,
        steerable,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_outbox_sent<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    key: String,
) -> Result<bool, String> {
    sent(
        &*store.lock_conn()?,
        &key,
        window.label(),
        mono_messages::now_ms(),
    )
    .map_err(|error| error.to_string())
}
