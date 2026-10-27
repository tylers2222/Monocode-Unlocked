//! The ask and ack ledger (Change R plan v2, R5 and R5c). `mono_asks` holds
//! one row per key, `mono_ask_events` what happened to it. An ask or an ack
//! travels on a `send_message` row written by `submit_in`, so delivery,
//! dedupe by request id and the send gate stay one path. An ask's id is the
//! id of the row that carried it. Its parties are actors (`ask_gate::Actor`).

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};

use crate::ask_gate::{
    self, AckRefused, Actor, AskState, ExtendRefused, ExtendState, Party, State,
};
use crate::ask_scheduler;
use crate::mono_gate::{self, Caller, Input, Refusal, SessionLevel, Speaker};
use crate::mono_messages::{
    self, Live, MessageId, MonoId, MonoMessage, PayloadHash, RequestId, SessionId, Status,
    Submitted, Tool, ToolError,
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
        "ask" => Call::Ask(AskInput::parse(input)),
        "ack" => Call::Ack(AckInput::parse(input)),
        _ => Call::Message(mono_gate::parse(Tool::parse(tool)?, input)),
    })
}

#[derive(Debug)]
pub struct AskInput {
    message: String,
    send_to: String,
    due_minutes: Option<u32>,
}

#[derive(Debug)]
pub struct AckInput {
    ask_id: MessageId,
    message: String,
}

// R6: the values of an ask or ack are parsed here, field by field, so each
// `invalid_input` hint names its field. The web app's `FIELDS` refuses an
// unknown field first; `known_fields` keeps that rule at this boundary too.
// `message` and `send_to` go on to the carrier's `mono_gate::parse`, which
// names them; a value that is not a string reads as empty there.
fn text(raw: &Value, name: &str) -> String {
    raw.get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn due_rule() -> String {
    format!(
        "due_minutes must be a whole number of active minutes, {} to {}. Leave it out for {}.",
        ask_gate::DUE_MINUTES.start(),
        ask_gate::DUE_MINUTES.end(),
        ask_gate::DEFAULT_DUE_MINUTES
    )
}

/// `due_minutes` of an ask or a `sessions.start` (R7); `None` when left out.
pub(crate) fn due_minutes(raw: &Value) -> Result<Option<u32>, String> {
    raw.get("due_minutes")
        .map(|due| {
            due.as_u64()
                .and_then(|due| u32::try_from(due).ok())
                .filter(|due| ask_gate::DUE_MINUTES.contains(due))
                .ok_or_else(due_rule)
        })
        .transpose()
}

impl AskInput {
    pub fn parse(raw: &Value) -> Result<Self, String> {
        mono_gate::known_fields("ask", raw, &["message", "send_to", "due_minutes"])?;
        let due_minutes = due_minutes(raw)?;
        Ok(AskInput {
            message: text(raw, "message"),
            send_to: text(raw, "send_to"),
            due_minutes,
        })
    }
}

impl AckInput {
    pub fn parse(raw: &Value) -> Result<Self, String> {
        mono_gate::known_fields("ack", raw, &["ask_id", "message"])?;
        let ask_id = raw
            .get("ask_id")
            .and_then(Value::as_str)
            .and_then(MessageId::parse)
            .ok_or("ask_id must be the ask_id (m_…) of the <mono_message> that asked you.")?;
        Ok(AckInput {
            ask_id,
            message: text(raw, "message"),
        })
    }
}

#[derive(Debug)]
pub struct ExtendInput {
    ask_id: Option<MessageId>,
}

impl ExtendInput {
    pub fn parse(raw: &Value) -> Result<Self, String> {
        mono_gate::known_fields("extend", raw, &["ask_id"])?;
        let ask_id = raw
            .get("ask_id")
            .map(|id| {
                id.as_str().and_then(MessageId::parse).ok_or(
                    "ask_id must be the ask_id (m_…) of the <mono_reminder>, or left out when you have one open ask.",
                )
            })
            .transpose()?;
        Ok(ExtendInput { ask_id })
    }
}

/// What `messages.extend` answers: the ask and its active time left now.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum Extension {
    #[serde(rename_all = "camelCase")]
    Extended {
        ask_id: MessageId,
        due_in_ms: i64,
    },
    Failed {
        error: ToolError,
    },
}

impl Extension {
    fn failed(code: &'static str, hint: impl Into<String>) -> Self {
        Extension::Failed {
            error: tool_error(code, hint),
        }
    }
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

/// What names an ask for dedupe by request id: its carrier and its
/// `due_minutes`, the default filled in. A repeat with another deadline is
/// another call (R6b review r1 m2).
fn ask_hash(carrier: &Input, due_minutes: Option<u32>) -> PayloadHash {
    mono_gate::action_hash(
        "ask",
        &json!({
            "carrier": carrier.payload_hash().as_str(),
            "due_minutes": due_minutes.unwrap_or(ask_gate::DEFAULT_DUE_MINUTES),
        }),
    )
}

fn tool_error(code: &'static str, hint: impl Into<String>) -> ToolError {
    ToolError {
        code,
        hint: hint.into(),
        message_id: None,
        open_handles: None,
        duplicate: false,
    }
}

fn failed(code: &'static str, hint: impl Into<String>) -> Submitted {
    Submitted::Failed {
        error: tool_error(code, hint),
    }
}

/// A new ask as saved rows decide it: who asks, who is asked, and its
/// `due_active_ms` (`None` for a Giga's ask).
pub(crate) struct NewAsk {
    parent: Party,
    child: Party,
    due: Option<i64>,
}

/// What saved rows decide of an ask from a caller (review r1 N1).
pub(crate) enum Decided {
    Ask(NewAsk),
    /// The caller or the child has no saved row.
    NoRow,
    NotDirectChild,
}

impl NewAsk {
    /// An ask from `caller` to `child` with `due_minutes`. `is_start`: the
    /// ask is `caller`'s start of `child`, so the start row is what
    /// `started_by` would read.
    fn decide(
        conn: &Connection,
        caller: &Caller,
        child: Option<Party>,
        is_start: bool,
        due_minutes: Option<u32>,
    ) -> rusqlite::Result<Decided> {
        let (Some(parent), Some(child)) = (caller_party(conn, caller)?, child) else {
            return Ok(Decided::NoRow);
        };
        let started = match (caller.mono_id(), child.actor()) {
            (Some(mono), Actor::Session(lead)) => started_by(conn, mono, lead)?,
            _ => false,
        };
        let started = is_start || started;
        let decided = ask_gate::decide_ask(&parent, &child, started, due_minutes, active_ms(conn)?);
        Ok(match decided {
            Ok(due) => Decided::Ask(NewAsk { parent, child, due }),
            Err(ask_gate::AskRefused::NotDirectChild) => Decided::NotDirectChild,
        })
    }

    /// The active time left now, `None` for a Giga's ask.
    pub(crate) fn due_in_ms(&self, conn: &Connection) -> rusqlite::Result<Option<i64>> {
        let active = active_ms(conn)?;
        Ok(self.due.map(|due| due - active))
    }

    /// The open ask, keyed by `id`, the row that carries it.
    fn insert(&self, conn: &Connection, id: &MessageId, now: i64) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO mono_asks (id, parent_actor, child_actor, parent_level,
               child_level, state, due_active_ms, next_due_active_ms, created_wall_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8)",
            params![
                id.as_str(),
                self.parent.actor().to_string(),
                self.child.actor().to_string(),
                self.parent.level().rank(),
                self.child.level().rank(),
                self.due,
                self.due.and_then(|due| ask_scheduler::next_due(due, 0)),
                now
            ],
        )?;
        Ok(())
    }
}

/// R7, before the launch: the ask `caller`'s `sessions.start` of `started`
/// carries, when `started` will be the caller's direct child. `lead` is the
/// lead the level check writes on `started`, so it is the level its saved
/// row will have.
pub(crate) fn decide_start(
    conn: &Connection,
    caller: &Caller,
    started: &SessionId,
    lead: Option<SessionId>,
    due_minutes: Option<u32>,
) -> rusqlite::Result<Decided> {
    let level = lead.map_or(SessionLevel::Lead, |lead| SessionLevel::Worker { lead });
    let child = Party::session(level, started.clone());
    NewAsk::decide(conn, caller, Some(child), true, due_minutes)
}

/// R7: the `start_session` row `id` that `caller`'s `sessions.start` of
/// `started` just wrote is also an ask, when `started` is the caller's direct
/// child: a Mono's lead or a lead's worker. A Giga's start, or one whose
/// session MonoCode never saves (so it could never ack), asks nothing.
pub(crate) fn ask_on_start(
    tx: &Connection,
    caller: &Caller,
    started: &SessionId,
    id: &MessageId,
    due_minutes: Option<u32>,
    now: i64,
) -> rusqlite::Result<()> {
    let child = session_party(tx, started)?;
    match NewAsk::decide(tx, caller, child, true, due_minutes)? {
        Decided::Ask(new) => new.insert(tx, id, now),
        Decided::NoRow | Decided::NotDirectChild => Ok(()),
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
    let (carrier, input) =
        match input.and_then(|input| Ok((carrier(&input.send_to, &input.message)?, input))) {
            Ok(input) => input,
            Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
        };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let prior = mono_messages::get_by_request(&tx, &caller.session_id, request_id)?;
    // A start row is an ask too (R7), so this passes an ask sent under an
    // earlier start's request id; `submit_in`'s hash check then refuses it
    // with the same `request_id_reused` (review r1 N2).
    if another_calls(prior.first(), |id| is_ask(&tx, id))? {
        return Ok(reused());
    }
    let mut new = None;
    if prior.is_empty() {
        // A party with no saved row: the send gate below answers it.
        let child = addressed_party(&tx, &input.send_to)?;
        match NewAsk::decide(&tx, caller, child, false, input.due_minutes)? {
            Decided::Ask(decided) => new = Some(decided),
            Decided::NoRow => {}
            Decided::NotDirectChild => {
                let refused = ask_gate::AskRefused::NotDirectChild;
                return Ok(failed(refused.code(), refused.hint()));
            }
        }
    }
    let hash = ask_hash(&carrier, input.due_minutes);
    let mut submitted =
        mono_messages::submit_in(&tx, caller, request_id, carrier, hash, live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &mut submitted
    {
        // The gate let through a party with no saved row (a session the
        // window lists): no child of the caller. Dropping `tx` undoes the row.
        let Some(new) = new else {
            let refused = ask_gate::AskRefused::NotDirectChild;
            return Ok(failed(refused.code(), refused.hint()));
        };
        new.insert(&tx, &messages[0].message.id, now)?;
        messages[0].carries = carries(&tx, &messages[0].message.id)?;
    }
    tx.commit()?;
    Ok(submitted)
}

/// A refusal the caller of `on_ask` reads: its code and its hint.
pub(crate) struct Refused(pub &'static str, pub String);

/// One call on an ask that a `send_message` row carries to the ask's other
/// party: `ack`, or an R14 question, answer or update. `on_ask` runs it.
pub(crate) trait OnAsk {
    /// What `decide` found, for `write`.
    type Decided;
    /// The tool, and the `mono_ask_events.kind` of its event.
    fn kind(&self) -> &'static str;
    fn message(&self) -> &str;
    /// The ask the call is on. `prior` is the first row already under this
    /// request id.
    fn ask_id(
        &self,
        conn: &Connection,
        prior: Option<&MonoMessage>,
        me: Option<&Actor>,
    ) -> rusqlite::Result<Result<MessageId, Refused>>;
    /// Who hears it.
    fn to<'a>(&self, ask: &'a AskState) -> &'a Actor;
    fn no_open_ask(&self) -> Refused;
    /// The refusal for a caller that no saved row names.
    fn not_a_party(&self) -> Refused;
    /// Whether `me` may make the call on the ask, under the write lock.
    fn decide(
        &self,
        conn: &Connection,
        ask_id: &MessageId,
        ask: &AskState,
        me: &Actor,
    ) -> rusqlite::Result<Result<Self::Decided, Refused>>;
    /// What the accepted call changes on the ask; returns its event's seq.
    fn write(
        self,
        tx: &Connection,
        ask_id: &MessageId,
        ask: &AskState,
        me: &Actor,
        decided: Self::Decided,
        active: i64,
    ) -> rusqlite::Result<Result<i64, Refused>>;
}

/// `call` from `caller`, decided before its carrier row, as `ask` is: a
/// call that is not this caller's to make never reaches the send gate. The
/// decision, the row, the write and the event share one `BEGIN IMMEDIATE`,
/// so two calls on one ask cannot both pass a rule. A row the gate refuses
/// is kept with the ask untouched, so a repeat (same request id) answers
/// the first outcome, as it does for every tool.
pub(crate) fn on_ask<C: OnAsk>(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    call: Result<C, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let call = match call {
        Ok(call) => call,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let refused = |Refused(code, hint)| Ok(failed(code, hint));
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let prior = mono_messages::get_by_request(&tx, &caller.session_id, request_id)?;
    let me = caller_party(&tx, caller)?.map(Party::into_actor);
    let ask_id = match call.ask_id(&tx, prior.first(), me.as_ref())? {
        Ok(ask_id) => ask_id,
        Err(refusal) => return refused(refusal),
    };
    let Some(ask) = load(&tx, &ask_id)? else {
        return refused(call.no_open_ask());
    };
    if another_calls(prior.first(), |id| said_with(&tx, &ask_id, call.kind(), id))? {
        return Ok(reused());
    }
    let (carrier, hash) = match named(&call, &ask_id, &ask) {
        Ok(named) => named,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let to = call.to(&ask);
    // A repeat or a retried refusal (a row under this request id) answers
    // its first outcome, and the send gate a caller no saved row names, so
    // neither is decided.
    let mut decided = None;
    let mut listed = None;
    if let (true, Some(me)) = (prior.is_empty(), &me) {
        decided = Some(match call.decide(&tx, &ask_id, &ask, me)? {
            Ok(decided) => decided,
            Err(refusal) => return refused(refusal),
        });
        if let Actor::Session(session) = to {
            listed = Some(mono_messages::with_own_session(&tx, live, session)?);
        }
    }
    let live = Live {
        sessions: listed.as_deref().unwrap_or(live.sessions),
        ..*live
    };
    let kind = call.kind();
    let mut submitted =
        mono_messages::submit_in(&tx, caller, request_id, carrier, hash, &live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &mut submitted
    {
        // The gate let through a caller with no saved row (a session the
        // window lists): no party to any ask. Dropping `tx` undoes the row.
        let (Some(me), Some(decided)) = (me, decided) else {
            return refused(call.not_a_party());
        };
        let active = active_ms(&tx)?;
        let seq = match call.write(&tx, &ask_id, &ask, &me, decided, active)? {
            Ok(seq) => seq,
            Err(refusal) => return refused(refusal),
        };
        let id = &messages[0].message.id;
        tx.execute(
            "INSERT INTO mono_ask_events (ask_id, kind, seq, message_id, at_active_ms, at_wall_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ask_id.as_str(), kind, seq, id.as_str(), active, now],
        )?;
        messages[0].carries = carries(&tx, id)?;
    }
    tx.commit()?;
    Ok(submitted)
}

/// The row that carries `call` on ask `ask_id`, and the hash that names the
/// call for dedupe by request id: its own call, as `ask_hash` makes an ask
/// (R6b review r2 m1).
fn named<C: OnAsk>(
    call: &C,
    ask_id: &MessageId,
    ask: &AskState,
) -> Result<(Input, PayloadHash), String> {
    let carrier = carrier(call.to(ask).send_to(), call.message())?;
    let named = json!({"carrier": carrier.payload_hash().as_str(), "ask_id": ask_id.as_str()});
    let hash = mono_gate::action_hash(call.kind(), &named);
    Ok((carrier, hash))
}

fn ack_refused(reason: AckRefused) -> Refused {
    Refused("ack_refused", reason.hint().into())
}

impl OnAsk for AckInput {
    type Decided = ();

    fn kind(&self) -> &'static str {
        "ack"
    }

    fn message(&self) -> &str {
        &self.message
    }

    fn ask_id(
        &self,
        _: &Connection,
        _: Option<&MonoMessage>,
        _: Option<&Actor>,
    ) -> rusqlite::Result<Result<MessageId, Refused>> {
        Ok(Ok(self.ask_id.clone()))
    }

    fn to<'a>(&self, ask: &'a AskState) -> &'a Actor {
        &ask.parent
    }

    fn no_open_ask(&self) -> Refused {
        ack_refused(AckRefused::NoOpenAsk)
    }

    fn not_a_party(&self) -> Refused {
        ack_refused(AckRefused::NotYours)
    }

    fn decide(
        &self,
        _: &Connection,
        _: &MessageId,
        ask: &AskState,
        me: &Actor,
    ) -> rusqlite::Result<Result<(), Refused>> {
        Ok(ask_gate::decide_ack(ask, me).map_err(ack_refused))
    }

    fn write(
        self,
        tx: &Connection,
        ask_id: &MessageId,
        ask: &AskState,
        me: &Actor,
        (): (),
        _: i64,
    ) -> rusqlite::Result<Result<i64, Refused>> {
        let changed = tx.execute(
            "UPDATE mono_asks SET state = 'acked'
              WHERE id = ?1 AND state = 'open' AND child_actor = ?2",
            params![ask_id.as_str(), me.to_string()],
        )?;
        if changed == 0 {
            // Decided above under the write lock; this guards the invariant.
            let reason = ask_gate::decide_ack(ask, me).err();
            return Ok(Err(ack_refused(reason.unwrap_or(AckRefused::NoOpenAsk))));
        }
        // Spending the key withdraws every unsent outbox row of the ask, a
        // claimed one too: nothing about a spent key goes out.
        tx.execute(
            "DELETE FROM mono_outbox WHERE ask_id = ?1 AND sent_at IS NULL",
            [ask_id.as_str()],
        )?;
        Ok(Ok(1))
    }
}

/// `ack` from `caller` (design R5): spends the key of ask `ask_id`, which
/// must be open and sent to the caller, and tells its parent. Only once the
/// send gate accepts that row does one guarded `UPDATE` spend the key, so
/// of two acks of one ask exactly one gets through.
pub fn ack(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Result<AckInput, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    on_ask(conn, caller, request_id, input, live, now)
}

/// `extend` from `caller` (plan R10): `decide_extend` and its writes in one
/// `BEGIN IMMEDIATE`, so of two extends of one ask exactly one moves the
/// deadline. The scheduler's next stage moves with it, unless a question
/// paused the clock. Without `ask_id` it extends the caller's one open ask.
pub fn extend(
    conn: &Connection,
    caller: &Caller,
    input: Result<ExtendInput, String>,
    now: i64,
) -> rusqlite::Result<Extension> {
    let input = match input {
        Ok(input) => input,
        Err(hint) => return Ok(Extension::failed(Refusal::InvalidInput.as_str(), hint)),
    };
    let refused = |reason: ExtendRefused| Ok(Extension::failed("extend_refused", reason.hint()));
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(me) = caller_party(&tx, caller)?.map(Party::into_actor) else {
        return refused(ExtendRefused::NotYours);
    };
    let ask_id = match input.ask_id {
        Some(id) => Some(id),
        None => only_open_ask(&tx, &me)?,
    };
    let Some(ask_id) = ask_id else {
        return refused(ExtendRefused::NoSingleOpenAsk);
    };
    let Some(ask) = load_extend(&tx, &ask_id)? else {
        return refused(ExtendRefused::NoOpenAsk);
    };
    let due = match ask_gate::decide_extend(&ask, &me) {
        Ok(due) => due,
        Err(reason) => return refused(reason),
    };
    let next_due = ask_scheduler::next_due(due, ask.stage);
    tx.execute(
        "UPDATE mono_asks SET due_active_ms = ?1,
           next_due_active_ms = CASE WHEN paused_at_active_ms IS NULL THEN ?2 END
         WHERE id = ?3",
        params![due, next_due, ask_id.as_str()],
    )?;
    let active = active_ms(&tx)?;
    tx.execute(
        "INSERT INTO mono_ask_events (ask_id, kind, seq, at_active_ms, at_wall_ms)
         VALUES (?1, 'extension', 1, ?2, ?3)",
        params![ask_id.as_str(), active, now],
    )?;
    tx.commit()?;
    Ok(Extension::Extended {
        ask_id,
        due_in_ms: due - active,
    })
}

#[tauri::command(async)]
pub fn mono_ask_extend(
    store: tauri::State<'_, crate::session_store::SessionStore>,
    caller: mono_messages::CallerArg,
    input: Value,
) -> Result<Extension, String> {
    let caller = caller.parse()?;
    let conn = store.lock_conn()?;
    extend(
        &conn,
        &caller,
        ExtendInput::parse(&input),
        mono_messages::now_ms(),
    )
    .map_err(|error| error.to_string())
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

/// Whether row `id` carried event `kind` (an ack, or an R14 question,
/// answer or update) of ask `ask`.
fn said_with(
    conn: &Connection,
    ask: &MessageId,
    kind: &str,
    id: &MessageId,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_ask_events
           WHERE kind = ?2 AND ask_id = ?1 AND message_id = ?3)",
        params![ask.as_str(), kind, id.as_str()],
        |row| row.get(0),
    )
}

/// The ask's state and parties from columns 0 to 2 of `row`.
fn ask_state(row: &Row) -> rusqlite::Result<AskState> {
    Ok(AskState {
        state: match row.get::<_, String>(0)?.as_str() {
            "open" => State::Open,
            "acked" => State::Acked,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        parent: actor(row, 1)?,
        child: actor(row, 2)?,
    })
}

fn load(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<AskState>> {
    conn.query_row(
        "SELECT state, parent_actor, child_actor FROM mono_asks WHERE id = ?1",
        [id.as_str()],
        ask_state,
    )
    .optional()
}

/// Ask `id` as `decide_extend` reads it.
fn load_extend(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<ExtendState>> {
    conn.query_row(
        "SELECT state, parent_actor, child_actor, due_active_ms, stage,
           EXISTS (SELECT 1 FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'extension')
         FROM mono_asks WHERE id = ?1",
        [id.as_str()],
        |row| {
            Ok(ExtendState {
                ask: ask_state(row)?,
                due: row.get(3)?,
                stage: row.get(4)?,
                extended: row.get(5)?,
            })
        },
    )
    .optional()
}

/// The caller's one open ask, `None` when it has none or several.
fn only_open_ask(conn: &Connection, child: &Actor) -> rusqlite::Result<Option<MessageId>> {
    let open: Vec<String> = conn
        .prepare("SELECT id FROM mono_asks WHERE state = 'open' AND child_actor = ?1 LIMIT 2")?
        .query_map([child.to_string()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    match open.as_slice() {
        [id] => Ok(MessageId::parse(id)),
        _ => Ok(None),
    }
}

fn actor(row: &Row, index: usize) -> rusqlite::Result<Actor> {
    Actor::parse(&row.get::<_, String>(index)?).ok_or(rusqlite::Error::InvalidQuery)
}
