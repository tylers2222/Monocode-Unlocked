//! The ask and ack ledger (Change R plan v2, R5 and R5c). `mono_asks` holds
//! one row per key, `mono_ask_events` what happened to it. An ask or an ack
//! travels on a `send_message` row written by `submit_in`, so delivery,
//! dedupe by request id and the send gate stay one path. An ask's id is the
//! id of the row that carried it. Its parties are actors (`ask_gate::Actor`).

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ask_gate::{self, AckRefused, Actor, AskState, Party, State};
use crate::ask_scheduler;
use crate::mono_gate::{self, Caller, Input, Refusal, Speaker};
use crate::mono_messages::{
    self, Live, MessageId, MonoId, MonoMessage, RequestId, SessionId, Status, Submitted, Tool,
    ToolError,
};

/// The ask table under `name`, so the R5c rebuild copies into the same
/// shape. A Giga or Mono party (level 1 or 2) is `mono:<id>`, a lead or
/// worker `session:<id>`.
fn asks_sql(name: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {name} (
           id                  TEXT PRIMARY KEY,
           parent_actor        TEXT NOT NULL CHECK (parent_actor GLOB
                                 CASE WHEN parent_level <= 2 THEN 'mono:?*' ELSE 'session:?*' END),
           child_actor         TEXT NOT NULL CHECK (child_actor <> parent_actor) CHECK (child_actor GLOB
                                 CASE WHEN child_level <= 2 THEN 'mono:?*' ELSE 'session:?*' END),
           parent_level        INTEGER NOT NULL CHECK (parent_level BETWEEN 1 AND 3),
           child_level         INTEGER NOT NULL CHECK (child_level = parent_level + 1),
           state               TEXT NOT NULL CHECK (state IN ('open', 'acked')),
           due_active_ms       INTEGER CHECK ((due_active_ms IS NULL) = (parent_level = 1)),
           paused_at_active_ms INTEGER,
           stage               INTEGER NOT NULL DEFAULT 0 CHECK (stage BETWEEN 0 AND 3),
           next_due_active_ms  INTEGER,
           created_wall_ms     INTEGER NOT NULL
         );"
    )
}

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    rebuild_with_actors(conn)?;
    conn.execute_batch(&asks_sql("mono_asks"))?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS mono_asks_next_due
           ON mono_asks (next_due_active_ms) WHERE state = 'open';
         CREATE INDEX IF NOT EXISTS mono_asks_child
           ON mono_asks (child_actor) WHERE state = 'open';
         CREATE TABLE IF NOT EXISTS mono_ask_events (
           ask_id       TEXT NOT NULL REFERENCES mono_asks(id),
           kind         TEXT NOT NULL CHECK (kind IN
                          ('ack', 'question', 'answer', 'update', 'extension', 'miss', 'interruption')),
           seq          INTEGER NOT NULL CHECK (seq >= 1),
           blocked      INTEGER NOT NULL DEFAULT 0 CHECK (blocked IN (0, 1)),
           reason       TEXT,
           message_id   TEXT,
           at_active_ms INTEGER NOT NULL,
           at_wall_ms   INTEGER NOT NULL,
           UNIQUE (ask_id, kind, seq),
           CHECK (kind NOT IN ('question', 'answer') OR seq <= 3),
           CHECK (kind NOT IN ('ack', 'update', 'extension', 'miss') OR seq = 1)
         );
         CREATE INDEX IF NOT EXISTS mono_ask_events_ack_message
           ON mono_ask_events (message_id) WHERE kind = 'ack';",
    )
}

/// R5c: a table from before actors names its parties by chat and allows
/// `dropped`. Each Giga or Mono party becomes the Mono its ask's carrier row
/// names (`from_mono_id` for the parent, `to_mono_id` for the child), which
/// a chat reset does not change; a lead or worker keeps its session. Nothing
/// expires, so a `dropped` ask is open again. An ask whose Giga or Mono was
/// deleted since (the carrier names no Mono) can never be acked or reach its
/// asker, so it is left out with its events. Keyed on the old column, so it
/// runs once. Foreign keys are off for the copy, as for `mono_messages`:
/// `DROP TABLE` under them fails once an event names an ask.
fn rebuild_with_actors(conn: &Connection) -> rusqlite::Result<()> {
    let chats: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('mono_asks') WHERE name = 'parent_session')",
        [],
        |row| row.get(0),
    )?;
    if !chats {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let copied = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).and_then(|tx| {
        tx.execute_batch(&format!(
            "DROP TABLE IF EXISTS mono_asks_new;
             {}
             INSERT INTO mono_asks_new (id, parent_actor, child_actor, parent_level, child_level,
               state, due_active_ms, paused_at_active_ms, stage, next_due_active_ms, created_wall_ms)
             SELECT * FROM (SELECT a.id,
               CASE WHEN a.parent_level <= 2 THEN 'mono:' || m.from_mono_id
                    ELSE 'session:' || a.parent_session END AS parent_actor,
               CASE WHEN a.child_level <= 2 THEN 'mono:' || m.to_mono_id
                    ELSE 'session:' || a.child_session END AS child_actor,
               a.parent_level, a.child_level, CASE a.state WHEN 'acked' THEN 'acked' ELSE 'open' END,
               a.due_active_ms, a.paused_at_active_ms, a.stage, a.next_due_active_ms,
               a.created_wall_ms
             FROM mono_asks a LEFT JOIN mono_messages m ON m.id = a.id)
             WHERE parent_actor IS NOT NULL AND child_actor IS NOT NULL;
             DELETE FROM mono_ask_events WHERE ask_id NOT IN (SELECT id FROM mono_asks_new);
             DROP TABLE mono_asks;
             ALTER TABLE mono_asks_new RENAME TO mono_asks;",
            asks_sql("mono_asks_new")
        ))?;
        tx.commit()
    });
    let restored = conn.execute_batch("PRAGMA foreign_keys = ON");
    copied.and(restored)
}

/// What one `mono_message_submit` call runs: a message tool, an ask or an ack.
pub enum Call {
    Message(Result<Input, String>),
    Ask(Result<AskInput, String>),
    Ack(Result<AckInput, String>),
}

impl From<Result<Input, String>> for Call {
    fn from(input: Result<Input, String>) -> Self {
        Call::Message(input)
    }
}

/// The call tool `tool` makes with `input`; `None` for no such tool.
pub fn call(tool: &str, input: &Value) -> Option<Call> {
    Some(match tool {
        "ask" => Call::Ask(fields(input)),
        "ack" => Call::Ack(fields(input)),
        _ => Call::Message(mono_gate::parse(Tool::parse(tool)?, input)),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskInput {
    message: String,
    send_to: String,
    due_minutes: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckInput {
    ask_id: String,
    message: String,
}

fn fields<T: DeserializeOwned>(input: &Value) -> Result<T, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

/// What a `mono_messages` row carries in the ledger (R6), for its reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Carries {
    /// The row is ask `id` (its own id). `due_in_ms` is the active time
    /// left, none for a Giga's ask.
    #[serde(rename_all = "camelCase")]
    Ask { due_in_ms: Option<i64> },
    /// The row is the ack of ask `ask_id`.
    #[serde(rename_all = "camelCase")]
    Ack { ask_id: MessageId },
}

/// What row `id` carries: `mono_asks` by id, else the `ack` event that
/// names the row. One statement, as `targeted()` reads it for every row.
pub fn carries(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<Carries>> {
    let (ask, due_in_ms, ack_of): (bool, Option<i64>, Option<String>) = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_asks WHERE id = ?1),
           (SELECT due_active_ms - ifnull((SELECT active_ms FROM mono_clock WHERE id = 1), 0)
              FROM mono_asks WHERE id = ?1),
           (SELECT ask_id FROM mono_ask_events WHERE kind = 'ack' AND message_id = ?1)",
        [id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if ask {
        return Ok(Some(Carries::Ask { due_in_ms }));
    }
    ack_of
        .map(|ask_id| {
            let ask_id = MessageId::parse(&ask_id).ok_or(rusqlite::Error::InvalidQuery)?;
            Ok(Carries::Ack { ask_id })
        })
        .transpose()
}

/// A `send_message` of `message` to `send_to`: what carries an ask or ack.
fn carrier(send_to: &str, message: &str) -> Result<Input, String> {
    mono_gate::parse(
        Tool::SendMessage,
        &json!({"message": message, "send_to": send_to, "wanting_reply": false, "intent": "new"}),
    )
}

fn failed(code: &'static str, hint: impl Into<String>) -> Submitted {
    Submitted::Failed {
        error: ToolError {
            code,
            hint: hint.into(),
            message_id: None,
            open_handles: None,
            duplicate: false,
        },
    }
}

/// `ask` from `caller` (design R5): a new key on its direct child, with a
/// deadline in active time unless the caller is a Giga. A repeat of the
/// call (same request id) answers what the first one did.
pub fn ask(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Result<AskInput, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let range = ask_gate::DUE_MINUTES;
    let (carrier, input) = match input.and_then(|input| {
        if input.due_minutes.is_some_and(|due| !range.contains(&due)) {
            return Err(format!(
                "due_minutes is {} to {}.",
                range.start(),
                range.end()
            ));
        }
        Ok((carrier(&input.send_to, &input.message)?, input))
    }) {
        Ok(input) => input,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let prior = mono_messages::get_by_request(&tx, &caller.session_id, request_id)?;
    if another_calls(prior.first(), |id| is_ask(&tx, id))? {
        return Ok(reused());
    }
    let mut new = None;
    if prior.is_empty() {
        let parent = caller_party(&tx, caller)?;
        let child = addressed_party(&tx, &input.send_to)?;
        // A party with no saved row: the send gate below answers it.
        if let (Some(parent), Some(child)) = (parent, child) {
            let started = match (caller.mono_id(), child.actor()) {
                (Some(mono), Actor::Session(lead)) => started_by(&tx, mono, lead)?,
                _ => false,
            };
            let due = input.due_minutes;
            match ask_gate::decide_ask(&parent, &child, started, due, active_ms(&tx)?) {
                Ok(due) => new = Some((due, parent, child)),
                Err(refused) => return Ok(failed(refused.code(), refused.hint())),
            }
        }
    }
    let mut submitted = mono_messages::submit_in(&tx, caller, request_id, carrier, live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &mut submitted
    {
        // The gate let through a party with no saved row (a session the
        // window lists): no child of the caller. Dropping `tx` undoes the row.
        let Some((due, parent, child)) = new else {
            let refused = ask_gate::AskRefused::NotDirectChild;
            return Ok(failed(refused.code(), refused.hint()));
        };
        tx.execute(
            "INSERT INTO mono_asks (id, parent_actor, child_actor, parent_level,
               child_level, state, due_active_ms, next_due_active_ms, created_wall_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8)",
            params![
                messages[0].message.id.as_str(),
                parent.actor().to_string(),
                child.actor().to_string(),
                parent.level().rank(),
                child.level().rank(),
                due,
                due.and_then(|due| ask_scheduler::next_due(due, 0)),
                now
            ],
        )?;
        messages[0].carries = carries(&tx, &messages[0].message.id)?;
    }
    tx.commit()?;
    Ok(submitted)
}

/// `ack` from `caller` (design R5): spends the key of ask `ask_id`, which
/// must be open and sent to the caller, and tells its parent. `decide_ack`
/// runs before the row to the parent; only once the send gate accepts that
/// row does one guarded `UPDATE` spend the key, in one `BEGIN IMMEDIATE`
/// with the event, so of two acks of one ask exactly one gets through. A
/// row the gate refuses is kept with the ask untouched, so a repeat (same
/// request id) answers the first outcome, as it does for every tool.
pub fn ack(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Result<AckInput, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let input = match input {
        Ok(input) => input,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let refused = |reason: AckRefused| Ok(failed("ack_refused", reason.hint()));
    let Some(ask_id) = MessageId::parse(&input.ask_id) else {
        return refused(AckRefused::NoOpenAsk);
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(ask) = load(&tx, &ask_id)? else {
        return refused(AckRefused::NoOpenAsk);
    };
    let prior = mono_messages::get_by_request(&tx, &caller.session_id, request_id)?;
    if another_calls(prior.first(), |id| acked_with(&tx, &ask_id, id))? {
        return Ok(reused());
    }
    let carrier = match carrier(ask.parent.send_to(), &input.message) {
        Ok(carrier) => carrier,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let me = caller_party(&tx, caller)?.map(Party::into_actor);
    // Decided before the send, as `ask` is: an ack that is not this caller's
    // to give never reaches the send gate. A repeat or a retried refusal
    // (a row under this request id) answers its first outcome instead.
    if prior.is_empty() {
        if let Some(me) = &me {
            if let Err(reason) = ask_gate::decide_ack(&ask, me) {
                return refused(reason);
            }
        }
    }
    let mut submitted = mono_messages::submit_in(&tx, caller, request_id, carrier, live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &mut submitted
    {
        // The gate let through a caller with no saved row (a session the
        // window lists): no child of any ask. Dropping `tx` undoes the row.
        let Some(me) = me else {
            return refused(AckRefused::NotYours);
        };
        let changed = tx.execute(
            "UPDATE mono_asks SET state = 'acked'
              WHERE id = ?1 AND state = 'open' AND child_actor = ?2",
            params![ask_id.as_str(), me.to_string()],
        )?;
        if changed == 0 {
            // Decided above under the write lock; this guards the invariant.
            let reason = ask_gate::decide_ack(&ask, &me).err();
            return refused(reason.unwrap_or(AckRefused::NoOpenAsk));
        }
        // Spending the key withdraws every unsent outbox row of the ask, a
        // claimed one too: nothing about a spent key goes out.
        tx.execute(
            "DELETE FROM mono_outbox WHERE ask_id = ?1 AND sent_at IS NULL",
            [ask_id.as_str()],
        )?;
        tx.execute(
            "INSERT INTO mono_ask_events (ask_id, kind, seq, message_id, at_active_ms, at_wall_ms)
             VALUES (?1, 'ack', 1, ?2, ?3, ?4)",
            params![
                ask_id.as_str(),
                messages[0].message.id.as_str(),
                active_ms(&tx)?,
                now
            ],
        )?;
        messages[0].carries = carries(&tx, &messages[0].message.id)?;
    }
    tx.commit()?;
    Ok(submitted)
}

fn reused() -> Submitted {
    failed(
        Refusal::RequestIdReused.as_str(),
        mono_gate::REQUEST_ID_REUSED,
    )
}

/// Whether `prior`, the first row already under this call's request id, is
/// another call's: an accepted row that `own` says this call did not write.
/// No row, or a rejected one whose refusal `submit_in` replays, is not.
fn another_calls(
    prior: Option<&MonoMessage>,
    own: impl FnOnce(&MessageId) -> rusqlite::Result<bool>,
) -> rusqlite::Result<bool> {
    match prior {
        Some(row) if !matches!(row.status, Status::Rejected { .. }) => Ok(!own(&row.id)?),
        _ => Ok(false),
    }
}

/// The stored active time (R2), at most one persist interval behind.
fn active_ms(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row("SELECT active_ms FROM mono_clock WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or(0))
}

fn mono_party(conn: &Connection, mono: &MonoId) -> rusqlite::Result<Option<Party>> {
    Ok(mono_messages::mono_row(conn, mono)?.map(|row| Party::mono(row.role, mono.clone())))
}

fn session_party(conn: &Connection, session: &SessionId) -> rusqlite::Result<Option<Party>> {
    Ok(mono_messages::session_level(conn, session)?
        .map(|level| Party::session(level, session.clone())))
}

/// The caller, from saved rows: a Mono is its mono id, whichever chat calls.
fn caller_party(conn: &Connection, caller: &Caller) -> rusqlite::Result<Option<Party>> {
    match &caller.speaker {
        Speaker::Mono { mono_id, .. } => mono_party(conn, mono_id),
        Speaker::Session { .. } => session_party(conn, &caller.session_id),
    }
}

/// What `send_to` names, from saved rows: a Mono first, else a session.
fn addressed_party(conn: &Connection, send_to: &str) -> rusqlite::Result<Option<Party>> {
    if let Some(party) = MonoId::parse(send_to)
        .map(|mono| mono_party(conn, &mono))
        .transpose()?
        .flatten()
    {
        return Ok(Some(party));
    }
    SessionId::parse(send_to).map_or(Ok(None), |session| session_party(conn, &session))
}

/// Whether `mono_messages` row `id` carried an ask.
fn is_ask(conn: &Connection, id: &MessageId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_asks WHERE id = ?1)",
        [id.as_str()],
        |row| row.get(0),
    )
}

/// Whether Mono `mono` started `session`: its accepted `start_session` row.
fn started_by(conn: &Connection, mono: &MonoId, session: &SessionId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_messages
           WHERE tool = 'start_session' AND status <> 'rejected'
             AND from_mono_id = ?1 AND to_session_id = ?2)",
        params![mono.as_str(), session.as_str()],
        |row| row.get(0),
    )
}

/// Whether row `id` carried the ack of ask `ask`.
fn acked_with(conn: &Connection, ask: &MessageId, id: &MessageId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_ask_events
           WHERE kind = 'ack' AND ask_id = ?1 AND message_id = ?2)",
        params![ask.as_str(), id.as_str()],
        |row| row.get(0),
    )
}

fn load(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<AskState>> {
    conn.query_row(
        "SELECT state, parent_actor, child_actor FROM mono_asks WHERE id = ?1",
        [id.as_str()],
        |row| {
            Ok(AskState {
                state: match row.get::<_, String>(0)?.as_str() {
                    "open" => State::Open,
                    "acked" => State::Acked,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                },
                parent: actor(row, 1)?,
                child: actor(row, 2)?,
            })
        },
    )
    .optional()
}

fn actor(row: &Row, index: usize) -> rusqlite::Result<Actor> {
    Actor::parse(&row.get::<_, String>(index)?).ok_or(rusqlite::Error::InvalidQuery)
}
