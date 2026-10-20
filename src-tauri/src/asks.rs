//! The ask and ack ledger (Change R plan v2, R5). `mono_asks` holds one row
//! per key, `mono_ask_events` what happened to it. An ask or an ack travels
//! on a `send_message` row written by `submit_in`, so delivery, dedupe by
//! request id and the send gate stay one path. An ask's id is the id of the
//! row that carried it.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ask_gate::{self, AckRefused, AskState, Party, State};
use crate::mono_gate::{self, Caller, Input, Level, Refusal, SessionLevel, Speaker};
use crate::mono_messages::{
    self, Live, MessageId, MonoId, RequestId, SessionId, Status, Submitted, Tool, ToolError,
};
use crate::mono_roster::MonoRole;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_asks (
           id                  TEXT PRIMARY KEY,
           parent_session      TEXT NOT NULL,
           child_session       TEXT NOT NULL CHECK (child_session <> parent_session),
           parent_level        INTEGER NOT NULL CHECK (parent_level BETWEEN 1 AND 3),
           child_level         INTEGER NOT NULL CHECK (child_level = parent_level + 1),
           state               TEXT NOT NULL CHECK (state IN ('open', 'acked', 'dropped')),
           due_active_ms       INTEGER CHECK ((due_active_ms IS NULL) = (parent_level = 1)),
           paused_at_active_ms INTEGER,
           stage               INTEGER NOT NULL DEFAULT 0 CHECK (stage BETWEEN 0 AND 3),
           next_due_active_ms  INTEGER,
           created_wall_ms     INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS mono_asks_next_due
           ON mono_asks (next_due_active_ms) WHERE state = 'open';
         CREATE INDEX IF NOT EXISTS mono_asks_child
           ON mono_asks (child_session) WHERE state = 'open';
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
         );",
    )
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
    let mut new = None;
    match prior.first() {
        // A repeat answers the first outcome through `submit_in`, but only
        // a repeat of an ask: a plain send under this request id with the
        // same fields has the carrier's hash, and is no ask.
        Some(first) if !matches!(first.status, Status::Rejected { .. }) => {
            if !is_ask(&tx, &first.id)? {
                return Ok(failed(
                    Refusal::RequestIdReused.as_str(),
                    mono_gate::REQUEST_ID_REUSED,
                ));
            }
        }
        Some(_) => {}
        None => {
            let parent = caller_party(&tx, caller)?;
            let child = addressed_party(&tx, &input.send_to)?;
            // A party with no saved row: the send gate below answers it.
            if let (Some(parent), Some(child)) = (parent, child) {
                let started = match caller.mono_id() {
                    Some(mono) => started_by(&tx, mono, &child.session)?,
                    None => false,
                };
                let due = input.due_minutes;
                match ask_gate::decide_ask(&parent, &child, started, due, active_ms(&tx)?) {
                    Ok(due) => new = Some((due, parent, child)),
                    Err(refused) => return Ok(failed(refused.code(), refused.hint())),
                }
            }
        }
    }
    let submitted = mono_messages::submit_in(&tx, caller, request_id, carrier, live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &submitted
    {
        // The gate let through a party with no saved row (a session the
        // window lists): no child of the caller. Dropping `tx` undoes the row.
        let Some((due, parent, child)) = new else {
            let refused = ask_gate::AskRefused::NotDirectChild;
            return Ok(failed(refused.code(), refused.hint()));
        };
        tx.execute(
            "INSERT INTO mono_asks (id, parent_session, child_session, parent_level,
               child_level, state, due_active_ms, created_wall_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
            params![
                messages[0].message.id.as_str(),
                parent.session.as_str(),
                child.session.as_str(),
                parent.level.rank(),
                child.level.rank(),
                due,
                now
            ],
        )?;
    }
    tx.commit()?;
    Ok(submitted)
}

/// `ack` from `caller` (design R5): spends the key of ask `ask_id`, which
/// must be open and sent to the caller, and tells its parent. One guarded
/// `UPDATE` decides it, in one `BEGIN IMMEDIATE` with the event and the row
/// to the parent, so of two acks of one ask exactly one gets through, and a
/// row to the parent that is refused leaves the ask open.
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
    let ask = load(&tx, &ask_id)?;
    let me = caller_party(&tx, caller)?.map(|party| party.session);
    // A repeat of the ack (same request id) changes nothing and answers the
    // first outcome through `submit_in`.
    if mono_messages::get_by_request(&tx, &caller.session_id, request_id)?.is_empty() {
        let changed = tx.execute(
            "UPDATE mono_asks SET state = 'acked'
              WHERE id = ?1 AND state = 'open' AND child_session = ?2",
            params![ask_id.as_str(), me.as_ref().map(SessionId::as_str)],
        )?;
        if changed == 0 {
            // `Ok` here would be an ask that turned open between the read and
            // the update, which the write lock rules out.
            let state = ask.as_ref().map(|(state, _)| state);
            let reason = ask_gate::decide_ack(state, me.as_ref()).err();
            return refused(reason.unwrap_or(AckRefused::NoOpenAsk));
        }
    }
    let Some((_, parent)) = &ask else {
        return refused(AckRefused::NoOpenAsk);
    };
    let carrier = match carrier(&parent_address(&tx, parent)?, &input.message) {
        Ok(carrier) => carrier,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let submitted = mono_messages::submit_in(&tx, caller, request_id, carrier, live, now)?;
    match &submitted {
        Submitted::Accepted {
            duplicate: true, ..
        } => {}
        Submitted::Accepted { messages, .. } => {
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
        }
        // Dropping `tx` rolls the ack back with the refused row.
        Submitted::Failed { error } => {
            return Ok(Submitted::Failed {
                error: ToolError {
                    message_id: None,
                    ..error.clone()
                },
            })
        }
    }
    tx.commit()?;
    Ok(submitted)
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
    Ok(mono_messages::mono_row(conn, mono)?.and_then(|row| {
        Some(Party {
            level: match row.role {
                MonoRole::Giga => Level::Giga,
                MonoRole::Mono => Level::Mono,
            },
            session: row.chat?,
        })
    }))
}

fn session_party(conn: &Connection, session: &SessionId) -> rusqlite::Result<Option<Party>> {
    Ok(
        mono_messages::session_level(conn, session)?.map(|level| Party {
            level: match level {
                SessionLevel::Lead => Level::Lead,
                SessionLevel::Worker { lead } => Level::Worker { lead },
            },
            session: session.clone(),
        }),
    )
}

/// The caller as a parent, from saved rows: a Mono speaks through its chat.
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

/// Where an ack to `parent` goes: the Mono whose chat it is, else the session.
fn parent_address(conn: &Connection, parent: &SessionId) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            "SELECT id FROM monos WHERE json_extract(data_json, '$.sessionId') = ?1",
            [parent.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| parent.as_str().to_string()))
}

/// Ask `id` as `ack` reads it, with its parent session.
fn load(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<(AskState, SessionId)>> {
    conn.query_row(
        "SELECT state, child_session, parent_session FROM mono_asks WHERE id = ?1",
        [id.as_str()],
        |row| {
            let session = |index: usize| -> rusqlite::Result<SessionId> {
                let text: String = row.get(index)?;
                SessionId::parse(&text).ok_or(rusqlite::Error::InvalidQuery)
            };
            let state = match row.get::<_, String>(0)?.as_str() {
                "open" => State::Open,
                "acked" => State::Acked,
                _ => State::Dropped,
            };
            Ok((
                AskState {
                    state,
                    child_session: session(1)?,
                },
                session(2)?,
            ))
        },
    )
    .optional()
}
