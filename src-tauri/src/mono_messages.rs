//! Messages between Monos in `monocode.db`, one row per message and part.
//!
//! The CHECKs make every state the domain forbids unwritable, so a writer that
//! forgets a column fails at the write. [`parse_row`] is the one place a row
//! becomes a [`MonoMessage`]; [`insert`] writes only what that type can hold.
//! Session ids are plain text, not foreign keys: sender sessions can be
//! unpersisted, and a Mono reset deletes its chat.

use std::collections::HashMap;
use std::fmt;

use rusqlite::types::Value as Sql;
use rusqlite::{
    params, Connection, OptionalExtension, Row, ToSql, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::approvals::decide;
use crate::asks::{self, Call};
use crate::control::ControlHost;
use crate::mono_gate::{
    self, Access, ActionDecision, ActionFacts, Caller, CallerFacts, CallerKind, CallerMono,
    Closure, Decision, Facts, HandleState, Handled, Input, Level, MessageRef, OpenHandle,
    PairCount, Recipient, Referenced, Refusal, ReplyFacts, Route, SendFacts, SessionLevel, Speaker,
    Target, TargetSession,
};
use crate::mono_roster::{validate_mono_id, MonoRole};
use crate::session_store::{validate_id, SessionStore};

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    rebuild_mono_messages(conn)?;
    conn.execute_batch(&table_sql("mono_messages"))?;
    let pinned: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('mono_messages') WHERE name = 'pin_turn_id')",
        [],
        |row| row.get(0),
    )?;
    if !pinned {
        conn.execute_batch(&format!(
            "ALTER TABLE mono_messages ADD COLUMN {PIN_TURN_ID}"
        ))?;
    }
    conn.execute_batch(INDEXES)?;
    conn.execute_batch(
        "-- A session that was cleared or deleted. The gate trusts this over
         -- the live session list from the web app.
         CREATE TABLE IF NOT EXISTS mono_session_ends (
           session_id TEXT PRIMARY KEY,
           ended_at   INTEGER NOT NULL
         );",
    )
}

/// Rule (b) (review r1 m1): the turn of the Giga's chat that a late answer
/// was accepted for. Only that turn takes it, and only its end drops it.
const PIN_TURN_ID: &str = "pin_turn_id TEXT CHECK (pin_turn_id IS NULL OR
  (length(pin_turn_id) BETWEEN 1 AND 128 AND intent = 'steer' AND reply_to IS NOT NULL))";

/// The table of design 16.2 under `name`. Its foreign keys always name
/// `mono_messages`, so the rebuild's copy keeps them after the rename.
fn table_sql(name: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {name} (
           id                 TEXT PRIMARY KEY CHECK (length(id) = 14 AND substr(id, 1, 2) = 'm_'),
           request_id         TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
           part               INTEGER NOT NULL DEFAULT 0 CHECK (part BETWEEN 0 AND 9),
           payload_hash       TEXT NOT NULL CHECK (length(payload_hash) = 64),
           tool               TEXT NOT NULL CHECK (tool IN ('send_message', 'reply', 'start_session', 'app_action')),
           from_mono_id       TEXT REFERENCES monos(id) ON DELETE SET NULL,
           from_session_id    TEXT NOT NULL,
           to_mono_id         TEXT REFERENCES monos(id) ON DELETE SET NULL,
           to_session_id      TEXT,
           addressed          TEXT NOT NULL,
           body               TEXT NOT NULL CHECK (length(body) <= 8000),
           body_chars         INTEGER NOT NULL CHECK (body_chars >= 0),
           wanting_reply      INTEGER NOT NULL CHECK (wanting_reply IN (0, 1)),
           intent             TEXT NOT NULL CHECK (intent IN ('new', 'reply', 'steer', 'continue')),
           reply_to           TEXT REFERENCES mono_messages(id),
           scope              TEXT CHECK (scope IN ('turn')),
           scope_turn_id      TEXT CHECK (scope_turn_id IS NULL OR length(scope_turn_id) BETWEEN 1 AND 128),
           hop                INTEGER NOT NULL CHECK (hop >= 0),
           status             TEXT NOT NULL CHECK (status IN
                                ('open', 'answered', 'delivered', 'unanswered', 'dropped', 'rejected')),
           gate_result        TEXT NOT NULL,
           handled_by_session TEXT,
           delivery           TEXT CHECK (delivery IN ('submit', 'queue', 'steer')),
           turn_id            TEXT,
           slot_window        TEXT,
           handle             INTEGER CHECK (handle BETWEEN 1 AND 99),
           delivered_at       INTEGER,
           closed_at          INTEGER,
           notified_at        INTEGER,
           created_at         INTEGER NOT NULL,
           updated_at         INTEGER NOT NULL,
           {PIN_TURN_ID},
           UNIQUE (from_session_id, request_id, part),
           CHECK ((status = 'rejected') = (gate_result <> 'ok')),
           CHECK (status NOT IN ('open', 'unanswered') OR wanting_reply = 1),
           CHECK (status <> 'delivered' OR wanting_reply = 0),
           CHECK (handle IS NULL OR (turn_id IS NOT NULL AND wanting_reply = 1)),
           CHECK (status <> 'rejected' OR (handled_by_session IS NULL AND delivery IS NULL AND turn_id IS NULL)),
           CHECK (to_mono_id IS NULL OR to_session_id IS NULL),
           CHECK (status <> 'answered' OR wanting_reply = 1),
           CHECK (status IN ('rejected', 'dropped') OR (handled_by_session IS NOT NULL AND delivery IS NOT NULL)),
           CHECK (delivered_at IS NULL OR turn_id IS NOT NULL),
           CHECK (turn_id IS NULL OR wanting_reply = 0 OR handle IS NOT NULL),
           CHECK (status <> 'unanswered' OR delivered_at IS NOT NULL),
           CHECK ((slot_window IS NULL) = (turn_id IS NULL)),
           CHECK ((closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))),
           CHECK (status = 'rejected' OR body_chars = length(body)),
           CHECK (tool <> 'send_message' OR length(body) > 0),
           CHECK (notified_at IS NULL OR status IN ('unanswered', 'dropped')),
           CHECK (turn_id IS NULL OR delivery IN ('submit', 'steer')),
           CHECK (status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL),
           CHECK (part = 0 OR (tool = 'reply' AND status <> 'rejected')),
           CHECK (status = 'rejected' OR (reply_to IS NULL) = (hop = 0)),
           CHECK (status = 'rejected' OR (intent = 'new') = (reply_to IS NULL AND scope IS NULL)),
           CHECK (reply_to IS NULL OR scope IS NULL),
           CHECK (scope IS NULL OR tool = 'send_message'),
           CHECK ((scope_turn_id IS NOT NULL) = (scope IS NOT NULL AND status <> 'rejected')),
           CHECK (status = 'rejected' OR intent <> 'continue'),
           CHECK (tool <> 'reply' OR intent = 'reply'),
           CHECK (tool <> 'send_message' OR status = 'rejected' OR intent IN ('new', 'steer')),
           CHECK (tool <> 'app_action' OR status = 'rejected'),
           CHECK (tool <> 'start_session' OR status = 'rejected' OR (intent = 'new' AND wanting_reply = 0 AND to_session_id IS NOT NULL)),
           CHECK (tool IN ('send_message', 'reply') OR intent = 'new'),
           CHECK (intent <> 'steer' OR status = 'rejected' OR delivery IN ('queue', 'steer')),
           CHECK (id GLOB 'm_[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
           CHECK (payload_hash NOT GLOB '*[^0-9a-f]*'),
           CHECK (request_id NOT GLOB '*[^A-Za-z0-9_-]*'),
           CHECK (length(body) = min(body_chars, 8000)),
           CHECK ((gate_result = 'too_large') = (body_chars > 8000))
         );"
    )
}

const INDEXES: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS mono_messages_handle
      ON mono_messages (handled_by_session, turn_id, handle) WHERE handle IS NOT NULL;
    CREATE INDEX IF NOT EXISTS mono_messages_pair
      ON mono_messages (from_mono_id, to_mono_id, to_session_id, created_at)
      WHERE tool = 'send_message' AND status <> 'rejected';
    CREATE INDEX IF NOT EXISTS mono_messages_session_pair
      ON mono_messages (from_session_id, to_mono_id, to_session_id, created_at)
      WHERE from_mono_id IS NULL AND tool = 'send_message' AND status <> 'rejected';
    CREATE INDEX IF NOT EXISTS mono_messages_session
      ON mono_messages (handled_by_session, status) WHERE handled_by_session IS NOT NULL;
    CREATE INDEX IF NOT EXISTS mono_messages_from_session
      ON mono_messages (from_session_id, status) WHERE status IN ('open', 'unanswered');
    CREATE INDEX IF NOT EXISTS mono_messages_waiting
      ON mono_messages (to_mono_id, status) WHERE status IN ('open', 'unanswered');
    CREATE INDEX IF NOT EXISTS mono_messages_notice
      ON mono_messages (status) WHERE status IN ('unanswered', 'dropped') AND notified_at IS NULL;
    CREATE INDEX IF NOT EXISTS mono_messages_reply_to
      ON mono_messages (reply_to) WHERE reply_to IS NOT NULL;";

/// Design 16.2 and 16.12.3.2 (whole-turn): a table without `scope` gets the
/// current CHECKs, which SQLite cannot change in place. The PR 3 table
/// (`action_type`, `steers`) maps to intents; the PR I1 table copies by
/// name. Neither copy names `scope` or `scope_turn_id`, so both stay NULL.
/// Keyed on the missing `scope` column, so it runs once. Foreign keys are off
/// for the copy (decision 236): `DROP TABLE` under them fails once a row
/// names another, and the pragma is a no-op inside a transaction. A failure
/// rolls back to the old table, and the next open tries again.
fn rebuild_mono_messages(conn: &Connection) -> rusqlite::Result<()> {
    let column = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('mono_messages') WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )
    };
    if !column("id")? || column("scope")? {
        return Ok(());
    }
    let copy = if column("steers")? {
        PR3_COPY
    } else {
        INTENTS_COPY
    };
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let rebuilt = copy_into_new_table(conn, copy);
    let restored = conn.execute_batch("PRAGMA foreign_keys = ON");
    rebuilt.and(restored)
}

/// The PR 3 table into the intents shape (design 16.2).
const PR3_COPY: &str =
    "INSERT INTO mono_messages_new (id, request_id, part, payload_hash, tool, from_mono_id,
       from_session_id, to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
       intent, reply_to, hop, status, gate_result, handled_by_session, delivery, turn_id,
       slot_window, handle, delivered_at, closed_at, notified_at, created_at, updated_at)
     SELECT m.id, m.request_id, m.part, m.payload_hash, m.tool, m.from_mono_id,
       m.from_session_id, m.to_mono_id, m.to_session_id, m.addressed, m.body, m.body_chars,
       m.wanting_reply,
       CASE WHEN m.tool = 'reply' THEN 'reply'
            WHEN m.status = 'rejected' AND m.action_type = 'steer' THEN 'steer'
            WHEN COALESCE(m.steers, m.reply_to) IS NOT NULL THEN 'steer'
            ELSE 'new' END,
       CASE WHEN m.tool = 'reply' THEN m.reply_to ELSE COALESCE(m.steers, m.reply_to) END,
       CASE WHEN m.tool = 'send_message' AND m.status <> 'rejected' AND m.reply_to IS NULL
                 AND m.steers IS NOT NULL
            THEN (SELECT t.hop + 1 FROM mono_messages t WHERE t.id = m.steers)
            ELSE m.hop END,
       m.status, m.gate_result, m.handled_by_session, m.delivery, m.turn_id, m.slot_window,
       m.handle, m.delivered_at, m.closed_at, m.notified_at, m.created_at, m.updated_at
     FROM mono_messages m";

/// The PR I1 table, every column by name.
const INTENTS_COPY: &str =
    "INSERT INTO mono_messages_new (id, request_id, part, payload_hash, tool, from_mono_id,
       from_session_id, to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
       intent, reply_to, hop, status, gate_result, handled_by_session, delivery, turn_id,
       slot_window, handle, delivered_at, closed_at, notified_at, created_at, updated_at)
     SELECT id, request_id, part, payload_hash, tool, from_mono_id,
       from_session_id, to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
       intent, reply_to, hop, status, gate_result, handled_by_session, delivery, turn_id,
       slot_window, handle, delivered_at, closed_at, notified_at, created_at, updated_at
     FROM mono_messages";

fn copy_into_new_table(conn: &Connection, copy: &str) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch("DROP TABLE IF EXISTS mono_messages_new")?;
    tx.execute_batch(&table_sql("mono_messages_new"))?;
    tx.execute_batch(&format!(
        "{copy};
         DROP TABLE mono_messages;
         ALTER TABLE mono_messages_new RENAME TO mono_messages;"
    ))?;
    tx.execute_batch(INDEXES)?;
    let dangling: i64 = tx.query_row(
        "SELECT count(*) FROM pragma_foreign_key_check('mono_messages')",
        [],
        |row| row.get(0),
    )?;
    if dangling > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some(format!(
                "mono_messages rebuild: {dangling} rows name a message that does not exist"
            )),
        ));
    }
    tx.commit()
}

/// A closed set of SQL text values, serialized as that text.
macro_rules! text_enum {
    // `all`: also `ALL`, every variant, so a test can hold each one to a
    // rule (control_cli `app_help_names_every_level_code`).
    (all $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        text_enum!($name { $($variant = $text),+ });
        impl $name {
            #[cfg(test)]
            pub(crate) const ALL: &[Self] = &[$($name::$variant),+];
        }
    };
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
            pub(crate) fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
    };
}

text_enum!(Tool {
    SendMessage = "send_message",
    Reply = "reply",
    // Design 16.2: a level refusal of a `sessions.start` (A3).
    StartSession = "start_session",
    // Design 6.6: a refused app action other than `messages.*`; rejected
    // rows only.
    AppAction = "app_action",
});
text_enum!(
    /// The intent a call asked for. A rejected row keeps it as asked.
    IntentKind {
        New = "new",
        Reply = "reply",
        Steer = "steer",
        Continue = "continue",
    }
);

/// The intent of an accepted row (design 16.1). `continue` never passes the
/// gate, so it has no variant; a target goes with `reply` and `steer` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    New,
    Reply(MessageId),
    Steer(SteerTarget),
}

/// What an accepted steer changes (design 16.12, whole-turn). Neither and
/// both have no form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerTarget {
    /// The work `reply_to` started.
    Message(MessageId),
    /// A steer whose `message` started no turn of its own, pinned to turn
    /// `turn_id`, the turn its session ran when the gate accepted it: rule
    /// (b)'s late answer to the Giga's question (into the Giga's chat), or a
    /// steer through a `start_session` row (into the session it started).
    Pinned { message: MessageId, turn_id: String },
    /// Everything the recipient's session does in turn `turn_id`, the turn
    /// that ran when the gate accepted the steer.
    Turn { turn_id: String },
}

text_enum!(
    /// A steer's `scope` as asked (design 16.12, whole-turn).
    Scope { Turn = "turn" }
);

impl Intent {
    pub fn kind(&self) -> IntentKind {
        match self {
            Intent::New => IntentKind::New,
            Intent::Reply(_) => IntentKind::Reply,
            Intent::Steer(_) => IntentKind::Steer,
        }
    }

    pub fn reply_to(&self) -> Option<&MessageId> {
        match self {
            Intent::New | Intent::Steer(SteerTarget::Turn { .. }) => None,
            Intent::Reply(id)
            | Intent::Steer(SteerTarget::Message(id))
            | Intent::Steer(SteerTarget::Pinned { message: id, .. }) => Some(id),
        }
    }

    pub fn scope(&self) -> Option<Scope> {
        match self {
            Intent::Steer(SteerTarget::Turn { .. }) => Some(Scope::Turn),
            _ => None,
        }
    }

    /// The turn a whole-turn steer is pinned to (`scope_turn_id`).
    fn scope_turn_id(&self) -> Option<&str> {
        match self {
            Intent::Steer(SteerTarget::Turn { turn_id }) => Some(turn_id),
            _ => None,
        }
    }

    /// The turn a pinned steer joins (`pin_turn_id`).
    fn pin_turn_id(&self) -> Option<&str> {
        match self {
            Intent::Steer(SteerTarget::Pinned { turn_id, .. }) => Some(turn_id),
            _ => None,
        }
    }

    /// The five columns of an accepted row; `None` when they disagree.
    fn of_columns(
        kind: IntentKind,
        reply_to: Option<MessageId>,
        scope: Option<Scope>,
        scope_turn_id: Option<String>,
        pin_turn_id: Option<String>,
    ) -> Option<Self> {
        match (kind, reply_to, scope, scope_turn_id, pin_turn_id) {
            (IntentKind::New, None, None, None, None) => Some(Intent::New),
            (IntentKind::Reply, Some(id), None, None, None) => Some(Intent::Reply(id)),
            (IntentKind::Steer, Some(id), None, None, None) => {
                Some(Intent::Steer(SteerTarget::Message(id)))
            }
            (IntentKind::Steer, Some(message), None, None, Some(turn_id)) => {
                Some(Intent::Steer(SteerTarget::Pinned { message, turn_id }))
            }
            (IntentKind::Steer, None, Some(Scope::Turn), Some(turn_id), None) => {
                Some(Intent::Steer(SteerTarget::Turn { turn_id }))
            }
            _ => None,
        }
    }
}

/// Flat as `intent`, `replyTo` and `scope`, the shape a rejected row has
/// too. `scope_turn_id` and `pin_turn_id` stay in Rust.
impl Serialize for Intent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("intent", &self.kind())?;
        map.serialize_entry("replyTo", &self.reply_to())?;
        map.serialize_entry("scope", &self.scope())?;
        map.end()
    }
}
text_enum!(
    /// The route the app took: `queue` until hand-off, then `submit` (the text
    /// opened a turn) or `steer` (into a running turn).
    Delivery {
        Submit = "submit",
        Queue = "queue",
        Steer = "steer",
    }
);
text_enum!(all RejectCode {
    TooLarge = "too_large",
    UnknownTarget = "unknown_target",
    SelfSend = "self_send",
    TargetNoConversation = "target_no_conversation",
    DirectionForbidden = "direction_forbidden",
    UpwardSteerForbidden = "upward_steer_forbidden",
    ReplyUnsupportedTarget = "reply_unsupported_target",
    HabitCannotWait = "habit_cannot_wait",
    UnknownMessage = "unknown_message",
    SteerTargetMismatch = "steer_target_mismatch",
    IntentMismatch = "intent_mismatch",
    IntentNotAllowed = "intent_not_allowed",
    TargetGone = "target_gone",
    CannotSteer = "cannot_steer",
    SteerNeedsTarget = "steer_needs_target",
    SteerScopeNotAllowed = "steer_scope_not_allowed",
    NothingRunning = "nothing_running",
    RateLimited = "rate_limited",
    HopLimit = "hop_limit",
    NoOpenHandle = "no_open_handle",
    UnknownHandle = "unknown_handle",
    HandleClosed = "handle_closed",
    AmbiguousReply = "ambiguous_reply",
    WorkerCannotSpawn = "worker_cannot_spawn",
    WorkerScope = "worker_scope",
    SteerViaLead = "steer_via_lead",
    SessionCannotWait = "session_cannot_wait",
    LeadScope = "lead_scope",
    NotAGiga = "not_a_giga",
});

/// A text value the table stores as is, built only through `parse`.
macro_rules! text_id {
    ($(#[$meta:meta])* $name:ident, $valid:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn parse(text: &str) -> Option<Self> {
                let valid: fn(&str) -> bool = $valid;
                valid(text).then(|| $name(text.to_string()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

text_id!(
    /// `m_` and 12 base32 characters: `^m_[a-z2-7]{12}$`.
    MessageId,
    |text| {
        text.strip_prefix("m_").is_some_and(|tail| {
            tail.len() == 12
                && tail
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
        })
    }
);
text_id!(MonoId, |text| validate_mono_id(text).is_ok());
text_id!(SessionId, |text| validate_id(text, "session").is_ok());
text_id!(
    /// `^[A-Za-z0-9_-]{1,128}$`, the CLI's and `app-mcp`'s request ids.
    RequestId,
    |text| {
        (1..=128).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }
);
text_id!(
    /// sha256 of the canonical call, as 64 lowercase hex digits.
    PayloadHash,
    |text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
);

/// One row per handle a reply answers: parts 0 to 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Part(u8);

impl Part {
    pub const FIRST: Part = Part(0);

    pub fn new(number: i64) -> Option<Self> {
        u8::try_from(number)
            .ok()
            .filter(|number| *number <= 9)
            .map(Part)
    }
}

/// What a row keeps of a message: its first 8000 code points, and its real
/// length, so a `too_large` row still says how long the message was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Body {
    #[serde(rename = "body")]
    text: String,
    #[serde(rename = "bodyChars")]
    chars: u32,
}

impl Body {
    pub const MAX_CHARS: usize = 8000;

    pub fn new(full: &str) -> Self {
        Body {
            text: full.chars().take(Self::MAX_CHARS).collect(),
            chars: u32::try_from(full.chars().count()).unwrap_or(u32::MAX),
        }
    }

    /// The first 8000 code points of `full`, counted as stored: the body of a
    /// `start_session` or `app_action` row (design 13.5). A start prompt can
    /// be far longer, and the CHECK ties a longer length to `too_large`.
    pub fn stored(full: &str) -> Self {
        let text: String = full.chars().take(Self::MAX_CHARS).collect();
        Body {
            chars: u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
            text,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A message's number in one receiving turn, `#1` to `#99`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(u8);

impl Handle {
    pub fn new(number: i64) -> Option<Self> {
        u8::try_from(number)
            .ok()
            .filter(|number| (1..=99).contains(number))
            .map(Handle)
    }

    /// `"#n"` as the agent sees it: n from 1 to 99, no leading zero.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.is_empty()
            || digits.len() > 2
            || digits.starts_with('0')
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Handle::new(digits.parse().ok()?)
    }
}

impl Serialize for Handle {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("#{}", self.0))
    }
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// `wanting_reply` where the status fixes it, serialized as that boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wanting<const B: bool>;

impl<const B: bool> Serialize for Wanting<B> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(B)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Party {
    #[serde(rename_all = "camelCase")]
    Mono { mono_id: MonoId },
    /// A project session; it only receives.
    #[serde(rename_all = "camelCase")]
    Session { session_id: SessionId },
}

/// Where an accepted message went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Routed {
    pub handled_by_session: SessionId,
    pub delivery: Delivery,
}

/// The receiving turn of a message that wants a reply. `window` is the label
/// of the window whose hand-off set the slot; it never leaves Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingSlot {
    pub turn_id: String,
    pub handle: Handle,
    pub delivered_at: Option<i64>,
    #[serde(skip)]
    pub window: String,
}

/// The receiving turn of a message that wants no reply, so has no handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlainSlot {
    pub turn_id: String,
    pub delivered_at: Option<i64>,
    #[serde(skip)]
    pub window: String,
}

/// A [`WaitingSlot`] whose provider accepted the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveredSlot {
    pub turn_id: String,
    pub handle: Handle,
    pub delivered_at: i64,
    #[serde(skip)]
    pub window: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Status {
    /// `reply_to` only when the target resolved to a stored row: the column
    /// is a foreign key. `scope` as asked (design 16.12).
    #[serde(rename_all = "camelCase")]
    Rejected {
        gate_result: RejectCode,
        wanting_reply: bool,
        intent: IntentKind,
        reply_to: Option<MessageId>,
        scope: Option<Scope>,
    },
    #[serde(rename_all = "camelCase")]
    Open {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        intent: Intent,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<WaitingSlot>,
    },
    #[serde(rename_all = "camelCase")]
    Delivered {
        wanting_reply: Wanting<false>,
        #[serde(flatten)]
        intent: Intent,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<PlainSlot>,
    },
    #[serde(rename_all = "camelCase")]
    Answered {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        intent: Intent,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<WaitingSlot>,
        closed_at: i64,
    },
    #[serde(rename_all = "camelCase")]
    Unanswered {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        intent: Intent,
        #[serde(flatten)]
        routed: Routed,
        slot: DeliveredSlot,
        closed_at: i64,
        notified_at: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    Dropped {
        wanting_reply: bool,
        #[serde(flatten)]
        intent: Intent,
        closed_at: i64,
        notified_at: Option<i64>,
    },
}

impl Status {
    /// The intent of an accepted row; `None` on a rejected one.
    pub fn intent(&self) -> Option<&Intent> {
        match self {
            Status::Rejected { .. } => None,
            Status::Open { intent, .. }
            | Status::Delivered { intent, .. }
            | Status::Answered { intent, .. }
            | Status::Unanswered { intent, .. }
            | Status::Dropped { intent, .. } => Some(intent),
        }
    }

    /// The `intent`, `reply_to`, `scope`, `scope_turn_id` and `pin_turn_id`
    /// columns.
    fn intent_columns(&self) -> IntentColumns<'_> {
        match self {
            Status::Rejected {
                intent,
                reply_to,
                scope,
                ..
            } => IntentColumns {
                intent: *intent,
                reply_to: reply_to.as_ref(),
                scope: *scope,
                scope_turn_id: None,
                pin_turn_id: None,
            },
            Status::Open { intent, .. }
            | Status::Delivered { intent, .. }
            | Status::Answered { intent, .. }
            | Status::Unanswered { intent, .. }
            | Status::Dropped { intent, .. } => IntentColumns {
                intent: intent.kind(),
                reply_to: intent.reply_to(),
                scope: intent.scope(),
                scope_turn_id: intent.scope_turn_id(),
                pin_turn_id: intent.pin_turn_id(),
            },
        }
    }
}

struct IntentColumns<'a> {
    intent: IntentKind,
    reply_to: Option<&'a MessageId>,
    scope: Option<Scope>,
    scope_turn_id: Option<&'a str>,
    pin_turn_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonoMessage {
    pub id: MessageId,
    pub request_id: RequestId,
    pub part: Part,
    /// Dedupe is Rust's (design 3.4), so the web app never needs it.
    #[serde(skip)]
    pub payload_hash: PayloadHash,
    pub tool: Tool,
    /// `None` only after the Mono was deleted.
    pub from_mono_id: Option<MonoId>,
    pub from_session_id: SessionId,
    /// `None` for `unknown_target` rows, or after the recipient Mono was deleted.
    pub to: Option<Party>,
    /// What the model asked for, as it gave it.
    pub addressed: String,
    #[serde(flatten)]
    pub body: Body,
    pub hop: u32,
    pub created_at: i64,
    #[serde(flatten)]
    pub status: Status,
}

/// The status columns of one row, as [`Status`] lays them out.
#[derive(Default)]
struct StatusColumns<'a> {
    status: &'static str,
    gate_result: &'static str,
    wanting_reply: bool,
    handled_by_session: Option<&'a str>,
    delivery: Option<&'static str>,
    turn_id: Option<&'a str>,
    slot_window: Option<&'a str>,
    handle: Option<u8>,
    delivered_at: Option<i64>,
    closed_at: Option<i64>,
    notified_at: Option<i64>,
}

impl<'a> StatusColumns<'a> {
    fn routed(mut self, routed: &'a Routed) -> Self {
        self.handled_by_session = Some(routed.handled_by_session.as_str());
        self.delivery = Some(routed.delivery.as_str());
        self
    }

    fn slot(mut self, turn_id: &'a str, window: &'a str, handle: Option<Handle>) -> Self {
        self.turn_id = Some(turn_id);
        self.slot_window = Some(window);
        self.handle = handle.map(|handle| handle.0);
        self
    }

    fn of(status: &'a Status) -> Self {
        let accepted = |status, wanting_reply| StatusColumns {
            status,
            gate_result: "ok",
            wanting_reply,
            ..Default::default()
        };
        match status {
            Status::Rejected {
                gate_result,
                wanting_reply,
                ..
            } => StatusColumns {
                status: "rejected",
                gate_result: gate_result.as_str(),
                wanting_reply: *wanting_reply,
                ..Default::default()
            },
            Status::Open { routed, slot, .. } => {
                let columns = accepted("open", true).routed(routed);
                match slot {
                    Some(slot) => StatusColumns {
                        delivered_at: slot.delivered_at,
                        ..columns.slot(&slot.turn_id, &slot.window, Some(slot.handle))
                    },
                    None => columns,
                }
            }
            Status::Delivered { routed, slot, .. } => {
                let columns = accepted("delivered", false).routed(routed);
                match slot {
                    Some(slot) => StatusColumns {
                        delivered_at: slot.delivered_at,
                        ..columns.slot(&slot.turn_id, &slot.window, None)
                    },
                    None => columns,
                }
            }
            Status::Answered {
                routed,
                slot,
                closed_at,
                ..
            } => {
                let columns = StatusColumns {
                    closed_at: Some(*closed_at),
                    ..accepted("answered", true).routed(routed)
                };
                match slot {
                    Some(slot) => StatusColumns {
                        delivered_at: slot.delivered_at,
                        ..columns.slot(&slot.turn_id, &slot.window, Some(slot.handle))
                    },
                    None => columns,
                }
            }
            Status::Unanswered {
                routed,
                slot,
                closed_at,
                notified_at,
                ..
            } => StatusColumns {
                delivered_at: Some(slot.delivered_at),
                closed_at: Some(*closed_at),
                notified_at: *notified_at,
                ..accepted("unanswered", true).routed(routed).slot(
                    &slot.turn_id,
                    &slot.window,
                    Some(slot.handle),
                )
            },
            Status::Dropped {
                wanting_reply,
                closed_at,
                notified_at,
                ..
            } => StatusColumns {
                closed_at: Some(*closed_at),
                notified_at: *notified_at,
                ..accepted("dropped", *wanting_reply)
            },
        }
    }
}

/// `from_mono_id` goes through `monos` (design 6.6): a refusal written while
/// the caller's row is gone stores NULL, as the Mono's delete leaves its
/// rows, instead of failing the foreign key.
pub fn insert(conn: &Connection, message: &MonoMessage) -> rusqlite::Result<()> {
    let columns = StatusColumns::of(&message.status);
    let intent = message.status.intent_columns();
    let (to_mono_id, to_session_id) = match &message.to {
        Some(Party::Mono { mono_id }) => (Some(mono_id.as_str()), None),
        Some(Party::Session { session_id }) => (None, Some(session_id.as_str())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO mono_messages (
           id, request_id, part, payload_hash, tool, from_mono_id, from_session_id,
           to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
           intent, reply_to, scope, scope_turn_id, hop, status, gate_result, handled_by_session,
           delivery, turn_id, slot_window, handle, delivered_at, closed_at, notified_at,
           created_at, updated_at, pin_turn_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, (SELECT id FROM monos WHERE id = ?6), ?7, ?8, ?9,
                   ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                   ?24, ?25, ?26, ?27, ?28, ?29, ?29, ?30)",
        params![
            message.id.as_str(),
            message.request_id.as_str(),
            message.part.0,
            message.payload_hash.as_str(),
            message.tool.as_str(),
            message.from_mono_id.as_ref().map(MonoId::as_str),
            message.from_session_id.as_str(),
            to_mono_id,
            to_session_id,
            message.addressed,
            message.body.text,
            message.body.chars,
            columns.wanting_reply,
            intent.intent.as_str(),
            intent.reply_to.map(MessageId::as_str),
            intent.scope.map(Scope::as_str),
            intent.scope_turn_id,
            message.hop,
            columns.status,
            columns.gate_result,
            columns.handled_by_session,
            columns.delivery,
            columns.turn_id,
            columns.slot_window,
            columns.handle,
            columns.delivered_at,
            columns.closed_at,
            columns.notified_at,
            message.created_at,
            intent.pin_turn_id,
        ],
    )?;
    Ok(())
}

const SELECT: &str = "SELECT id, request_id, part, payload_hash, tool, from_mono_id,
       from_session_id, to_mono_id, to_session_id, addressed, body, body_chars,
       wanting_reply, intent, reply_to, scope, scope_turn_id, hop, status, gate_result,
       handled_by_session, delivery, turn_id, slot_window, handle, delivered_at,
       closed_at, notified_at, created_at, pin_turn_id
  FROM mono_messages";

pub fn get(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<MonoMessage>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [id.as_str()], parse_row)
        .optional()
}

/// The rows that are messages. A `start_session` row is delivered with no
/// slot for good (design 16.2): it records a start and names its work, so
/// it never waits for a hand-off, takes a slot, or is dropped as waiting.
const MESSAGE_TOOLS: &str = "tool IN ('send_message', 'reply')";

/// Row `id` when it is a message.
fn get_queueable(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<MonoMessage>> {
    conn.query_row(
        &format!("{SELECT} WHERE id = ?1 AND {MESSAGE_TOOLS}"),
        [id.as_str()],
        parse_row,
    )
    .optional()
}

/// Every part one call wrote, in part order.
pub fn get_by_request(
    conn: &Connection,
    from_session_id: &SessionId,
    request_id: &RequestId,
) -> rusqlite::Result<Vec<MonoMessage>> {
    conn.prepare(&format!(
        "{SELECT} WHERE from_session_id = ?1 AND request_id = ?2 ORDER BY part"
    ))?
    .query_map([from_session_id.as_str(), request_id.as_str()], parse_row)?
    .collect()
}

fn invalid(column: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("invalid mono_messages.{column}").into(),
    )
}

fn required<T>(value: Option<T>, column: &str) -> rusqlite::Result<T> {
    value.ok_or_else(|| invalid(column))
}

fn parsed<T>(row: &Row, column: &str, parse: fn(&str) -> Option<T>) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(column)?
        .map(|text| parse(&text).ok_or_else(|| invalid(column)))
        .transpose()
}

#[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
fn parse_row(row: &Row) -> rusqlite::Result<MonoMessage> {
    let text = |column: &str| row.get::<_, Option<String>>(column);
    let wanting_reply: bool = row.get("wanting_reply")?;
    let routed = || -> rusqlite::Result<Routed> {
        Ok(Routed {
            handled_by_session: required(
                parsed(row, "handled_by_session", SessionId::parse)?,
                "handled_by_session",
            )?,
            delivery: required(
                text("delivery")?.as_deref().and_then(Delivery::parse),
                "delivery",
            )?,
        })
    };
    let turn_id = text("turn_id")?;
    let window = || required(text("slot_window")?, "slot_window");
    let handle = || -> rusqlite::Result<Handle> {
        required(
            row.get::<_, Option<i64>>("handle")?.and_then(Handle::new),
            "handle",
        )
    };
    let delivered_at: Option<i64> = row.get("delivered_at")?;
    let closed_at = || required(row.get::<_, Option<i64>>("closed_at")?, "closed_at");
    let notified_at: Option<i64> = row.get("notified_at")?;
    let waiting_slot = || -> rusqlite::Result<Option<WaitingSlot>> {
        turn_id
            .clone()
            .map(|turn_id| {
                Ok(WaitingSlot {
                    turn_id,
                    handle: handle()?,
                    delivered_at,
                    window: window()?,
                })
            })
            .transpose()
    };
    let intent_kind = required(
        text("intent")?.as_deref().and_then(IntentKind::parse),
        "intent",
    )?;
    let reply_to = parsed(row, "reply_to", MessageId::parse)?;
    let scope = parsed(row, "scope", Scope::parse)?;
    let scope_turn_id = text("scope_turn_id")?;
    let pin_turn_id = text("pin_turn_id")?;
    let intent = || {
        required(
            Intent::of_columns(
                intent_kind,
                reply_to.clone(),
                scope,
                scope_turn_id.clone(),
                pin_turn_id.clone(),
            ),
            "intent",
        )
    };
    let status = match (required(text("status")?, "status")?.as_str(), wanting_reply) {
        ("rejected", _) => Status::Rejected {
            gate_result: required(
                text("gate_result")?.as_deref().and_then(RejectCode::parse),
                "gate_result",
            )?,
            wanting_reply,
            intent: intent_kind,
            reply_to: reply_to.clone(),
            scope,
        },
        ("open", true) => Status::Open {
            wanting_reply: Wanting,
            intent: intent()?,
            routed: routed()?,
            slot: waiting_slot()?,
        },
        ("delivered", false) => Status::Delivered {
            wanting_reply: Wanting,
            intent: intent()?,
            routed: routed()?,
            slot: turn_id
                .clone()
                .map(|turn_id| {
                    Ok::<_, rusqlite::Error>(PlainSlot {
                        turn_id,
                        delivered_at,
                        window: window()?,
                    })
                })
                .transpose()?,
        },
        ("answered", true) => Status::Answered {
            wanting_reply: Wanting,
            intent: intent()?,
            routed: routed()?,
            slot: waiting_slot()?,
            closed_at: closed_at()?,
        },
        ("unanswered", true) => Status::Unanswered {
            wanting_reply: Wanting,
            intent: intent()?,
            routed: routed()?,
            slot: DeliveredSlot {
                turn_id: required(turn_id.clone(), "turn_id")?,
                handle: handle()?,
                delivered_at: required(delivered_at, "delivered_at")?,
                window: window()?,
            },
            closed_at: closed_at()?,
            notified_at,
        },
        ("dropped", _) => Status::Dropped {
            wanting_reply,
            intent: intent()?,
            closed_at: closed_at()?,
            notified_at,
        },
        _ => return Err(invalid("status")),
    };
    let to = match (
        parsed(row, "to_mono_id", MonoId::parse)?,
        parsed(row, "to_session_id", SessionId::parse)?,
    ) {
        (Some(mono_id), None) => Some(Party::Mono { mono_id }),
        (None, Some(session_id)) => Some(Party::Session { session_id }),
        (None, None) => None,
        (Some(_), Some(_)) => return Err(invalid("to_session_id")),
    };
    let tool = required(text("tool")?.as_deref().and_then(Tool::parse), "tool")?;
    // Design 1.4 (levels review r1): an app action is only ever refused, so
    // no accepted variant holds one.
    if tool == Tool::AppAction && !matches!(status, Status::Rejected { .. }) {
        return Err(invalid("tool"));
    }
    Ok(MonoMessage {
        id: required(parsed(row, "id", MessageId::parse)?, "id")?,
        request_id: required(parsed(row, "request_id", RequestId::parse)?, "request_id")?,
        part: required(Part::new(row.get("part")?), "part")?,
        payload_hash: required(
            parsed(row, "payload_hash", PayloadHash::parse)?,
            "payload_hash",
        )?,
        tool,
        from_mono_id: parsed(row, "from_mono_id", MonoId::parse)?,
        from_session_id: required(
            parsed(row, "from_session_id", SessionId::parse)?,
            "from_session_id",
        )?,
        to,
        addressed: row.get("addressed")?,
        // As stored: a `too_large` row keeps the real length of a longer text.
        body: Body {
            text: row.get("body")?,
            chars: row.get("body_chars")?,
        },
        hop: row.get("hop")?,
        created_at: row.get("created_at")?,
        status,
    })
}

/// One session of the web app's live list (design 6): the sessions loaded in
/// the caller's window, and the addressed session when only `sessions` has it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveSession {
    pub session_id: String,
    pub steerable: bool,
    /// A session in one of the caller's projects that is not a Mono chat,
    /// habit run or orchestration session.
    pub own_project: bool,
}

/// The caller as the web app sends it: `kind` `mono_chat` or `habit_run`
/// with `monoId`, or `session` with `access` and `cwd`.
/// [`CallerArg::parse`] refuses any other combination.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallerArg {
    kind: String,
    #[serde(default)]
    mono_id: Option<String>,
    session_id: String,
    turn_id: String,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// What the web app and `ControlHost` know about sessions, read before the
/// store transaction opens (design 5.3 lock order).
pub struct Live<'a> {
    pub sessions: &'a [LiveSession],
    /// Each session with an authorized turn (`ControlHost.active`), with
    /// that turn's id.
    pub running: &'a HashMap<String, String>,
    /// Harness ids whose running turn can be steered (design 6: steerable
    /// depends only on the harness), for sessions not in `sessions`.
    pub steerable_harnesses: &'a [String],
}

impl CallerArg {
    pub(crate) fn parse(self) -> Result<Caller, String> {
        let session_fields = self.access.is_some() || self.cwd.is_some();
        let mono = |kind| -> Result<Speaker, String> {
            if session_fields {
                return Err("A Mono caller has no access or cwd".into());
            }
            Ok(Speaker::Mono {
                kind,
                mono_id: self
                    .mono_id
                    .as_deref()
                    .and_then(MonoId::parse)
                    .ok_or("Invalid caller Mono id")?,
            })
        };
        let speaker = match self.kind.as_str() {
            "mono_chat" => mono(CallerKind::MonoChat)?,
            "habit_run" => mono(CallerKind::HabitRun)?,
            "session" => {
                if self.mono_id.is_some() {
                    return Err("A session caller has no monoId".into());
                }
                Speaker::Session {
                    access: match self.access.as_deref() {
                        Some("operator") => Access::Operator,
                        Some("started") => Access::Started,
                        _ => return Err("Invalid caller access".into()),
                    },
                    cwd: self
                        .cwd
                        .clone()
                        .filter(|cwd| !cwd.is_empty())
                        .ok_or("Invalid caller cwd")?,
                }
            }
            _ => return Err("Invalid caller kind".into()),
        };
        Ok(Caller {
            speaker,
            session_id: SessionId::parse(&self.session_id).ok_or("Invalid caller session id")?,
            turn_id: parse_turn_id(&self.turn_id)?.to_string(),
        })
    }
}

/// A call that failed: a refusal (no row) or a rejection (`message_id` names
/// its row). `open_handles` is present on the reply codes that list them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolError {
    pub code: &'static str,
    pub hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_handles: Option<Vec<OpenHandle>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum Submitted {
    /// The rows of an accepted call. Only a call that is not a duplicate is
    /// routed (design 5.5 rule 1), so a duplicate has no `routes`.
    #[serde(rename_all = "camelCase")]
    Accepted {
        /// Each row with what its line names, so a queue can link its bubble.
        messages: Vec<Targeted>,
        /// The messages this reply answered, with their handles.
        answered: Vec<Answered>,
        duplicate: bool,
        routes: Vec<RouteGroup>,
    },
    Failed {
        error: ToolError,
    },
}

/// A failure that writes no row.
fn refusal(code: Refusal, hint: String) -> ToolError {
    ToolError {
        code: code.as_str(),
        hint,
        message_id: None,
        open_handles: None,
        duplicate: false,
    }
}

fn refused(code: Refusal, hint: String) -> Submitted {
    Submitted::Failed {
        error: refusal(code, hint),
    }
}

const REQUEST_ID_RULE: &str = "A request id has 1 to 128 letters, digits, - and _.";

/// Where a new accept goes: the gate's route, which `delivery` does not hold
/// until a hand-off writes it (design 1.2 review r2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Routing {
    pub session_id: SessionId,
    pub via: Delivery,
}

/// The rows of one accept that go to one session. A reply to several handles
/// has one group per sender session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteGroup {
    #[serde(flatten)]
    pub route: Routing,
    pub message_ids: Vec<MessageId>,
    /// Set when another window owns the session and runs the group (design
    /// 6); the caller window then routes nothing of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_window: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answered {
    pub handle: Option<Handle>,
    pub message_id: MessageId,
}

fn accept_outcome(
    conn: &Connection,
    messages: Vec<MonoMessage>,
    routes: Vec<RouteGroup>,
) -> rusqlite::Result<Submitted> {
    let mut answered = Vec::new();
    for message_id in messages
        .iter()
        .filter_map(|message| match message.status.intent() {
            Some(Intent::Reply(answered)) => Some(answered.clone()),
            _ => None,
        })
    {
        let handle = get(conn, &message_id)?.and_then(|message| match message.status {
            Status::Open { slot, .. } | Status::Answered { slot, .. } => {
                slot.map(|slot| slot.handle)
            }
            Status::Unanswered { slot, .. } => Some(slot.handle),
            _ => None,
        });
        answered.push(Answered { handle, message_id });
    }
    Ok(Submitted::Accepted {
        messages: messages
            .into_iter()
            .map(|message| targeted(conn, message))
            .collect::<rusqlite::Result<_>>()?,
        answered,
        duplicate: routes.is_empty(),
        routes,
    })
}

/// One group per session, in part order.
fn route_groups(messages: &[MonoMessage], routes: Vec<Route>) -> Vec<RouteGroup> {
    let mut groups: Vec<RouteGroup> = Vec::new();
    for (message, route) in messages.iter().zip(routes) {
        match groups
            .iter_mut()
            .find(|group| group.route.session_id == route.session)
        {
            Some(group) => group.message_ids.push(message.id.clone()),
            None => groups.push(RouteGroup {
                route: Routing {
                    session_id: route.session,
                    via: route.via.delivery(),
                },
                message_ids: vec![message.id.clone()],
                owner_window: None,
            }),
        }
    }
    groups
}

/// `m_` and 12 random base32 characters.
pub(crate) fn new_message_id() -> MessageId {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    // Bytes 9 to 15 and the low half of byte 0 are random in a v4 uuid.
    let bits = bytes[9..]
        .iter()
        .fold(u64::from(bytes[0] & 0x0f), |bits, byte| {
            bits << 8 | u64::from(*byte)
        });
    message_id_of(bits)
}

/// R7: the id of the start row of `caller`'s `sessions.start` under
/// `request_id`, a function of the call, so the level check names it on the
/// first prompt before the launch and `record_start` writes the row after.
fn start_message_id(caller: &SessionId, request_id: &RequestId) -> MessageId {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{}\0{}", caller.as_str(), request_id.as_str()));
    message_id_of(u64::from_be_bytes(digest[..8].try_into().expect("8 bytes")))
}

/// `m_` and 12 base32 characters of `bits`.
fn message_id_of(bits: u64) -> MessageId {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let tail: String = (0..12)
        .map(|index| char::from(ALPHABET[((bits >> (5 * index)) & 31) as usize]))
        .collect();
    MessageId::parse(&format!("m_{tail}")).expect("base32 id")
}

/// A Mono's `monos` row (design 16.12, 6.1): `None` when it is gone, else
/// its current chat, its role, the `path_key` of each of its projects and
/// its `messageAllowlist` (missing is empty).
pub(crate) fn mono_row(
    conn: &Connection,
    mono_id: &MonoId,
) -> rusqlite::Result<Option<CallerMono>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT role, data_json FROM monos WHERE id = ?1",
            [mono_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(role, data)| {
        let data = json_of(&data);
        Ok(CallerMono {
            chat: chat_of(&data),
            role: required(MonoRole::parse(&role), "role")?,
            // TS `parseMono` drops an empty path, whose key would be `/`.
            projects: texts(&data, "projects")
                .filter(|path| !path.is_empty())
                .map(mono_gate::path_key)
                .collect(),
            allowlist: texts(&data, "messageAllowlist")
                .filter_map(MonoId::parse)
                .collect(),
        })
    })
    .transpose()
}

/// `monos.data_json`; `null` when it is not JSON.
fn json_of(data: &str) -> Value {
    serde_json::from_str(data).unwrap_or(Value::Null)
}

/// The strings of the array at `key` in `data`; nothing when it is not one.
fn texts<'a>(data: &'a Value, key: &str) -> impl Iterator<Item = &'a str> {
    data.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// `data_json.sessionId`, a Mono's current chat.
fn chat_of(data: &Value) -> Option<SessionId> {
    data.get("sessionId")?.as_str().and_then(SessionId::parse)
}

/// The Mono whose current chat is `chat`.
pub(crate) fn mono_of_chat(
    conn: &Connection,
    chat: &SessionId,
) -> rusqlite::Result<Option<MonoId>> {
    let monos = conn
        .prepare("SELECT id, data_json FROM monos")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(monos
        .into_iter()
        .find(|(_, data)| chat_of(&json_of(data)).as_ref() == Some(chat))
        .and_then(|(id, _)| MonoId::parse(&id)))
}

/// Design 13.1, R4: the level of `session`, only from its stored
/// `lead_session_id`; `None` when `sessions` has no row for it. No copy the
/// web app or an agent sends counts.
pub(crate) fn session_level(
    conn: &Connection,
    session: &SessionId,
) -> rusqlite::Result<Option<SessionLevel>> {
    let stored: Option<Option<String>> = conn
        .query_row(
            "SELECT lead_session_id FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    stored
        .map(|lead| {
            Ok(match lead {
                Some(lead) => SessionLevel::Worker {
                    lead: required(SessionId::parse(&lead), "sessions.lead_session_id")?,
                },
                None => SessionLevel::Lead,
            })
        })
        .transpose()
}

/// Design 7: `session` was reset, deleted or archived.
fn session_ended(conn: &Connection, session: &SessionId) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM mono_session_ends WHERE session_id = ?1",
            [session.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// A1: `prior` is an accepted start row whose session was archived or
/// deleted since.
fn start_ended(conn: &Connection, prior: &MonoMessage) -> rusqlite::Result<bool> {
    match (&prior.status, &prior.to) {
        (Status::Delivered { .. }, Some(Party::Session { session_id }))
            if prior.tool == Tool::StartSession =>
        {
            session_ended(conn, session_id)
        }
        _ => Ok(false),
    }
}

/// Design 7 (PR 6 review r1 m1): a call that a clear of its session
/// overtook. It writes nothing, as the session can never be told.
fn caller_ended() -> ToolError {
    ToolError {
        code: "unknown_token",
        hint: crate::control::UNKNOWN_TOKEN.to_string(),
        message_id: None,
        open_handles: None,
        duplicate: false,
    }
}

fn target(conn: &Connection, session: SessionId, live: &Live) -> rusqlite::Result<Target> {
    let listed = live
        .sessions
        .iter()
        .find(|entry| entry.session_id == session.as_str());
    // The live list is a snapshot from before an await, so a clear that ran
    // since wins over it.
    let ended = session_ended(conn, &session)?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT harness FROM sessions WHERE id = ?1 AND worktree_removed = 0",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let steerable = match (listed, &stored) {
        (Some(entry), _) => entry.steerable,
        (None, Some(harness)) => live.steerable_harnesses.contains(harness),
        (None, None) => false,
    };
    Ok(Target {
        exists: (listed.is_some() || stored.is_some()) && !ended,
        turn: live.running.get(session.as_str()).cloned(),
        steerable,
        session,
    })
}

/// `live` with `session`, an ack's parent, first if no window lists it and
/// `target` finds it, so an idle lead gets it and a gone one does not
/// (R6b review r1 M1, r2 M1).
pub(crate) fn with_own_session(
    conn: &Connection,
    live: &Live,
    session: &SessionId,
) -> rusqlite::Result<Vec<LiveSession>> {
    let listed = live.sessions.to_vec();
    if listed.iter().any(|e| e.session_id == session.as_str()) {
        return Ok(listed);
    }
    let parent = target(conn, session.clone(), live)?;
    let own = parent.exists.then(|| LiveSession {
        session_id: session.as_str().into(),
        steerable: parent.steerable,
        own_project: true,
    });
    Ok(own.into_iter().chain(listed).collect())
}

fn recipient(conn: &Connection, send_to: &str, live: &Live) -> rusqlite::Result<Recipient> {
    if let Some(mono_id) = MonoId::parse(send_to) {
        if let Some(row) = mono_row(conn, &mono_id)? {
            let chat = row
                .chat
                .map(|session| target(conn, session, live))
                .transpose()?;
            return Ok(Recipient::Mono {
                mono_id,
                role: row.role,
                chat,
                projects: row.projects,
                allowlist: row.allowlist,
            });
        }
    }
    if let Some(session) = SessionId::parse(send_to) {
        let listed = live
            .sessions
            .iter()
            .find(|entry| entry.session_id == send_to);
        let own_project = listed.is_some_and(|entry| entry.own_project);
        let target = target(conn, session, live)?;
        if target.exists {
            // R4: no row means no stored lead link. A worker always has one: its
            // lead's `sessions.start` returns only once the worker's row is saved
            // (`acceptQuickLaunch`). So a session the window lists with no row was
            // never sent a message, or MonoCode never saves it: a lead.
            return Ok(Recipient::Session {
                level: session_level(conn, &target.session)?.unwrap_or(SessionLevel::Lead),
                target,
                own_project,
            });
        }
    }
    Ok(Recipient::Unknown)
}

fn closure(message: &MonoMessage) -> Option<Closure> {
    match message.status {
        Status::Answered { .. } => Some(Closure::Answered),
        Status::Unanswered { .. } => Some(Closure::Unanswered),
        Status::Dropped { .. } => Some(Closure::Dropped),
        _ => None,
    }
}

/// The rows of one session with a handle that `filter` selects, by handle,
/// as the reply rows of design 6.4 see them. `state` says how a row stands,
/// given its `closed_at`; `None` leaves the row out.
fn handled(
    conn: &Connection,
    live: &Live,
    filter: &str,
    args: &[&dyn ToSql],
    state: impl Fn(&MonoMessage, Option<i64>) -> Option<HandleState>,
) -> rusqlite::Result<Vec<Handled>> {
    let rows = conn
        .prepare(&format!(
            "{SELECT} WHERE {filter} AND handle IS NOT NULL ORDER BY handle, delivered_at DESC"
        ))?
        .query_map(args, |row| {
            Ok((
                parse_row(row)?,
                row.get::<_, i64>("handle")?,
                row.get::<_, Option<i64>>("closed_at")?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut handled = Vec::with_capacity(rows.len());
    for (message, handle, closed_at) in rows {
        let Some(state) = state(&message, closed_at) else {
            continue;
        };
        let from = match &message.from_mono_id {
            Some(mono_id) => mono_name(conn, mono_id)?,
            None => String::new(),
        };
        handled.push(Handled {
            handle: required(Handle::new(handle), "handle")?,
            state,
            from,
            sender: target(conn, message.from_session_id.clone(), live)?,
            message,
        });
    }
    Ok(handled)
}

/// The handles of the caller's turn (design 5.2): open once the provider
/// took the text, until a reply or the turn end closes them.
fn handled_in_turn(
    conn: &Connection,
    caller: &Caller,
    live: &Live,
) -> rusqlite::Result<Vec<Handled>> {
    handled(
        conn,
        live,
        "handled_by_session = ?1 AND turn_id = ?2",
        params![caller.session_id.as_str(), caller.turn_id],
        |message, _| match &message.status {
            Status::Open {
                slot: Some(slot), ..
            } => Some(if slot.delivered_at.is_some() {
                HandleState::Open
            } else {
                HandleState::Unseen
            }),
            _ => closure(message).map(HandleState::Closed),
        },
    )
}

/// The caller's handles as they stood at `at` (PR 4a review m3): taken by
/// then, and open unless closed by then.
fn handled_at(
    conn: &Connection,
    caller: &Caller,
    at: i64,
    live: &Live,
) -> rusqlite::Result<Vec<Handled>> {
    // Handles restart at #1 in each turn, so a replay in a later turn lists
    // none of the first call's numbers: they name other messages now (PR 5
    // review m5).
    handled(
        conn,
        live,
        "handled_by_session = ?1 AND turn_id = ?3 AND delivered_at <= ?2",
        params![caller.session_id.as_str(), at, caller.turn_id],
        |message, closed_at| match closed_at {
            Some(closed_at) if closed_at <= at => closure(message).map(HandleState::Closed),
            _ => Some(HandleState::Open),
        },
    )
}

/// Design 6: load the facts, decide, write the rows, in one `BEGIN
/// IMMEDIATE` transaction, so dedupe cannot race. `live.running` is read
/// from `ControlHost` before this runs: the control lock is never held inside
/// a store transaction (design 5.3).
pub fn submit(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Result<Input, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let input = match input {
        Ok(input) => input,
        Err(hint) => return Ok(refused(Refusal::InvalidInput, hint)),
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let hash = input.payload_hash();
    let submitted = submit_in(&tx, caller, request_id, input, hash, live, now)?;
    tx.commit()?;
    Ok(submitted)
}

/// `submit` inside a transaction the caller holds: an ask or ack (R5)
/// writes its row this way, in its own `BEGIN IMMEDIATE`. `hash` names the
/// call for dedupe by request id: an ask's covers its `due_minutes` too.
pub(crate) fn submit_in(
    tx: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Input,
    hash: PayloadHash,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    // Design 7 (PR 6 review r1 m1): the control host let this call through
    // before the clear forgot its session, and it reaches here after the clear
    // committed. A row written now would never be dropped.
    if session_ended(tx, &caller.session_id)? {
        return Ok(Submitted::Failed {
            error: caller_ended(),
        });
    }
    // Design 5.4 (2): a call bound to the turn shows that its provider runs
    // it, so it took the text that opened it.
    confirm_openers(tx, &caller.session_id, &caller.turn_id, now)?;
    let caller_row = match &caller.speaker {
        Speaker::Mono { mono_id, .. } => mono_row(tx, mono_id)?.map(CallerFacts::Mono),
        Speaker::Session { cwd, .. } => {
            session_level(tx, &caller.session_id)?.map(|level| CallerFacts::Session {
                level,
                cwd: mono_gate::path_key(cwd),
            })
        }
    };
    let prior = get_by_request(tx, &caller.session_id, request_id)?;
    let mut referenced_id = None;
    let decision = match &input {
        Input::Send(send) => {
            let recipient = recipient(tx, &send.send_to, live)?;
            // Without its `monos` row a Mono caller has no level to resolve
            // `reply_to` with; the gate refuses it with `not_a_mono` first.
            let reply_to = match (send.reply_to(), &caller_row) {
                (Some(id), Some(row)) => resolve_ref(tx, id)?
                    .map(|message| {
                        referenced(tx, caller, &row.level(), &recipient, message, live, now)
                    })
                    .transpose()?,
                _ => None,
            };
            referenced_id = reply_to.as_ref().map(|found| found.message.id.clone());
            let pair = pair_count(tx, caller, &recipient, now)?;
            mono_gate::decide_send(
                caller,
                send,
                &hash,
                &Facts {
                    caller: caller_row,
                    prior,
                    about: SendFacts {
                        recipient,
                        reply_to,
                        pair,
                    },
                },
                now,
            )
        }
        Input::Reply(reply) => {
            let then = match prior.first() {
                Some(first) if matches!(first.status, Status::Rejected { .. }) => {
                    handled_at(tx, caller, first.created_at, live)?
                }
                _ => Vec::new(),
            };
            mono_gate::decide_reply(
                caller,
                reply,
                &hash,
                &Facts {
                    caller: caller_row,
                    prior,
                    about: ReplyFacts {
                        handled: handled_in_turn(tx, caller, live)?,
                        then,
                    },
                },
            )
        }
    };
    record(
        tx,
        caller,
        request_id,
        &input,
        hash,
        decision,
        referenced_id,
        now,
    )
}

/// Design 16.6: the accepted row `id` names. A steer's `reply_to` is a
/// message id only (PR I2 review r1).
fn resolve_ref(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<MonoMessage>> {
    Ok(get(conn, id)?.filter(|message| !matches!(message.status, Status::Rejected { .. })))
}

/// Design 6.1 "pair count" (S19): the caller's accepted sends to `recipient`
/// in the rate window. The sender key is the caller's Mono, so its chat and
/// its habit runs share one count, or the session of a session caller,
/// whose rows have no Mono.
fn pair_count(
    conn: &Connection,
    caller: &Caller,
    recipient: &Recipient,
    now: i64,
) -> rusqlite::Result<PairCount> {
    let (to_mono, to_session) = match recipient {
        Recipient::Unknown => return Ok(PairCount::default()),
        Recipient::Mono { mono_id, .. } => (Some(mono_id.as_str()), None),
        Recipient::Session { target, .. } => (None, Some(target.session.as_str())),
    };
    let (from, from_key) = match caller.mono_id() {
        Some(mono_id) => ("from_mono_id = ?1", mono_id.as_str()),
        None => (
            "from_mono_id IS NULL AND from_session_id = ?1",
            caller.session_id.as_str(),
        ),
    };
    conn.query_row(
        &format!(
            "SELECT count(*), min(created_at) FROM mono_messages
              WHERE {from} AND to_mono_id IS ?2 AND to_session_id IS ?3
                AND tool = 'send_message' AND status <> 'rejected' AND created_at > ?4"
        ),
        params![
            from_key,
            to_mono,
            to_session,
            now - mono_gate::RATE_WINDOW_MS
        ],
        |row| {
            Ok(PairCount {
                count: row.get(0)?,
                oldest: row.get(1)?,
            })
        },
    )
}

/// `message` with the session target_session gives for it, the receiver's
/// or rule (b)'s Giga chat, as live sees it.
fn referenced(
    conn: &Connection,
    caller: &Caller,
    level: &Level,
    recipient: &Recipient,
    message: MonoMessage,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Referenced> {
    let session = match mono_gate::target_session(caller, level, recipient, &message, now) {
        TargetSession::Receiver {
            session: Some(session),
            ..
        }
        | TargetSession::Giga {
            chat: Some(session),
            ..
        } => Some(target(conn, session, live)?),
        _ => None,
    };
    Ok(Referenced { message, session })
}

/// A Mono's name; empty when its row is gone.
fn mono_name(conn: &Connection, mono_id: &MonoId) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            "SELECT name FROM monos WHERE id = ?1",
            [mono_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// One end of a message as `get_message` shows it (design 16.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Named {
    Mono { mono_id: MonoId, mono_name: String },
    Session { session_id: SessionId },
}

/// What `get_message` answers (design 16.4). `message_id` is always there:
/// a handle copied from an older turn names this turn's message, and only
/// the id tells the two apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadMessage {
    pub message_id: MessageId,
    pub from: Named,
    pub to: Option<Named>,
    pub intent: IntentKind,
    pub reply_to: Option<MessageId>,
    pub body: String,
    pub wanting_reply: bool,
    pub status: &'static str,
    pub created_at: i64,
    /// Only when the row holds a handle in the caller's current turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<Handle>,
}

/// Design 16.4: the accepted row `named` points at, when the caller is a
/// party to it: a Mono caller through its Mono (its chat and its habit
/// runs), a session caller through its own session. A handle names a row of
/// the caller's session and turn, in any status. A row between two others
/// reads as no row. Writes nothing.
pub fn readable(
    conn: &Connection,
    caller: &Caller,
    named: &MessageRef,
) -> rusqlite::Result<Option<ReadMessage>> {
    let (filter, key) = match named {
        MessageRef::Id(id) => ("id = ?4", Sql::Text(id.as_str().to_string())),
        MessageRef::Handle(handle) => (
            "handled_by_session = ?2 AND turn_id = ?3 AND handle = ?4",
            Sql::Integer(i64::from(handle.0)),
        ),
    };
    // Decision 506: a leg of the other kind of caller would widen what a
    // caller reads, so each kind gets only its own.
    let party = match caller.speaker {
        Speaker::Mono { .. } => "from_mono_id = ?1 OR to_mono_id = ?1",
        Speaker::Session { .. } => "from_session_id = ?2 OR handled_by_session = ?2",
    };
    let found = conn
        .query_row(
            &format!("{SELECT} WHERE {filter} AND status <> 'rejected' AND ({party})"),
            params![
                caller.mono_id().map(MonoId::as_str),
                caller.session_id.as_str(),
                caller.turn_id,
                key
            ],
            |row| {
                let in_turn = row
                    .get::<_, Option<String>>("handled_by_session")?
                    .as_deref()
                    == Some(caller.session_id.as_str())
                    && row.get::<_, Option<String>>("turn_id")?.as_deref()
                        == Some(caller.turn_id.as_str());
                let handle = match row.get::<_, Option<i64>>("handle")? {
                    Some(number) if in_turn => Some(required(Handle::new(number), "handle")?),
                    _ => None,
                };
                Ok((parse_row(row)?, handle))
            },
        )
        .optional()?;
    let Some((message, handle)) = found else {
        return Ok(None);
    };
    let named_mono = |mono_id: &MonoId| -> rusqlite::Result<Named> {
        Ok(Named::Mono {
            mono_id: mono_id.clone(),
            mono_name: mono_name(conn, mono_id)?,
        })
    };
    let from = match &message.from_mono_id {
        Some(mono_id) => named_mono(mono_id)?,
        None => Named::Session {
            session_id: message.from_session_id.clone(),
        },
    };
    let to = match &message.to {
        Some(Party::Mono { mono_id }) => Some(named_mono(mono_id)?),
        Some(Party::Session { session_id }) => Some(Named::Session {
            session_id: session_id.clone(),
        }),
        None => None,
    };
    let IntentColumns {
        intent, reply_to, ..
    } = message.status.intent_columns();
    let columns = StatusColumns::of(&message.status);
    Ok(Some(ReadMessage {
        intent,
        reply_to: reply_to.cloned(),
        status: columns.status,
        wanting_reply: columns.wanting_reply,
        body: message.body.text,
        message_id: message.id,
        from,
        to,
        created_at: message.created_at,
        handle,
    }))
}

/// What `mono_message_get` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum Got {
    Found { message: ReadMessage },
    Failed { error: ToolError },
}

/// `get_message` (design 16.4): `invalid_input`, `unknown_handle` for a
/// handle that names nothing in this turn, and one `unknown_message` for a
/// missing, rejected or unreadable row, so it never tells whether one exists.
pub fn get_message(
    conn: &Connection,
    caller: &Caller,
    input: Result<MessageRef, String>,
) -> rusqlite::Result<Got> {
    let failed = |code: &'static str, hint: String| Got::Failed {
        error: ToolError {
            code,
            hint,
            message_id: None,
            open_handles: None,
            duplicate: false,
        },
    };
    let named = match input {
        Ok(named) => named,
        Err(hint) => return Ok(failed(Refusal::InvalidInput.as_str(), hint)),
    };
    Ok(match (readable(conn, caller, &named)?, named) {
        (Some(message), _) => Got::Found { message },
        (None, MessageRef::Handle(handle)) => failed(
            RejectCode::UnknownHandle.as_str(),
            format!("No handle {handle} in this turn. Pass the message's message_id instead."),
        ),
        (None, MessageRef::Id(_)) => failed(
            RejectCode::UnknownMessage.as_str(),
            "No message you can read has that id.".into(),
        ),
    })
}

/// An accepted row's `delivery` is `queue` until a hand-off writes `submit`
/// or `steer` (design 1.2 review r2); the route goes back in [`Routing`].
fn accepted(wanting_reply: bool, route: &Route, intent: Intent) -> Status {
    let routed = Routed {
        handled_by_session: route.session.clone(),
        delivery: Delivery::Queue,
    };
    if wanting_reply {
        Status::Open {
            wanting_reply: Wanting,
            intent,
            routed,
            slot: None,
        }
    } else {
        Status::Delivered {
            wanting_reply: Wanting,
            intent,
            routed,
            slot: None,
        }
    }
}

/// `reply_to` is the row the call's `reply_to` resolved to, which a
/// rejected row keeps (the column is a foreign key, so only a stored row).
#[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
#[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
fn record(
    tx: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: &Input,
    hash: PayloadHash,
    decision: Decision,
    reply_to: Option<MessageId>,
    now: i64,
) -> rusqlite::Result<Submitted> {
    // A rejected row keeps the intent and scope the call asked for, so a
    // refused steer stays a steer (design 16.2, 16.12).
    let (message, addressed, wanting_reply, asked, scope) = match input {
        Input::Send(send) => (
            &send.message,
            send.send_to.as_str(),
            send.wanting_reply,
            send.intent,
            send.whole_turn().then_some(Scope::Turn),
        ),
        Input::Reply(reply) => (
            &reply.message,
            "",
            reply.wanting_reply,
            IntentKind::Reply,
            None,
        ),
    };
    let row = |part: Part, to: Option<Party>, hop: u32, status| MonoMessage {
        id: new_message_id(),
        request_id: request_id.clone(),
        part,
        payload_hash: hash.clone(),
        tool: input.tool(),
        from_mono_id: caller.mono_id().cloned(),
        from_session_id: caller.session_id.clone(),
        to,
        addressed: addressed.into(),
        body: Body::new(message),
        hop,
        created_at: now,
        status,
    };
    match decision {
        Decision::Duplicate {
            prior,
            rejection: Some(rejection),
        } => Ok(Submitted::Failed {
            error: ToolError {
                code: rejection.code.as_str(),
                hint: rejection.hint,
                message_id: Some(prior[0].id.clone()),
                open_handles: rejection
                    .code
                    .lists_handles()
                    .then_some(rejection.open_handles),
                duplicate: true,
            },
        }),
        Decision::Duplicate {
            prior,
            rejection: None,
        } => accept_outcome(tx, prior, Vec::new()),
        Decision::Refuse { code, hint } => Ok(refused(code, hint)),
        Decision::Reject {
            code,
            hint,
            to,
            hop,
            open_handles,
        } => {
            let message = row(
                Part::FIRST,
                to,
                hop,
                Status::Rejected {
                    gate_result: code,
                    wanting_reply,
                    intent: asked,
                    reply_to,
                    scope,
                },
            );
            insert(tx, &message)?;
            Ok(Submitted::Failed {
                error: ToolError {
                    code: code.as_str(),
                    hint,
                    message_id: Some(message.id),
                    open_handles: code.lists_handles().then_some(open_handles),
                    duplicate: false,
                },
            })
        }
        Decision::Accept { hop, rows } => {
            let mut messages = Vec::with_capacity(rows.len());
            let mut routes = Vec::with_capacity(rows.len());
            for (part, outgoing) in (0..).zip(rows) {
                if let Intent::Reply(answered) = &outgoing.intent {
                    tx.execute(
                        "UPDATE mono_messages SET status = 'answered', closed_at = ?2, updated_at = ?2
                          WHERE id = ?1 AND status = 'open'",
                        params![answered.as_str(), now],
                    )?;
                }
                let message = row(
                    Part::new(part).expect("at most 10 parts"),
                    Some(outgoing.to),
                    hop,
                    accepted(wanting_reply, &outgoing.route, outgoing.intent),
                );
                insert(tx, &message)?;
                messages.push(message);
                routes.push(outgoing.route);
            }
            let groups = route_groups(&messages, routes);
            accept_outcome(tx, messages, groups)
        }
    }
}

/// What an inference line needs of the message a row refers to (design
/// 16.7): its text, and its handle with the session and turn that hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyTarget {
    pub message_id: MessageId,
    pub body: String,
    /// `None` once the chat that handled it was cleared (design 7).
    pub handled_by_session: Option<SessionId>,
    pub slot: Option<TargetSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetSlot {
    pub turn_id: String,
    pub handle: Handle,
}

/// The row `reply_to` names. The column is a foreign key, so it exists.
pub fn reply_target(conn: &Connection, id: &MessageId) -> rusqlite::Result<ReplyTarget> {
    conn.query_row(
        "SELECT body, handled_by_session, turn_id, handle FROM mono_messages WHERE id = ?1",
        [id.as_str()],
        |row| {
            let handle = row
                .get::<_, Option<i64>>("handle")?
                .map(|number| Handle::new(number).ok_or_else(|| invalid("handle")))
                .transpose()?;
            Ok(ReplyTarget {
                message_id: id.clone(),
                body: row.get("body")?,
                handled_by_session: parsed(row, "handled_by_session", SessionId::parse)?,
                slot: row
                    .get::<_, Option<String>>("turn_id")?
                    .zip(handle)
                    .map(|(turn_id, handle)| TargetSlot { turn_id, handle }),
            })
        },
    )
}

/// A row and the message its line names: `None` for `new` and for a
/// whole-turn steer (design 16.7, 16.12.5). A hand-off reads it for the
/// line, a queue for its bubble's link (PR I5). `carries` is the row's ask
/// or ack in the ledger (R6), `None` for any other row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Targeted {
    pub message: MonoMessage,
    pub target: Option<ReplyTarget>,
    pub carries: Option<asks::Carries>,
}

fn targeted(conn: &Connection, message: MonoMessage) -> rusqlite::Result<Targeted> {
    let target = match message.status.intent().and_then(Intent::reply_to) {
        Some(target) => Some(reply_target(conn, target)?),
        None => None,
    };
    let carries = asks::carries(conn, &message.id)?;
    Ok(Targeted {
        message,
        target,
        carries,
    })
}

/// Design 5.5 rule 3: before a hand-off removes a queue item or sends its
/// text, row `id` takes a slot of turn `turn_id`, set by `window`. Same row,
/// same turn: the slot it has. Returns the row with its slot and its target,
/// or `None` when the row is no longer `open` or `delivered`, was already
/// accepted, or holds another turn's slot; the caller then removes the item
/// and delivers nothing (5.1). A steer row takes a slot only as a steer into
/// the turn that runs the work it names; any other hand-off drops it, and
/// its sender is owed a notice (design 16.6).
pub fn assign_slot(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    window: &str,
    delivery: Delivery,
    now: i64,
) -> rusqlite::Result<(Option<Targeted>, Notices)> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(message) = get_queueable(&tx, id)? else {
        return Ok((None, Notices::default()));
    };
    let (routed, slot, wanting_reply) = match &message.status {
        Status::Open { routed, slot, .. } => (
            routed,
            slot.as_ref()
                .map(|slot| (slot.turn_id.as_str(), slot.delivered_at)),
            true,
        ),
        Status::Delivered { routed, slot, .. } => (
            routed,
            slot.as_ref()
                .map(|slot| (slot.turn_id.as_str(), slot.delivered_at)),
            false,
        ),
        _ => return Ok((None, Notices::default())),
    };
    if let (None, Some(Intent::Steer(target))) = (slot, message.status.intent()) {
        // The turn the steer may join: its target's for a targeted steer,
        // its pin for a whole-turn one (design 16.12.3.3) or a late answer.
        let joins: Option<String> = match target {
            SteerTarget::Message(target) => tx.query_row(
                "SELECT turn_id FROM mono_messages WHERE id = ?1",
                [target.as_str()],
                |row| row.get(0),
            )?,
            SteerTarget::Pinned { turn_id, .. } | SteerTarget::Turn { turn_id } => {
                Some(turn_id.clone())
            }
        };
        if delivery != Delivery::Steer || joins.as_deref() != Some(turn_id) {
            let notices = drop_steer(&tx, id, now)?;
            tx.commit()?;
            return Ok((None, notices));
        }
    }
    let assigned = match slot {
        Some((held, delivered_at)) => held == turn_id && delivered_at.is_none(),
        None => {
            // The CHECKs give a row that wants a reply a handle with its slot.
            let handle: Option<i64> = if wanting_reply {
                Some(tx.query_row(
                    "SELECT COALESCE(MAX(handle), 0) + 1 FROM mono_messages
                      WHERE handled_by_session = ?1 AND turn_id = ?2",
                    params![routed.handled_by_session.as_str(), turn_id],
                    |row| row.get(0),
                )?)
            } else {
                None
            };
            tx.execute(
                "UPDATE mono_messages
                    SET turn_id = ?2, slot_window = ?3, handle = ?4, delivery = ?5, updated_at = ?6
                  WHERE id = ?1 AND turn_id IS NULL",
                params![id.as_str(), turn_id, window, handle, delivery.as_str(), now],
            )? == 1
        }
    };
    let assigned = if assigned { get(&tx, id)? } else { None };
    let assigned = assigned.map(|message| targeted(&tx, message)).transpose()?;
    tx.commit()?;
    Ok((assigned, Notices::default()))
}

/// Design 16.6: a steer row that missed the turn it was accepted for is
/// dropped where it stands, so nothing hands it to another turn.
fn drop_steer(tx: &Connection, id: &MessageId, now: i64) -> rusqlite::Result<Notices> {
    tx.execute(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?2, updated_at = ?2
          WHERE id = ?1 AND intent = 'steer' AND status IN ('open', 'delivered')
            AND delivered_at IS NULL",
        params![id.as_str(), now],
    )?;
    let mut notices = Notices::default();
    notices.name_senders(tx, std::slice::from_ref(id))?;
    Ok(notices)
}

/// Design 5.5 rule 4: a hand-off of turn `turn_id` failed, so the row goes
/// back to waiting with no slot. A row the provider accepted keeps its slot.
/// A steer row is dropped instead (design 16.6), and its sender is owed a
/// notice.
pub fn release(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    now: i64,
) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let steer: bool = tx
        .query_row(
            "SELECT intent = 'steer' FROM mono_messages
              WHERE id = ?1 AND turn_id = ?2 AND delivered_at IS NULL",
            params![id.as_str(), turn_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    let notices = if steer {
        drop_steer(&tx, id, now)?
    } else {
        tx.execute(
            "UPDATE mono_messages SET turn_id = NULL, slot_window = NULL, handle = NULL, updated_at = ?3
              WHERE id = ?1 AND turn_id = ?2 AND delivered_at IS NULL",
            params![id.as_str(), turn_id, now],
        )?;
        Notices::default()
    };
    tx.commit()?;
    Ok(notices)
}

/// The provider took the text the hand-off of turn `turn_id` gave it. A row
/// a clear dropped since keeps its `turn_id` (decision 376), so the late
/// confirm skips it (design 16.6, intents review r2 M3).
pub fn mark_delivery(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_messages SET delivered_at = ?3, updated_at = ?3
          WHERE id = ?1 AND turn_id = ?2 AND delivered_at IS NULL
            AND status IN ('open', 'delivered')",
        params![id.as_str(), turn_id, now],
    )?;
    Ok(())
}

/// Design 5.5 rule 6: the rows `session` must still receive that neither a
/// queue item nor a slot holds, oldest first, each with its target so the
/// queue links its bubble as a route does (PR I5).
pub fn unqueued(conn: &Connection, session: &SessionId) -> rusqlite::Result<Vec<Targeted>> {
    let rows: Vec<MonoMessage> = conn
        .prepare(&format!(
            "{SELECT} WHERE handled_by_session = ?1 AND status IN ('open', 'delivered')
               AND delivered_at IS NULL AND turn_id IS NULL AND {MESSAGE_TOOLS}
             ORDER BY created_at, id"
        ))?
        .query_map([session.as_str()], parse_row)?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|message| targeted(conn, message))
        .collect()
}

/// Design 5.4: the provider of turn `turn_id` of `session` runs it, so it
/// took the text that opened it. A steer into the turn is not confirmed: it
/// may still wait for the provider. A row dropped since keeps its slot, so
/// it is skipped (design 16.6, intents review r2 M3).
pub fn confirm_openers(
    conn: &Connection,
    session: &SessionId,
    turn_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_messages SET delivered_at = ?3, updated_at = ?3
          WHERE handled_by_session = ?1 AND turn_id = ?2 AND delivery = 'submit'
            AND delivered_at IS NULL AND status IN ('open', 'delivered')",
        params![session.as_str(), turn_id, now],
    )?;
    Ok(())
}

/// Design 5.3: the one definition of a row whose sender Mono is owed a
/// notice. A row sent from a chat that was reset, deleted or archived since
/// owes none.
const PENDING_NOTICE: &str = "status IN ('unanswered', 'dropped') AND notified_at IS NULL
   AND from_mono_id IS NOT NULL
   AND from_session_id NOT IN (SELECT session_id FROM mono_session_ends)";

/// The sessions one `mono-message-notices` event names (design 5.3), each
/// once. A window reconciles the ones it owns.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Notices {
    sessions: Vec<SessionId>,
    /// A decision row was dropped or requeued: the board reloads once.
    #[serde(skip)]
    pub(crate) approvals: bool,
}

impl Notices {
    pub(crate) fn name(&mut self, session: SessionId) {
        if !self.sessions.contains(&session) {
            self.sessions.push(session);
        }
    }

    pub fn merge(&mut self, other: Notices) {
        self.approvals |= other.approvals;
        for session in other.sessions {
            self.name(session);
        }
    }

    /// Names the current chat of the sender Mono of each of `ids` that now
    /// has a pending notice (design 5.3 review r4): the notice goes to that
    /// chat, never to the session that sent the row.
    fn name_senders(&mut self, conn: &Connection, ids: &[MessageId]) -> rusqlite::Result<()> {
        for id in ids {
            let sender: Option<String> = conn
                .query_row(
                    &format!(
                        "SELECT from_mono_id FROM mono_messages WHERE id = ?1 AND {PENDING_NOTICE}"
                    ),
                    [id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(mono_id) = sender.as_deref().and_then(MonoId::parse) {
                if let Some(chat) = mono_row(conn, &mono_id)?.and_then(|row| row.chat) {
                    self.name(chat);
                }
            }
        }
        Ok(())
    }

    /// To every window; nothing when nothing changed.
    pub fn emit<R: tauri::Runtime>(&self, app: &AppHandle<R>) {
        if !self.sessions.is_empty() {
            let _ = app.emit("mono-message-notices", self);
        }
        if self.approvals {
            let _ = app.emit(crate::approvals::CHANGED, ());
        }
    }
}

/// The turn-end rules of design 5.3 and 16.6 on the rows `scope` selects
/// (`?1` is `now`): a message the provider took that still waits for a reply
/// becomes `unanswered`; a steer it never took is dropped with its slot, since
/// the work it named is over; any other row it never took loses its slot, so
/// reconcile queues it again. Returns the ids of the rows whose sender is now
/// owed a notice, and the rows changed.
fn end_turn_rows(
    tx: &Connection,
    scope: &str,
    args: &[&dyn ToSql],
) -> rusqlite::Result<(Vec<MessageId>, usize)> {
    let ids = |sql: String| -> rusqlite::Result<Vec<MessageId>> {
        tx.prepare(&sql)?
            .query_map(args, |row| row.get::<_, String>(0))?
            .map(|id| id.and_then(|id| required(MessageId::parse(&id), "id")))
            .collect()
    };
    let mut closed = ids(format!(
        "UPDATE mono_messages SET status = 'unanswered', closed_at = ?1, updated_at = ?1
          WHERE {scope} AND status = 'open' AND delivered_at IS NOT NULL
          RETURNING id"
    ))?;
    let released = tx.execute(
        &format!(
            "UPDATE mono_messages SET turn_id = NULL, handle = NULL, slot_window = NULL, updated_at = ?1
              WHERE {scope} AND delivered_at IS NULL AND intent <> 'steer'"
        ),
        args,
    )?;
    closed.extend(ids(format!(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
          WHERE {scope} AND intent = 'steer' AND status IN ('open', 'delivered')
            AND delivered_at IS NULL
          RETURNING id"
    ))?);
    let changed = closed.len() + released;
    Ok((closed, changed))
}

/// Design 5.3: turn `turn_id` of `session` is over. Names `session` when a
/// row changed, so its owner reconciles it, and the chat of each sender now
/// owed a notice. Design 16.6 (PR I2 review r1): a steer accepted for this
/// turn that no hand-off slotted yet, also one in a paused queue, names work
/// that is over, so it is dropped too: a targeted one through its target's
/// turn, a late answer through `pin_turn_id` (review r1 m1) and a whole-turn
/// one through `scope_turn_id` (design 16.12.3.3). Only steers into
/// `session`: a late answer names a row of another session.
pub fn turn_ended(
    conn: &Connection,
    session: &SessionId,
    turn_id: &str,
    now: i64,
) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    // Before end_turn_rows, which frees the slot of a target never taken.
    let waiting = tx
        .prepare(
            "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
              WHERE intent = 'steer' AND status IN ('open', 'delivered') AND turn_id IS NULL
                AND handled_by_session = ?2
                AND (reply_to IN (SELECT id FROM mono_messages
                                   WHERE handled_by_session = ?2 AND turn_id = ?3)
                     OR pin_turn_id = ?3
                     OR (scope = 'turn' AND scope_turn_id = ?3))
              RETURNING id",
        )?
        .query_map(params![now, session.as_str(), turn_id], |row| {
            row.get::<_, String>(0)
        })?
        .map(|id| id.and_then(|id| required(MessageId::parse(&id), "id")))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (unanswered, changed) = end_turn_rows(
        &tx,
        "handled_by_session = ?2 AND turn_id = ?3",
        params![now, session.as_str(), turn_id],
    )?;
    let mut notices = Notices::default();
    // R9: a reminder that waited for this turn's end goes out now.
    if changed + waiting.len() > 0 || crate::ask_scheduler::waits(&tx, session)? {
        notices.name(session.clone());
    }
    notices.name_senders(&tx, &waiting)?;
    notices.name_senders(&tx, &unanswered)?;
    tx.commit()?;
    Ok(notices)
}

/// Design 2 (review r5): window `label` runs no turn any more, so the slots
/// it set that no provider took are free again, and so are the reminders it
/// claimed and did not send (R9). Names their sessions.
pub fn release_window_slots(conn: &Connection, label: &str, now: i64) -> rusqlite::Result<Notices> {
    let sessions = conn
        .prepare(
            "UPDATE mono_messages SET turn_id = NULL, handle = NULL, slot_window = NULL, updated_at = ?2
              WHERE slot_window = ?1 AND delivered_at IS NULL
              RETURNING handled_by_session",
        )?
        .query_map(params![label, now], |row| row.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    for session in sessions
        .iter()
        .flatten()
        .filter_map(|id| SessionId::parse(id))
    {
        notices.name(session);
    }
    notices.merge(crate::ask_scheduler::release(conn, label)?);
    Ok(notices)
}

/// Design 5.3, when `SessionStore` opens: no turn outlives the app, so every
/// slot's turn has ended. A receiver that `sessions` no longer has (or whose
/// worktree was removed) is never loaded again to take its messages, so they
/// are dropped and their senders are owed a notice. No window listens yet,
/// and every window reconciles when it boots, so this names no session.
/// A reminder that a window claimed and did not send is pending again (R8).
pub fn startup(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    end_turn_rows(&tx, "turn_id IS NOT NULL", params![now])?;
    // A steer still waiting for a hand-off names work no turn runs now.
    tx.execute(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
          WHERE intent = 'steer' AND status IN ('open', 'delivered') AND delivered_at IS NULL",
        [now],
    )?;
    let drop_gone = || {
        tx.execute(
            &format!(
                "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
                  WHERE (status = 'open' OR (status = 'delivered' AND delivered_at IS NULL))
                    AND {MESSAGE_TOOLS}
                    AND NOT EXISTS (SELECT 1 FROM sessions
                                     WHERE sessions.id = mono_messages.handled_by_session
                                       AND worktree_removed = 0)"
            ),
            [now],
        )
    };
    drop_gone()?;
    // A decision moved to a chat that is gone too is dropped again there.
    crate::approvals::decide::requeue(&tx, now)?;
    drop_gone()?;
    crate::ask_scheduler::startup(&tx)?;
    tx.commit()
}

/// Design 5.3: the notices the chat `chat` must enqueue: the pending notices
/// of the Mono whose current chat it is, oldest first.
pub fn pending_notices(conn: &Connection, chat: &SessionId) -> rusqlite::Result<Vec<MonoMessage>> {
    let Some(mono_id) = mono_of_chat(conn, chat)? else {
        return Ok(Vec::new());
    };
    conn.prepare(&format!(
        "{SELECT} WHERE from_mono_id = ?1 AND {PENDING_NOTICE} ORDER BY created_at, id"
    ))?
    .query_map([mono_id.as_str()], parse_row)?
    .collect()
}

text_enum!(
    /// The statuses a sender is told about.
    NoticeStatus {
        Unanswered = "unanswered",
        Dropped = "dropped",
    }
);

/// The sender of row `id` was told it is `status`. True when this call marked
/// it; a second call, or a row whose status changed since, marks nothing.
pub fn mark_notified(
    conn: &Connection,
    id: &MessageId,
    status: NoticeStatus,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE mono_messages SET notified_at = ?3, updated_at = ?3
          WHERE id = ?1 AND status = ?2 AND notified_at IS NULL",
        params![id.as_str(), status.as_str(), now],
    )? == 1)
}

/// Design 7: session `session` was reset, deleted or archived. The messages
/// it had still to take are dropped and leave it, and their senders are owed
/// a notice. The messages it sent that still wait for an answer are dropped
/// too, so a late reply gets `handle_closed` instead of landing in a chat
/// without that context (Q8); no one is told, since the sender is the one
/// that ended. `mono_session_ends` keeps the end: the gate treats `session`
/// as gone (design 6), its late calls are refused, and no row it sent owes a
/// notice (5.3). Each update selects on the status it changes, so a second
/// clear changes nothing. It runs inside the transaction that deletes or
/// archives `session`, so a failure there leaves the session receiving (PR 6
/// review r1 M1). Names the chats owed a notice.
pub fn clear_session(
    tx: &Transaction<'_>,
    session: &SessionId,
    now: i64,
) -> rusqlite::Result<Notices> {
    tx.execute(
        "INSERT OR IGNORE INTO mono_session_ends (session_id, ended_at) VALUES (?1, ?2)",
        params![session.as_str(), now],
    )?;
    tx.execute(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?2, notified_at = ?2, updated_at = ?2
          WHERE from_session_id = ?1 AND status IN ('open', 'unanswered')",
        params![session.as_str(), now],
    )?;
    let dropped = tx
        .prepare(&format!(
            "UPDATE mono_messages
                SET status = 'dropped', handled_by_session = NULL, closed_at = ?2, updated_at = ?2
              WHERE handled_by_session = ?1 AND {MESSAGE_TOOLS}
                AND (status = 'open' OR (status = 'delivered' AND delivered_at IS NULL))
              RETURNING id"
        ))?
        .query_map(params![session.as_str(), now], |row| {
            row.get::<_, String>(0)
        })?
        .map(|id| id.and_then(|id| required(MessageId::parse(&id), "id")))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    notices.name_senders(tx, &dropped)?;
    notices.merge(decide::dropped(tx, &dropped)?);
    notices.merge(crate::approvals::decide::requeue(tx, now)?);
    Ok(notices)
}

/// Design 7: an archived session was unarchived, so it can receive again,
/// starting with the decisions its archive dropped.
pub(crate) fn session_reopened(conn: &Connection, session_id: &str) -> rusqlite::Result<Notices> {
    conn.execute(
        "DELETE FROM mono_session_ends WHERE session_id = ?1",
        [session_id],
    )?;
    crate::approvals::decide::requeue(conn, now_ms())
}

/// Design 5.5 rule 8: what the user's Remove of a queued Mono row did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropAnswer {
    /// The row is dropped: remove the item; its sender is owed a notice.
    Dropped,
    /// The row was already closed or delivered: remove the item.
    Closed,
    /// A hand-off holds the row: keep the item.
    InFlight,
}

/// Design 5.5 rule 8 (review r6 order): the user removes the queue item of
/// row `id`. Only a row that still waits for a hand-off is dropped. A row
/// that no longer exists has nothing to deliver, so its item goes as for a
/// closed row.
pub fn drop_message(
    conn: &Connection,
    id: &MessageId,
    now: i64,
) -> rusqlite::Result<(DropAnswer, Notices)> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let slot = get_queueable(&tx, id)?.and_then(|message| match message.status {
        Status::Open { slot, .. } => Some(slot.map(|slot| slot.delivered_at)),
        Status::Delivered { slot, .. } => Some(slot.map(|slot| slot.delivered_at)),
        _ => None,
    });
    let answer = match slot {
        None | Some(Some(Some(_))) => DropAnswer::Closed,
        Some(Some(None)) => DropAnswer::InFlight,
        Some(None) => DropAnswer::Dropped,
    };
    let mut notices = Notices::default();
    if answer == DropAnswer::Dropped {
        // A row with no sender Mono owes no notice. Its `notified_at` marks
        // a drop the user chose, which `requeue` never undoes.
        tx.execute(
            "UPDATE mono_messages SET status = 'dropped', closed_at = ?2, updated_at = ?2,
               notified_at = CASE WHEN from_mono_id IS NULL THEN ?2 END
              WHERE id = ?1",
            params![id.as_str(), now],
        )?;
        notices.name_senders(&tx, std::slice::from_ref(id))?;
        notices.merge(decide::dropped(&tx, std::slice::from_ref(id))?);
    }
    tx.commit()?;
    Ok((answer, notices))
}

/// What `mono_app_action_check` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum ActionChecked {
    /// The action runs. A start writes `lead_session_id` on the new session
    /// (design 13.3), and puts `ask` on its first prompt (R7).
    #[serde(rename_all = "camelCase")]
    Allowed {
        lead_session_id: Option<SessionId>,
        ask: Option<StartAsk>,
    },
    Failed {
        error: ToolError,
    },
}

/// R7: the ask a submitted `sessions.start` gives the session it starts,
/// which the web app puts on its first prompt as R6's `<mono_message>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartAsk {
    pub ask_id: MessageId,
    /// Active time left; `None` for a Giga's ask.
    pub due_in_ms: Option<i64>,
}

/// `input.prompt`, or `''` for an action without one.
fn prompt_of(input: &Value) -> &str {
    input.get("prompt").and_then(Value::as_str).unwrap_or("")
}

const START_ACTION: &str = "sessions.start";

/// What `mono_message_record_start` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum StartRecorded {
    #[serde(rename_all = "camelCase")]
    Recorded {
        message_id: MessageId,
    },
    Failed {
        error: ToolError,
    },
}

/// Design 16.2 and 9: the accepted `start_session` row of session `started`,
/// which a submitted `sessions.start` of `caller` launched, so a steer can
/// name its work. Body: the prompt as given; hash: the one `check_action`
/// writes. R7: the row is also the ask to `started` (`asks::ask_on_start`)
/// with the input's `due_minutes`. A repeat answers its first row and asks
/// nothing more; any other row under the key answers `request_id_reused`.
pub fn record_start(
    conn: &Connection,
    caller: &Caller,
    started: &SessionId,
    request_id: &RequestId,
    input: &Value,
    now: i64,
) -> rusqlite::Result<StartRecorded> {
    let hash = mono_gate::action_hash(START_ACTION, input);
    // `check_action` refused a bad value before the launch.
    let due_minutes = match asks::due_minutes(input) {
        Ok(due) => due,
        Err(hint) => {
            return Ok(StartRecorded::Failed {
                error: refusal(Refusal::InvalidInput, hint),
            })
        }
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    // A clear of the caller ran during the launch.
    if session_ended(&tx, &caller.session_id)? {
        return Ok(StartRecorded::Failed {
            error: caller_ended(),
        });
    }
    // The write lock is held, so no other call takes the key in between.
    let stored = match get_by_request(&tx, &caller.session_id, request_id)?
        .into_iter()
        .next()
    {
        // A refused row under the key names no session (review r1 m1).
        Some(prior) => (prior.tool == Tool::StartSession
            && matches!(prior.status, Status::Delivered { .. }))
        .then_some(prior),
        None => {
            let row = MonoMessage {
                id: start_message_id(&caller.session_id, request_id),
                request_id: request_id.clone(),
                part: Part::FIRST,
                payload_hash: hash.clone(),
                tool: Tool::StartSession,
                from_mono_id: caller.mono_id().cloned(),
                from_session_id: caller.session_id.clone(),
                to: Some(Party::Session {
                    session_id: started.clone(),
                }),
                addressed: START_ACTION.into(),
                body: Body::stored(prompt_of(input)),
                hop: 0,
                created_at: now,
                status: Status::Delivered {
                    wanting_reply: Wanting,
                    intent: Intent::New,
                    routed: Routed {
                        handled_by_session: started.clone(),
                        delivery: Delivery::Submit,
                    },
                    slot: None,
                },
            };
            insert(&tx, &row)?;
            asks::ask_on_start(&tx, caller, started, &row.id, due_minutes, now)?;
            Some(row)
        }
    };
    tx.commit()?;
    Ok(match stored.filter(|stored| stored.payload_hash == hash) {
        Some(stored) => StartRecorded::Recorded {
            message_id: stored.id,
        },
        None => StartRecorded::Failed {
            error: refusal(
                Refusal::RequestIdReused,
                mono_gate::REQUEST_ID_REUSED.into(),
            ),
        },
    })
}

/// R7 and review r1 N3: an allowed `sessions.start` of `started`, with the
/// ask its first prompt carries. `due_minutes` is checked here, after the
/// level gate and the replay of a prior row: out of range, or on a start
/// that asks nothing (a Giga's, or a draft), it is `invalid_input`.
fn start_allowed(
    tx: &Connection,
    caller: &Caller,
    started: &SessionId,
    request_id: &RequestId,
    lead_session_id: Option<SessionId>,
    input: &Value,
) -> rusqlite::Result<ActionChecked> {
    let invalid = |hint: String| ActionChecked::Failed {
        error: refusal(Refusal::InvalidInput, hint),
    };
    let due_minutes = match asks::due_minutes(input) {
        Ok(due) => due,
        Err(hint) => return Ok(invalid(hint)),
    };
    let draft = input.get("draft") == Some(&Value::Bool(true));
    let decided = (!draft)
        .then(|| asks::decide_start(tx, caller, started, lead_session_id.clone(), due_minutes))
        .transpose()?;
    let ask = match decided {
        Some(asks::Decided::Ask(new)) => Some(StartAsk {
            ask_id: start_message_id(&caller.session_id, request_id),
            due_in_ms: new.due_in_ms(tx)?,
        }),
        _ if due_minutes.is_some() => {
            return Ok(invalid(
                "due_minutes is the deadline of a start's ask, and this start asks nothing: a Giga's start and a draft are no ask. Leave due_minutes out.".into(),
            ))
        }
        _ => None,
    };
    Ok(ActionChecked::Allowed {
        lead_session_id,
        ask,
    })
}

/// Design 6.6: the A rows for one app action of `caller`, and the one
/// rejected row they may write, in one `BEGIN IMMEDIATE` transaction, as
/// `submit` does. `target` is the session id the action names; `started` is
/// the id a `sessions.start` launches. Both levels come from stored rows.
#[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
#[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
pub fn check_action(
    conn: &Connection,
    caller: &Caller,
    action: &str,
    target: Option<&str>,
    started: Option<&SessionId>,
    request_id: &RequestId,
    input: &Value,
    now: i64,
) -> rusqlite::Result<ActionChecked> {
    // PR 7 review r1 B1: a target that is no session id is refused. Read as
    // no target, it would skip A5 while the action ran on another reading.
    let target_id = match target.map(|text| SessionId::parse(text).ok_or(text)) {
        None => None,
        Some(Ok(target)) => Some(target),
        Some(Err(text)) => {
            return Ok(ActionChecked::Failed {
                error: refusal(
                    Refusal::InvalidInput,
                    format!("sessionId {text:?} is not a session id."),
                ),
            })
        }
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if session_ended(&tx, &caller.session_id)? {
        return Ok(ActionChecked::Failed {
            error: caller_ended(),
        });
    }
    let prior = get_by_request(&tx, &caller.session_id, request_id)?
        .into_iter()
        .next();
    let prior_start_ended = match (&prior, started) {
        (Some(prior), _) => start_ended(&tx, prior)?,
        // A start with no row (a draft, or a failed record) whose session
        // ended: `host.start` would launch its id again.
        (None, Some(started)) => session_ended(&tx, started)?,
        (None, None) => false,
    };
    let hash = mono_gate::action_hash(action, input);
    let caller_level = match &caller.speaker {
        Speaker::Mono { .. } => None,
        Speaker::Session { .. } => session_level(&tx, &caller.session_id)?,
    };
    let facts = ActionFacts {
        caller_level,
        // R4: no row means no stored lead link. A worker always has one: its
        // lead's `sessions.start` returns only once the worker's row is saved
        // (`acceptQuickLaunch`). So a session the window lists with no row was
        // never sent a message, or MonoCode never saves it: a lead.
        target_level: match &target_id {
            Some(target) => Some(session_level(&tx, target)?.unwrap_or(SessionLevel::Lead)),
            None => None,
        },
        caller_role: match &caller.speaker {
            Speaker::Mono { mono_id, .. } => mono_row(&tx, mono_id)?.map(|row| row.role),
            Speaker::Session { .. } => None,
        },
        prior,
        prior_start_ended,
    };
    let checked = match mono_gate::decide_action(caller, action, &hash, &facts) {
        // `started` is set only for a `sessions.start` (`appLaunchId`).
        ActionDecision::Allow { lead_session_id } => match started {
            Some(started) => {
                start_allowed(&tx, caller, started, request_id, lead_session_id, input)?
            }
            None => ActionChecked::Allowed {
                lead_session_id,
                ask: None,
            },
        },
        ActionDecision::Refuse { code, hint } => ActionChecked::Failed {
            error: refusal(code, hint),
        },
        ActionDecision::Replay { prior, code, hint } => ActionChecked::Failed {
            error: ToolError {
                code: code.as_str(),
                hint,
                message_id: Some(prior),
                open_handles: None,
                duplicate: true,
            },
        },
        ActionDecision::Reject { code, hint, tool } => {
            // Design 13.5: only an A5 row names its target session and keeps
            // the prompt the steer would have carried.
            let steer = code == RejectCode::SteerViaLead;
            let row = MonoMessage {
                id: new_message_id(),
                request_id: request_id.clone(),
                part: Part::FIRST,
                payload_hash: hash,
                tool,
                from_mono_id: caller.mono_id().cloned(),
                from_session_id: caller.session_id.clone(),
                to: target_id
                    .filter(|_| steer)
                    .map(|session_id| Party::Session { session_id }),
                addressed: match target {
                    Some(target) => format!("{action} {target}"),
                    None => action.into(),
                },
                body: Body::stored(if steer || tool == Tool::StartSession {
                    prompt_of(input)
                } else {
                    ""
                }),
                hop: 0,
                created_at: now,
                status: Status::Rejected {
                    gate_result: code,
                    wanting_reply: false,
                    intent: IntentKind::New,
                    reply_to: None,
                    scope: None,
                },
            };
            insert(&tx, &row)?;
            ActionChecked::Failed {
                error: ToolError {
                    code: code.as_str(),
                    hint,
                    message_id: Some(row.id),
                    open_handles: None,
                    duplicate: false,
                },
            }
        }
    };
    tx.commit()?;
    Ok(checked)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn message_id(id: &str) -> Result<MessageId, String> {
    MessageId::parse(id).ok_or_else(|| "Invalid message id".into())
}

/// A turn id from the web app: 1 to 128 characters.
pub(crate) fn parse_turn_id(turn_id: &str) -> Result<&str, String> {
    if (1..=128).contains(&turn_id.len()) {
        Ok(turn_id)
    } else {
        Err("Invalid turn id".into())
    }
}

/// Design 16.12.3.3 (whole-turn review r1): an accepted steer, of either
/// kind, whose route session no longer runs the turn `first` gave when the
/// gate read it. Its turn end may have committed between that read and the
/// insert, and no later turn end would drop it. So this runs `turn_ended`
/// for that turn (it converges: a second run changes nothing the first
/// left), puts the stored row in the answer and takes its route away.
/// When `again` still shows the turn, its removal and turn end come after
/// this commit and drop the row there.
fn drop_steers_of_ended_turns(
    conn: &Connection,
    submitted: &mut Submitted,
    first: &HashMap<String, String>,
    again: &HashMap<String, String>,
    now: i64,
) -> rusqlite::Result<Notices> {
    let mut notices = Notices::default();
    let Submitted::Accepted {
        messages, routes, ..
    } = submitted
    else {
        return Ok(notices);
    };
    let mut ended = Vec::new();
    for Targeted { message, .. } in messages.iter_mut() {
        if !matches!(message.status.intent(), Some(Intent::Steer(_))) {
            continue;
        }
        let Some(group) = routes
            .iter()
            .find(|group| group.message_ids.contains(&message.id))
        else {
            continue;
        };
        let session = group.route.session_id.clone();
        let Some(turn) = first.get(session.as_str()) else {
            continue;
        };
        if again.get(session.as_str()) == Some(turn) {
            continue;
        }
        notices.merge(turn_ended(conn, &session, turn, now)?);
        if let Some(stored) = get(conn, &message.id)? {
            *message = stored;
        }
        ended.push(message.id.clone());
    }
    routes.retain(|group| !group.message_ids.iter().any(|id| ended.contains(id)));
    Ok(notices)
}

/// `mono_message_submit` without Tauri (PR I2 review r2 m1): reads
/// `running`, submits, then reads `running` again with no store lock held
/// and drops a steer whose turn ended in between (design 16.12.3.3). The
/// control lock is never held inside a store transaction (5.3).
#[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
pub(crate) fn submit_and_recheck(
    store: &SessionStore,
    running: impl Fn() -> HashMap<String, String>,
    caller: &Caller,
    request_id: &RequestId,
    input: impl Into<Call>,
    live: &[LiveSession],
    steerable_harnesses: &[String],
    now: i64,
) -> Result<(Submitted, Notices), String> {
    let first = running();
    let mut submitted = {
        let conn = store.lock_conn()?;
        let live = Live {
            sessions: live,
            running: &first,
            steerable_harnesses,
        };
        match input.into() {
            Call::Message(input) => submit(&conn, caller, request_id, input, &live, now),
            Call::Ask(input) => asks::ask(&conn, caller, request_id, input, &live, now),
            Call::Ack(input) => asks::ack(&conn, caller, request_id, input, &live, now),
            Call::Talk(input) => asks::on_ask(&conn, caller, request_id, input, &live, now),
        }
        .map_err(|error| error.to_string())?
    };
    let again = running();
    let conn = store.lock_conn()?;
    let notices = drop_steers_of_ended_turns(&conn, &mut submitted, &first, &again, now)
        .map_err(|error| error.to_string())?;
    Ok((submitted, notices))
}

/// The route of a new accept, sent to the window that owns its session.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RouteEvent<'a> {
    messages: &'a [Targeted],
    route: &'a Routing,
    /// The window that made the call; the owner tells it the delivery.
    caller_window: &'a str,
}

#[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
#[tauri::command(async)]
pub fn mono_message_submit(
    app: AppHandle,
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    control: State<'_, ControlHost>,
    caller: CallerArg,
    tool: String,
    request_id: String,
    input: Value,
    live: Vec<LiveSession>,
    steerable_harnesses: Vec<String>,
) -> Result<Submitted, String> {
    let caller = caller.parse()?;
    let call = asks::call(&tool, &input).ok_or("Unknown messaging tool")?;
    let Some(request_id) = RequestId::parse(&request_id) else {
        return Ok(refused(Refusal::InvalidInput, REQUEST_ID_RULE.into()));
    };
    let (mut submitted, notices) = submit_and_recheck(
        &store,
        || control.running_sessions(),
        &caller,
        &request_id,
        call,
        &live,
        &steerable_harnesses,
        now_ms(),
    )?;
    notices.emit(&app);
    // Design 6: the window that owns a session runs its routes, so no other
    // window loads a second copy of it to deliver. A window that is gone
    // cannot (PR 4a review m11); the caller then routes the group.
    if let Submitted::Accepted {
        messages, routes, ..
    } = &mut submitted
    {
        for group in routes {
            let owner = control.route_owner(group.route.session_id.as_str(), window.label());
            if owner == window.label() || app.get_webview_window(&owner).is_none() {
                continue;
            }
            let rows: Vec<Targeted> = messages
                .iter()
                .filter(|row| group.message_ids.contains(&row.message.id))
                .cloned()
                .collect();
            let event = RouteEvent {
                messages: &rows,
                route: &group.route,
                caller_window: window.label(),
            };
            if app
                .emit_to(owner.as_str(), "mono-message-route", event)
                .is_ok()
            {
                group.owner_window = Some(owner);
            }
        }
    }
    Ok(submitted)
}

#[tauri::command(async)]
pub fn mono_message_assign_slot(
    app: AppHandle,
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
    kind: String,
) -> Result<Option<Targeted>, String> {
    let delivery = match kind.as_str() {
        "submit" => Delivery::Submit,
        "steer" => Delivery::Steer,
        _ => return Err("A hand-off is submit or steer".into()),
    };
    let (assigned, notices) = {
        let conn = store.lock_conn()?;
        assign_slot(
            &conn,
            &message_id(&id)?,
            parse_turn_id(&turn_id)?,
            window.label(),
            delivery,
            now_ms(),
        )
        .map_err(|error| error.to_string())?
    };
    notices.emit(&app);
    Ok(assigned)
}

#[tauri::command(async)]
pub fn mono_message_confirm_openers(
    store: State<'_, SessionStore>,
    session_id: String,
    turn_id: String,
) -> Result<(), String> {
    let session = SessionId::parse(&session_id).ok_or("Invalid session id")?;
    let conn = store.lock_conn()?;
    confirm_openers(&conn, &session, parse_turn_id(&turn_id)?, now_ms())
        .map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_message_release(
    app: AppHandle,
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
) -> Result<(), String> {
    let notices = {
        let conn = store.lock_conn()?;
        release(&conn, &message_id(&id)?, parse_turn_id(&turn_id)?, now_ms())
            .map_err(|error| error.to_string())?
    };
    notices.emit(&app);
    Ok(())
}

#[tauri::command(async)]
pub fn mono_message_mark_delivery(
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
) -> Result<(), String> {
    let conn = store.lock_conn()?;
    mark_delivery(&conn, &message_id(&id)?, parse_turn_id(&turn_id)?, now_ms())
        .map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_messages_unqueued(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<Vec<Targeted>, String> {
    let session = SessionId::parse(&session_id).ok_or("Invalid session id")?;
    let conn = store.lock_conn()?;
    unqueued(&conn, &session).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_messages_pending_notices(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<Vec<MonoMessage>, String> {
    let chat = SessionId::parse(&session_id).ok_or("Invalid session id")?;
    let conn = store.lock_conn()?;
    pending_notices(&conn, &chat).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_message_mark_notified(
    store: State<'_, SessionStore>,
    id: String,
    status: String,
) -> Result<bool, String> {
    let status =
        NoticeStatus::parse(&status).ok_or("A notice is for an unanswered or dropped message")?;
    let conn = store.lock_conn()?;
    mark_notified(&conn, &message_id(&id)?, status, now_ms()).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_message_drop(
    app: AppHandle,
    store: State<'_, SessionStore>,
    id: String,
) -> Result<DropAnswer, String> {
    let (answer, notices) = {
        let conn = store.lock_conn()?;
        drop_message(&conn, &message_id(&id)?, now_ms()).map_err(|error| error.to_string())?
    };
    notices.emit(&app);
    Ok(answer)
}

#[tauri::command(async)]
pub fn mono_message_get(
    store: State<'_, SessionStore>,
    caller: CallerArg,
    input: Value,
) -> Result<Got, String> {
    let caller = caller.parse()?;
    let conn = store.lock_conn()?;
    get_message(&conn, &caller, mono_gate::parse_get(&input)).map_err(|error| error.to_string())
}

/// The one argument of `mono_app_action_check`, as the web app's
/// `levelGate` sends it: `invoke("mono_app_action_check", { call })`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppActionCall {
    caller: CallerArg,
    action: String,
    target: Option<String>,
    started_session_id: Option<String>,
    request_id: String,
    input: Value,
}

impl AppActionCall {
    fn check(self, store: &SessionStore, now: i64) -> Result<ActionChecked, String> {
        let caller = self.caller.parse()?;
        let started = self
            .started_session_id
            .as_deref()
            .map(|id| SessionId::parse(id).ok_or("Invalid startedSessionId"))
            .transpose()?;
        let Some(request_id) = RequestId::parse(&self.request_id) else {
            return Ok(ActionChecked::Failed {
                error: refusal(Refusal::InvalidInput, REQUEST_ID_RULE.into()),
            });
        };
        let conn = store.lock_conn()?;
        check_action(
            &conn,
            &caller,
            &self.action,
            self.target.as_deref(),
            started.as_ref(),
            &request_id,
            &self.input,
            now,
        )
        .map_err(|error| error.to_string())
    }
}

#[tauri::command(async)]
pub fn mono_app_action_check(
    store: State<'_, SessionStore>,
    call: AppActionCall,
) -> Result<ActionChecked, String> {
    call.check(&store, now_ms())
}

/// The one argument of `mono_message_record_start`, as the web app's
/// `levelGate` sends it: `invoke("mono_message_record_start", { call })`.
/// `session_id` is the session the start launched.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordStartCall {
    caller: CallerArg,
    session_id: String,
    request_id: String,
    input: Value,
}

impl RecordStartCall {
    fn record(self, store: &SessionStore, now: i64) -> Result<StartRecorded, String> {
        let caller = self.caller.parse()?;
        let started = SessionId::parse(&self.session_id).ok_or("Invalid session id")?;
        let Some(request_id) = RequestId::parse(&self.request_id) else {
            return Ok(StartRecorded::Failed {
                error: refusal(Refusal::InvalidInput, REQUEST_ID_RULE.into()),
            });
        };
        let conn = store.lock_conn()?;
        record_start(&conn, &caller, &started, &request_id, &self.input, now)
            .map_err(|error| error.to_string())
    }
}

#[tauri::command(async)]
pub fn mono_message_record_start(
    store: State<'_, SessionStore>,
    call: RecordStartCall,
) -> Result<StartRecorded, String> {
    call.record(&store, now_ms())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session_store::SessionStore;
    use rusqlite::types::Value as Sql;

    fn store() -> SessionStore {
        let store = SessionStore::open_in_memory().unwrap();
        store
            .lock_conn()
            .unwrap()
            .execute_batch(
                "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
                 VALUES ('giga', 'G', 'giga', '{}', 0, 1, 1),
                        ('mono-a', 'A', 'mono', '{}', 1, 1, 1);",
            )
            .unwrap();
        store
    }

    fn id(letter: char) -> MessageId {
        MessageId::parse(&format!("m_{}", letter.to_string().repeat(12))).unwrap()
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn mono(id: &str) -> MonoId {
        MonoId::parse(id).unwrap()
    }

    fn routed() -> Routed {
        Routed {
            handled_by_session: session("a-chat"),
            delivery: Delivery::Submit,
        }
    }

    fn waiting(handle: i64, delivered_at: Option<i64>) -> WaitingSlot {
        WaitingSlot {
            turn_id: "turn-1".into(),
            handle: Handle::new(handle).unwrap(),
            delivered_at,
            window: "main".into(),
        }
    }

    fn message(letter: char, status: Status) -> MonoMessage {
        MonoMessage {
            id: id(letter),
            request_id: RequestId::parse(&format!("req-{letter}")).unwrap(),
            part: Part::FIRST,
            payload_hash: PayloadHash::parse(&"0".repeat(64)).unwrap(),
            tool: Tool::SendMessage,
            from_mono_id: Some(mono("giga")),
            from_session_id: session("giga-chat"),
            to: Some(Party::Mono {
                mono_id: mono("mono-a"),
            }),
            addressed: "mono-a".into(),
            body: Body::new("Ship the fix"),
            hop: 0,
            created_at: 10,
            status,
        }
    }

    #[test]
    fn every_status_reads_back_as_it_was_written() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let messages = [
            MonoMessage {
                body: Body::new(&"x".repeat(8001)),
                ..message(
                    'a',
                    Status::Rejected {
                        intent: IntentKind::New,
                        reply_to: None,
                        gate_result: RejectCode::TooLarge,
                        wanting_reply: true,
                        scope: None,
                    },
                )
            },
            message(
                'b',
                Status::Open {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: None,
                },
            ),
            message(
                'c',
                Status::Open {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: Some(waiting(1, Some(20))),
                },
            ),
            message(
                'd',
                Status::Delivered {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: Some(PlainSlot {
                        turn_id: "turn-1".into(),
                        delivered_at: None,
                        window: "main".into(),
                    }),
                },
            ),
            message(
                'e',
                Status::Answered {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: Some(waiting(2, Some(20))),
                    closed_at: 30,
                },
            ),
            message(
                'f',
                Status::Unanswered {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: DeliveredSlot {
                        turn_id: "turn-1".into(),
                        handle: Handle::new(3).unwrap(),
                        delivered_at: 20,
                        window: "main".into(),
                    },
                    closed_at: 30,
                    notified_at: Some(31),
                },
            ),
            message(
                'g',
                Status::Dropped {
                    intent: Intent::New,
                    wanting_reply: false,
                    closed_at: 30,
                    notified_at: None,
                },
            ),
        ];
        for message in &messages {
            insert(&conn, message).unwrap();
        }
        for message in &messages {
            assert_eq!(get(&conn, &message.id).unwrap().as_ref(), Some(message));
        }
        assert_eq!(get(&conn, &id('z')).unwrap(), None);
    }

    #[test]
    fn a_reply_to_two_handles_reads_back_in_part_order() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let asked = message(
            'a',
            Status::Answered {
                intent: Intent::New,
                wanting_reply: Wanting,
                routed: routed(),
                slot: Some(waiting(1, Some(20))),
                closed_at: 30,
            },
        );
        insert(&conn, &asked).unwrap();
        let part = |letter, part| MonoMessage {
            part: Part::new(part).unwrap(),
            request_id: RequestId::parse("reply-1").unwrap(),
            tool: Tool::Reply,
            from_mono_id: Some(mono("mono-a")),
            from_session_id: session("a-chat"),
            to: Some(Party::Session {
                session_id: session("giga-chat"),
            }),
            hop: 1,
            ..message(
                letter,
                Status::Delivered {
                    intent: Intent::Reply(asked.id.clone()),
                    wanting_reply: Wanting,
                    routed: Routed {
                        handled_by_session: session("giga-chat"),
                        delivery: Delivery::Queue,
                    },
                    slot: None,
                },
            )
        };
        let (second, first) = (part('c', 1), part('b', 0));
        insert(&conn, &second).unwrap();
        insert(&conn, &first).unwrap();
        let reply = RequestId::parse("reply-1").unwrap();
        assert_eq!(
            get_by_request(&conn, &session("a-chat"), &reply).unwrap(),
            [first, second]
        );
        assert!(get_by_request(&conn, &session("giga-chat"), &reply)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_body_keeps_8000_code_points_and_the_real_length() {
        let long = Body::new(&"é".repeat(8001));
        assert_eq!(long.text().chars().count(), Body::MAX_CHARS);
        assert_eq!(long.chars, 8001);
        let short = Body::new("Ship the fix");
        assert_eq!((short.text(), short.chars), ("Ship the fix", 12));
    }

    #[test]
    fn parts_and_stored_text_values_are_built_only_within_the_table_rules() {
        assert_eq!(Part::new(9), Some(Part(9)));
        assert_eq!(Part::new(10), None);
        assert_eq!(Part::new(-1), None);
        assert!(RequestId::parse(&"r".repeat(128)).is_some());
        assert!(RequestId::parse(&"r".repeat(129)).is_none());
        assert!(RequestId::parse("").is_none());
        assert!(PayloadHash::parse(&"a".repeat(64)).is_some());
        assert!(PayloadHash::parse(&"a".repeat(63)).is_none());
        assert!(PayloadHash::parse(&"A".repeat(64)).is_none());
        assert!(SessionId::parse("chat#0").is_none());
        assert!(MonoId::parse("a_b").is_none());
        assert!(RequestId::parse("a b").is_none());
        assert!(RequestId::parse("req#1").is_none());
        assert!(MessageId::parse("m_abcdefgh2345").is_some());
        assert!(MessageId::parse("m_ABCDEFGHIJKL").is_none());
        assert!(MessageId::parse("m_abcdefgh2341").is_none());
    }

    #[test]
    fn rows_serialize_as_the_web_app_type_without_the_slot_window() {
        let open = message(
            'a',
            Status::Open {
                intent: Intent::New,
                wanting_reply: Wanting,
                routed: routed(),
                slot: Some(waiting(1, None)),
            },
        );
        let json = serde_json::to_value(&open).unwrap();
        assert_eq!(json["status"], "open");
        assert!(json.get("payloadHash").is_none(), "{json}");
        assert_eq!(json["wantingReply"], true);
        assert_eq!(json["handledBySession"], "a-chat");
        assert_eq!(json["delivery"], "submit");
        assert_eq!(
            json["to"],
            serde_json::json!({"kind": "mono", "monoId": "mono-a"})
        );
        assert_eq!(
            json["slot"],
            serde_json::json!({"turnId": "turn-1", "handle": "#1", "deliveredAt": null})
        );
        let rejected = message(
            'b',
            Status::Rejected {
                intent: IntentKind::New,
                reply_to: None,
                gate_result: RejectCode::HandleClosed,
                wanting_reply: false,
                scope: None,
            },
        );
        let json = serde_json::to_value(&rejected).unwrap();
        assert_eq!(json["gateResult"], "handle_closed");
        assert_eq!(json["wantingReply"], false);
    }

    #[test]
    fn a_rejected_row_with_an_unknown_gate_result_is_an_error_not_a_message() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(
            &conn,
            &with(row("rejected"), &[("gate_result", text("not_a_code"))]),
        )
        .unwrap();
        assert!(get(&conn, &MessageId::parse("m_aaaaaaaaaaaa").unwrap()).is_err());
    }

    const PR3_TABLE: &str = "CREATE TABLE IF NOT EXISTS mono_messages (
           id                 TEXT PRIMARY KEY CHECK (length(id) = 14 AND substr(id, 1, 2) = 'm_'),
           request_id         TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
           part               INTEGER NOT NULL DEFAULT 0 CHECK (part BETWEEN 0 AND 9),
           payload_hash       TEXT NOT NULL CHECK (length(payload_hash) = 64),
           tool               TEXT NOT NULL CHECK (tool IN ('send_message', 'reply')),
           from_mono_id       TEXT REFERENCES monos(id) ON DELETE SET NULL,
           from_session_id    TEXT NOT NULL,
           to_mono_id         TEXT REFERENCES monos(id) ON DELETE SET NULL,
           to_session_id      TEXT,
           addressed          TEXT NOT NULL,
           body               TEXT NOT NULL CHECK (length(body) <= 8000),
           body_chars         INTEGER NOT NULL CHECK (body_chars >= 0),
           wanting_reply      INTEGER NOT NULL CHECK (wanting_reply IN (0, 1)),
           action_type        TEXT CHECK (action_type IN ('queue', 'steer')),
           reply_to           TEXT REFERENCES mono_messages(id),
           steers             TEXT REFERENCES mono_messages(id),
           hop                INTEGER NOT NULL CHECK (hop >= 0),
           status             TEXT NOT NULL CHECK (status IN
                                ('open', 'answered', 'delivered', 'unanswered', 'dropped', 'rejected')),
           gate_result        TEXT NOT NULL,
           handled_by_session TEXT,
           delivery           TEXT CHECK (delivery IN ('submit', 'queue', 'steer')),
           turn_id            TEXT,
           slot_window        TEXT,
           handle             INTEGER CHECK (handle BETWEEN 1 AND 99),
           delivered_at       INTEGER,
           closed_at          INTEGER,
           notified_at        INTEGER,
           created_at         INTEGER NOT NULL,
           updated_at         INTEGER NOT NULL,
           UNIQUE (from_session_id, request_id, part),
           CHECK ((status = 'rejected') = (gate_result <> 'ok')),
           CHECK (status NOT IN ('open', 'unanswered') OR wanting_reply = 1),
           CHECK (status <> 'delivered' OR wanting_reply = 0),
           CHECK (handle IS NULL OR (turn_id IS NOT NULL AND wanting_reply = 1)),
           CHECK (status <> 'rejected' OR (handled_by_session IS NULL AND delivery IS NULL AND turn_id IS NULL)),
           CHECK (tool <> 'reply' OR status = 'rejected' OR reply_to IS NOT NULL),
           CHECK (to_mono_id IS NULL OR to_session_id IS NULL),
           CHECK (status <> 'answered' OR wanting_reply = 1),
           CHECK (status IN ('rejected', 'dropped') OR (handled_by_session IS NOT NULL AND delivery IS NOT NULL)),
           CHECK (delivered_at IS NULL OR turn_id IS NOT NULL),
           CHECK (turn_id IS NULL OR wanting_reply = 0 OR handle IS NOT NULL),
           CHECK (status <> 'unanswered' OR delivered_at IS NOT NULL),
           CHECK ((slot_window IS NULL) = (turn_id IS NULL)),
           CHECK ((closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))),
           CHECK (status = 'rejected' OR body_chars = length(body)),
           CHECK (tool <> 'send_message' OR length(body) > 0),
           CHECK (notified_at IS NULL OR status IN ('unanswered', 'dropped')),
           CHECK (turn_id IS NULL OR delivery IN ('submit', 'steer')),
           CHECK (status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL),
           CHECK (tool <> 'reply' OR (steers IS NULL AND action_type IS NULL)),
           CHECK (part = 0 OR (tool = 'reply' AND status <> 'rejected')),
           CHECK (status = 'rejected' OR (reply_to IS NULL) = (hop = 0)),
           CHECK (id GLOB 'm_[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
           CHECK (payload_hash NOT GLOB '*[^0-9a-f]*'),
           CHECK (request_id NOT GLOB '*[^A-Za-z0-9_-]*'),
           CHECK (length(body) = min(body_chars, 8000)),
           CHECK ((gate_result = 'too_large') = (body_chars > 8000))
         );
         CREATE UNIQUE INDEX IF NOT EXISTS mono_messages_handle
           ON mono_messages (handled_by_session, turn_id, handle) WHERE handle IS NOT NULL;
         CREATE INDEX IF NOT EXISTS mono_messages_pair
           ON mono_messages (from_mono_id, to_mono_id, to_session_id, created_at)
           WHERE tool = 'send_message' AND status <> 'rejected';
         CREATE INDEX IF NOT EXISTS mono_messages_session
           ON mono_messages (handled_by_session, status) WHERE handled_by_session IS NOT NULL;
         CREATE INDEX IF NOT EXISTS mono_messages_from_session
           ON mono_messages (from_session_id, status) WHERE status IN ('open', 'unanswered');
         CREATE INDEX IF NOT EXISTS mono_messages_waiting
           ON mono_messages (to_mono_id, status) WHERE status IN ('open', 'unanswered');
         CREATE INDEX IF NOT EXISTS mono_messages_notice
           ON mono_messages (status) WHERE status IN ('unanswered', 'dropped') AND notified_at IS NULL;
         CREATE INDEX IF NOT EXISTS mono_messages_reply_to
           ON mono_messages (reply_to) WHERE reply_to IS NOT NULL;";

    /// A store as PR 3 (9f2d3e3) left it, with Monos `giga` and `mono-a`.
    fn pr3_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::mono_roster::ensure_tables(&conn).unwrap();
        conn.execute_batch(PR3_TABLE).unwrap();
        conn.execute_batch(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
             VALUES ('giga', 'G', 'giga', '{}', 0, 1, 1),
                    ('mono-a', 'A', 'mono', '{}', 1, 1, 1);",
        )
        .unwrap();
        conn
    }

    /// One PR 3 row: an accepted one is delivered from G's chat into A's.
    /// `columns` overrides any other column, for the PR 5 and PR 6 shapes.
    struct Pr3Row {
        id: char,
        tool: &'static str,
        gate_result: &'static str,
        action_type: Option<&'static str>,
        reply_to: Option<char>,
        steers: Option<char>,
        hop: i64,
        columns: Vec<(&'static str, Sql)>,
    }

    fn insert_pr3(conn: &Connection, row: Pr3Row) -> rusqlite::Result<usize> {
        let accepted = row.gate_result == "ok";
        let target =
            |letter: Option<char>| letter.map_or(Sql::Null, |letter| text(id(letter).as_str()));
        let optional = |value: Option<&str>| value.map_or(Sql::Null, text);
        let columns = vec![
            ("id", text(id(row.id).as_str())),
            ("request_id", text(&format!("req-{}", row.id))),
            ("part", Sql::Integer(0)),
            ("payload_hash", text(&"0".repeat(64))),
            ("tool", text(row.tool)),
            ("from_mono_id", text("giga")),
            ("from_session_id", text("giga-chat")),
            ("to_mono_id", text("mono-a")),
            ("addressed", text("mono-a")),
            ("body", text("hi")),
            ("body_chars", Sql::Integer(2)),
            ("wanting_reply", Sql::Integer(0)),
            ("action_type", optional(row.action_type)),
            ("reply_to", target(row.reply_to)),
            ("steers", target(row.steers)),
            ("hop", Sql::Integer(row.hop)),
            (
                "status",
                text(if accepted { "delivered" } else { "rejected" }),
            ),
            ("gate_result", text(row.gate_result)),
            ("handled_by_session", optional(accepted.then_some("a-chat"))),
            ("delivery", optional(accepted.then_some("queue"))),
            ("created_at", Sql::Integer(1)),
            ("updated_at", Sql::Integer(1)),
        ];
        raw_insert(conn, &with(columns, &row.columns))
    }

    fn pr3_send(id: char) -> Pr3Row {
        Pr3Row {
            id,
            tool: "send_message",
            gate_result: "ok",
            action_type: Some("queue"),
            reply_to: None,
            steers: None,
            hop: 0,
            columns: Vec::new(),
        }
    }

    /// Every row, each column but the ones the rebuild maps, by column name.
    fn copied_columns(conn: &Connection) -> Vec<Vec<(String, Sql)>> {
        const MAPPED: [&str; 8] = [
            "action_type",
            "steers",
            "intent",
            "reply_to",
            "hop",
            "scope",
            "scope_turn_id",
            "pin_turn_id",
        ];
        let mut statement = conn
            .prepare("SELECT * FROM mono_messages ORDER BY id")
            .unwrap();
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        statement
            .query_map([], |row| {
                let mut columns = Vec::new();
                for (index, name) in names.iter().enumerate() {
                    if !MAPPED.contains(&name.as_str()) {
                        columns.push((name.clone(), row.get::<_, Sql>(index)?));
                    }
                }
                columns.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(columns)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn intents(conn: &Connection) -> Vec<(String, String, Option<String>, i64)> {
        conn.prepare("SELECT id, intent, reply_to, hop FROM mono_messages ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn schema(conn: &Connection) -> Vec<Option<String>> {
        conn.prepare("SELECT sql FROM sqlite_master ORDER BY type, name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn has_column(conn: &Connection, column: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('mono_messages') WHERE name = ?1)",
            [column],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn foreign_keys(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap()
    }

    // Review r1 m1. Defect: a database made before the late-answer pin keeps
    // a table without `pin_turn_id`, so every read and write of a row fails
    // there; or a second open adds the column again and fails.
    #[test]
    fn ensure_tables_adds_pin_turn_id_to_a_table_made_before_it() {
        let conn = Connection::open_in_memory().unwrap();
        let before = table_sql("mono_messages").replace(&format!("{PIN_TURN_ID},"), "");
        conn.execute_batch(&before).unwrap();
        assert!(!has_column(&conn, "pin_turn_id"));
        ensure_tables(&conn).unwrap();
        ensure_tables(&conn).unwrap();
        assert!(has_column(&conn, "pin_turn_id"));
        assert!(get(&conn, &MessageId::parse("m_aaaaaaaaaaaa").unwrap())
            .unwrap()
            .is_none());
    }

    // Design 16.2 map. Defects: a wrong CASE line, a lost row, or a column
    // value lost or moved by the copy.
    #[test]
    #[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
    fn the_rebuild_maps_every_old_row_shape() {
        let conn = pr3_database();
        for row in [
            pr3_send('a'),
            Pr3Row {
                action_type: Some("steer"),
                ..pr3_send('b')
            },
            Pr3Row {
                steers: Some('a'),
                ..pr3_send('c')
            },
            Pr3Row {
                tool: "reply",
                action_type: None,
                reply_to: Some('a'),
                hop: 1,
                columns: vec![
                    ("delivery", text("submit")),
                    ("turn_id", text("turn-3")),
                    ("slot_window", text("side")),
                    ("delivered_at", Sql::Integer(5)),
                    ("created_at", Sql::Integer(2)),
                    ("updated_at", Sql::Integer(5)),
                ],
                ..pr3_send('d')
            },
            Pr3Row {
                tool: "reply",
                gate_result: "handle_closed",
                action_type: None,
                ..pr3_send('e')
            },
            Pr3Row {
                gate_result: "self_send",
                ..pr3_send('f')
            },
            Pr3Row {
                gate_result: "cannot_steer",
                action_type: Some("steer"),
                ..pr3_send('g')
            },
            Pr3Row {
                reply_to: Some('a'),
                hop: 1,
                ..pr3_send('h')
            },
            // PR 5: an answered steer delivery in a turn, with slot and handle.
            Pr3Row {
                action_type: Some("steer"),
                columns: vec![
                    ("status", text("answered")),
                    ("wanting_reply", Sql::Integer(1)),
                    ("delivery", text("steer")),
                    ("turn_id", text("turn-1")),
                    ("slot_window", text("main")),
                    ("handle", Sql::Integer(1)),
                    ("delivered_at", Sql::Integer(4)),
                    ("closed_at", Sql::Integer(6)),
                    ("created_at", Sql::Integer(3)),
                    ("updated_at", Sql::Integer(6)),
                ],
                ..pr3_send('i')
            },
            // PR 5: unanswered and noticed.
            Pr3Row {
                columns: vec![
                    ("status", text("unanswered")),
                    ("wanting_reply", Sql::Integer(1)),
                    ("delivery", text("submit")),
                    ("turn_id", text("turn-2")),
                    ("slot_window", text("side")),
                    ("handle", Sql::Integer(2)),
                    ("delivered_at", Sql::Integer(5)),
                    ("closed_at", Sql::Integer(7)),
                    ("notified_at", Sql::Integer(8)),
                    ("created_at", Sql::Integer(4)),
                    ("updated_at", Sql::Integer(8)),
                ],
                ..pr3_send('j')
            },
            // PR 6: dropped before any route, sent to a session, from no Mono.
            Pr3Row {
                columns: vec![
                    ("status", text("dropped")),
                    ("wanting_reply", Sql::Integer(1)),
                    ("from_mono_id", Sql::Null),
                    ("to_mono_id", Sql::Null),
                    ("to_session_id", text("b-chat")),
                    ("addressed", text("b-chat")),
                    ("handled_by_session", Sql::Null),
                    ("delivery", Sql::Null),
                    ("closed_at", Sql::Integer(9)),
                    ("notified_at", Sql::Integer(10)),
                    ("created_at", Sql::Integer(5)),
                    ("updated_at", Sql::Integer(10)),
                ],
                ..pr3_send('k')
            },
            // A rejected `steers` row keeps its hop.
            Pr3Row {
                gate_result: "steer_target_mismatch",
                steers: Some('a'),
                ..pr3_send('l')
            },
        ] {
            insert_pr3(&conn, row).unwrap();
        }
        let copied = copied_columns(&conn);
        ensure_tables(&conn).unwrap();
        assert_eq!(copied_columns(&conn), copied);
        let a = Some(id('a').as_str().to_owned());
        let row = |letter: char, intent: &str, reply_to: &Option<String>, hop| {
            (
                id(letter).as_str().to_owned(),
                intent.to_owned(),
                reply_to.clone(),
                hop,
            )
        };
        assert_eq!(
            intents(&conn),
            vec![
                row('a', "new", &None, 0),
                row('b', "new", &None, 0),
                row('c', "steer", &a, 1),
                row('d', "reply", &a, 1),
                row('e', "reply", &None, 0),
                row('f', "new", &None, 0),
                row('g', "steer", &None, 0),
                row('h', "steer", &a, 1),
                row('i', "new", &None, 0),
                row('j', "new", &None, 0),
                row('k', "new", &None, 0),
                row('l', "steer", &a, 0),
            ]
        );
        for letter in "abcdefghijkl".chars() {
            assert!(get(&conn, &id(letter)).unwrap().is_some(), "{letter}");
        }
    }

    // Decision 236. Defect: the rebuild runs with foreign keys on and fails
    // once a row names another, leaves them off, or runs on every open.
    #[test]
    fn the_rebuild_keeps_foreign_keys_on_and_runs_once() {
        let conn = pr3_database();
        insert_pr3(&conn, pr3_send('a')).unwrap();
        insert_pr3(
            &conn,
            Pr3Row {
                steers: Some('a'),
                ..pr3_send('b')
            },
        )
        .unwrap();
        ensure_tables(&conn).unwrap();
        assert!(has_column(&conn, "intent") && !has_column(&conn, "steers"));
        assert_eq!(foreign_keys(&conn), 1);
        let dangling = raw_insert(
            &conn,
            &with(
                row("delivered"),
                &[
                    ("id", text("m_cccccccccccc")),
                    ("intent", text("steer")),
                    ("reply_to", text("m_zzzzzzzzzzzz")),
                    ("hop", Sql::Integer(1)),
                ],
            ),
        )
        .unwrap_err()
        .to_string();
        assert!(
            dangling.contains("FOREIGN KEY constraint failed"),
            "{dangling}"
        );
        let before = schema(&conn);
        ensure_tables(&conn).unwrap();
        assert_eq!(schema(&conn), before);
        assert_eq!(intents(&conn).len(), 2);
    }

    // Design 16.2 step 6. Defect: a half rebuild, or a store left with
    // foreign keys off.
    #[test]
    fn a_failed_rebuild_leaves_the_old_table() {
        let conn = pr3_database();
        insert_pr3(&conn, pr3_send('a')).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        insert_pr3(
            &conn,
            Pr3Row {
                reply_to: Some('z'),
                hop: 1,
                ..pr3_send('b')
            },
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let error = ensure_tables(&conn).unwrap_err().to_string();
        assert!(
            error.contains("name a message that does not exist"),
            "{error}"
        );
        assert!(has_column(&conn, "steers") && !has_column(&conn, "intent"));
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM mono_messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 2);
        assert_eq!(foreign_keys(&conn), 1);
    }

    // Design 16.2 step 3. Defect: a table left by an aborted manual run
    // blocks every later open.
    #[test]
    fn a_leftover_rebuild_table_does_not_block_the_rebuild() {
        let conn = pr3_database();
        insert_pr3(&conn, pr3_send('a')).unwrap();
        conn.execute_batch(
            "CREATE TABLE mono_messages_new (id TEXT); INSERT INTO mono_messages_new VALUES ('x');",
        )
        .unwrap();
        ensure_tables(&conn).unwrap();
        assert!(has_column(&conn, "intent"));
        assert_eq!(intents(&conn).len(), 1);
        let leftover: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'mono_messages_new')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!leftover);
    }

    // Design 16.1. Defect: the web app parses another shape than Rust writes.
    #[test]
    fn rows_serialize_intent_and_reply_to_flat() {
        let target = id('z');
        let steer = message(
            'a',
            Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::Steer(SteerTarget::Message(target.clone())),
                routed: routed(),
                slot: None,
            },
        );
        let json = serde_json::to_value(&steer).unwrap();
        assert_eq!(
            (&json["intent"], &json["replyTo"]),
            (
                &serde_json::json!("steer"),
                &serde_json::json!("m_zzzzzzzzzzzz")
            )
        );
        let new = message(
            'b',
            Status::Open {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: routed(),
                slot: None,
            },
        );
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(
            (&json["intent"], &json["replyTo"]),
            (&serde_json::json!("new"), &serde_json::Value::Null)
        );
        assert!(json.as_object().unwrap().contains_key("replyTo"), "{json}");
        let refused = message(
            'c',
            Status::Rejected {
                gate_result: RejectCode::UnknownMessage,
                wanting_reply: false,
                intent: IntentKind::Continue,
                reply_to: Some(target),
                scope: None,
            },
        );
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(
            (&json["intent"], &json["replyTo"]),
            (
                &serde_json::json!("continue"),
                &serde_json::json!("m_zzzzzzzzzzzz")
            )
        );
    }

    #[test]
    fn ensure_tables_twice_is_a_no_op() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let kept = message(
            'a',
            Status::Dropped {
                intent: Intent::New,
                wanting_reply: true,
                closed_at: 30,
                notified_at: None,
            },
        );
        insert(&conn, &kept).unwrap();
        conn.execute(
            "INSERT INTO mono_session_ends (session_id, ended_at) VALUES ('a-chat', 1)",
            [],
        )
        .unwrap();
        ensure_tables(&conn).unwrap();
        assert_eq!(get(&conn, &kept.id).unwrap(), Some(kept));
        let ends: i64 = conn
            .query_row("SELECT count(*) FROM mono_session_ends", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(ends, 1);
    }

    // Design 16.1: a row whose columns disagree fails the parse. The CHECKs
    // keep such a row out today, so they are off here. Defect: `parse_row`
    // reads an accepted row whose intent and `reply_to` disagree.
    #[test]
    fn parse_row_refuses_an_accepted_row_whose_intent_and_reply_to_disagree() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(&conn, &row("delivered")).unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        let a = text(id('a').as_str());
        for (letter, intent, reply_to) in [
            ('b', "new", a.clone()),
            ('c', "reply", Sql::Null),
            ('d', "steer", Sql::Null),
            ('e', "continue", a.clone()),
        ] {
            let message = id(letter);
            raw_insert(
                &conn,
                &with(
                    row("delivered"),
                    &[
                        ("id", text(message.as_str())),
                        ("request_id", text(&format!("req-{letter}"))),
                        ("intent", text(intent)),
                        ("reply_to", reply_to),
                    ],
                ),
            )
            .unwrap();
            let error = get(&conn, &message).unwrap_err().to_string();
            assert!(
                error.contains("invalid mono_messages.intent"),
                "{intent}: {error}"
            );
        }
    }

    fn text(value: &str) -> Sql {
        Sql::Text(value.into())
    }

    /// A legal row with the given status, as raw columns.
    fn row(status: &str) -> Vec<(&'static str, Sql)> {
        let row = vec![
            ("id", text("m_aaaaaaaaaaaa")),
            ("request_id", text("req-1")),
            ("part", Sql::Integer(0)),
            ("payload_hash", text(&"0".repeat(64))),
            ("tool", text("send_message")),
            ("from_mono_id", text("giga")),
            ("from_session_id", text("giga-chat")),
            ("intent", text("new")),
            ("to_mono_id", text("mono-a")),
            ("addressed", text("mono-a")),
            ("body", text("hi")),
            ("body_chars", Sql::Integer(2)),
            ("hop", Sql::Integer(0)),
            ("status", text(status)),
            ("gate_result", text("ok")),
            ("handled_by_session", text("a-chat")),
            ("delivery", text("queue")),
            ("created_at", Sql::Integer(1)),
            ("updated_at", Sql::Integer(1)),
        ];
        let wanting = |row, wanting| with(row, &[("wanting_reply", Sql::Integer(wanting))]);
        match status {
            "delivered" => wanting(row, 0),
            "open" => wanting(row, 1),
            "answered" => with(
                row,
                &[
                    ("wanting_reply", Sql::Integer(1)),
                    ("closed_at", Sql::Integer(3)),
                ],
            ),
            "rejected" => with(
                row,
                &[
                    ("wanting_reply", Sql::Integer(0)),
                    ("gate_result", text("self_send")),
                    ("handled_by_session", Sql::Null),
                    ("delivery", Sql::Null),
                ],
            ),
            "unanswered" => with(
                row,
                &[
                    ("wanting_reply", Sql::Integer(1)),
                    ("delivery", text("submit")),
                    ("turn_id", text("turn-1")),
                    ("slot_window", text("main")),
                    ("handle", Sql::Integer(1)),
                    ("delivered_at", Sql::Integer(2)),
                    ("closed_at", Sql::Integer(3)),
                ],
            ),
            "dropped" => with(
                row,
                &[
                    ("wanting_reply", Sql::Integer(0)),
                    ("closed_at", Sql::Integer(3)),
                ],
            ),
            _ => unreachable!("no template for {status}"),
        }
    }

    fn with(
        mut row: Vec<(&'static str, Sql)>,
        changes: &[(&'static str, Sql)],
    ) -> Vec<(&'static str, Sql)> {
        for (column, value) in changes {
            match row.iter_mut().find(|(name, _)| name == column) {
                Some(cell) => cell.1 = value.clone(),
                None => row.push((column, value.clone())),
            }
        }
        row
    }

    fn raw_insert(conn: &Connection, row: &[(&'static str, Sql)]) -> rusqlite::Result<usize> {
        let columns: Vec<&str> = row.iter().map(|(name, _)| *name).collect();
        let marks: Vec<String> = (1..=row.len()).map(|index| format!("?{index}")).collect();
        conn.execute(
            &format!(
                "INSERT INTO mono_messages ({}) VALUES ({})",
                columns.join(", "),
                marks.join(", ")
            ),
            rusqlite::params_from_iter(row.iter().map(|(_, value)| value)),
        )
    }

    /// The legal `status` row inserts; with `changes` the CHECK containing
    /// `check` refuses it.
    fn assert_check_refuses(status: &str, changes: &[(&'static str, Sql)], check: &str) {
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(&conn, &row(status)).unwrap();
        let illegal = with(
            with(
                row(status),
                &[
                    ("id", text("m_bbbbbbbbbbbb")),
                    ("request_id", text("req-2")),
                ],
            ),
            changes,
        );
        let error = raw_insert(&conn, &illegal).unwrap_err().to_string();
        assert!(
            error.contains("CHECK constraint failed") && error.contains(check),
            "expected the CHECK `{check}` to refuse the row, got: {error}"
        );
    }

    #[test]
    fn check_refuses_an_open_row_that_wants_no_reply() {
        assert_check_refuses(
            "open",
            &[("wanting_reply", Sql::Integer(0))],
            "status NOT IN ('open', 'unanswered') OR wanting_reply = 1",
        );
    }

    #[test]
    fn check_refuses_a_handle_without_a_turn() {
        assert_check_refuses(
            "open",
            &[("handle", Sql::Integer(1))],
            "handle IS NULL OR (turn_id IS NOT NULL AND wanting_reply = 1)",
        );
    }

    #[test]
    fn check_refuses_a_rejected_row_whose_gate_result_is_ok() {
        assert_check_refuses(
            "rejected",
            &[("gate_result", text("ok"))],
            "(status = 'rejected') = (gate_result <> 'ok')",
        );
    }

    #[test]
    fn check_refuses_a_body_of_8001_characters() {
        assert_check_refuses(
            "delivered",
            &[
                ("body", text(&"x".repeat(8001))),
                ("body_chars", Sql::Integer(8001)),
            ],
            "length(body) <= 8000",
        );
    }

    #[test]
    fn check_refuses_an_open_row_in_a_turn_without_a_handle() {
        assert_check_refuses(
            "open",
            &[
                ("turn_id", text("turn-1")),
                ("slot_window", text("main")),
                ("delivery", text("submit")),
            ],
            "turn_id IS NULL OR wanting_reply = 0 OR handle IS NOT NULL",
        );
    }

    #[test]
    fn check_refuses_an_unanswered_row_never_delivered() {
        assert_check_refuses(
            "unanswered",
            &[("delivered_at", Sql::Null)],
            "status <> 'unanswered' OR delivered_at IS NOT NULL",
        );
    }

    #[test]
    fn check_refuses_an_accepted_row_without_a_handling_session() {
        assert_check_refuses(
            "delivered",
            &[("handled_by_session", Sql::Null)],
            "handled_by_session IS NOT NULL AND delivery IS NOT NULL",
        );
    }

    #[test]
    fn check_refuses_a_slot_window_without_a_turn() {
        assert_check_refuses(
            "delivered",
            &[("slot_window", text("main"))],
            "(slot_window IS NULL) = (turn_id IS NULL)",
        );
    }

    #[test]
    fn check_refuses_a_turn_without_a_slot_window() {
        assert_check_refuses(
            "delivered",
            &[("turn_id", text("turn-1")), ("delivery", text("submit"))],
            "(slot_window IS NULL) = (turn_id IS NULL)",
        );
    }

    #[test]
    fn check_refuses_a_dropped_row_without_closed_at() {
        assert_check_refuses(
            "dropped",
            &[("closed_at", Sql::Null)],
            "(closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))",
        );
    }

    #[test]
    fn check_refuses_a_rejected_row_with_closed_at() {
        assert_check_refuses(
            "rejected",
            &[("closed_at", Sql::Integer(3))],
            "(closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))",
        );
    }

    #[test]
    fn check_refuses_a_delivered_row_that_wants_a_reply() {
        assert_check_refuses(
            "delivered",
            &[("wanting_reply", Sql::Integer(1))],
            "status <> 'delivered' OR wanting_reply = 0",
        );
    }

    #[test]
    fn check_refuses_a_rejected_row_with_a_handling_session() {
        assert_check_refuses(
            "rejected",
            &[("handled_by_session", text("a-chat"))],
            "status <> 'rejected' OR (handled_by_session IS NULL AND delivery IS NULL AND turn_id IS NULL)",
        );
    }

    #[test]
    fn check_refuses_a_row_sent_to_a_mono_and_a_session() {
        assert_check_refuses(
            "delivered",
            &[("to_session_id", text("project-session"))],
            "to_mono_id IS NULL OR to_session_id IS NULL",
        );
    }

    #[test]
    fn check_refuses_an_answered_row_that_wanted_no_reply() {
        assert_check_refuses(
            "answered",
            &[("wanting_reply", Sql::Integer(0))],
            "status <> 'answered' OR wanting_reply = 1",
        );
    }

    #[test]
    fn check_refuses_delivered_at_without_a_turn() {
        assert_check_refuses(
            "delivered",
            &[("delivered_at", Sql::Integer(2))],
            "delivered_at IS NULL OR turn_id IS NOT NULL",
        );
    }

    #[test]
    fn check_refuses_an_accepted_row_whose_body_chars_disagree_with_its_body() {
        assert_check_refuses(
            "delivered",
            &[("body_chars", Sql::Integer(20_000))],
            "status = 'rejected' OR body_chars = length(body)",
        );
    }

    #[test]
    fn check_refuses_a_send_with_an_empty_body() {
        assert_check_refuses(
            "delivered",
            &[("body", text("")), ("body_chars", Sql::Integer(0))],
            "tool <> 'send_message' OR length(body) > 0",
        );
    }

    #[test]
    fn check_refuses_a_notice_on_an_open_row() {
        assert_check_refuses(
            "open",
            &[("notified_at", Sql::Integer(4))],
            "notified_at IS NULL OR status IN ('unanswered', 'dropped')",
        );
    }

    #[test]
    fn check_refuses_a_slot_whose_delivery_is_still_queue() {
        assert_check_refuses(
            "delivered",
            &[("turn_id", text("turn-1")), ("slot_window", text("main"))],
            "turn_id IS NULL OR delivery IN ('submit', 'steer')",
        );
    }

    #[test]
    fn check_refuses_dropping_a_no_reply_row_already_delivered() {
        assert_check_refuses(
            "dropped",
            &[
                ("turn_id", text("turn-1")),
                ("slot_window", text("main")),
                ("delivery", text("submit")),
                ("delivered_at", Sql::Integer(2)),
            ],
            "status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL",
        );
    }

    // Design 16.2. Defect: a row says new and names a target, or steers
    // nothing, so the tree of messages has a node the gate never accepted.
    #[test]
    fn check_refuses_a_new_row_with_a_target_and_a_steer_without_one() {
        assert_check_refuses(
            "delivered",
            &[
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
            ],
            "status = 'rejected' OR (intent = 'new') = (reply_to IS NULL AND scope IS NULL)",
        );
        assert_check_refuses(
            "delivered",
            &[("intent", text("steer"))],
            "status = 'rejected' OR (intent = 'new') = (reply_to IS NULL AND scope IS NULL)",
        );
    }

    // Design 16.12.3.1 (whole-turn). Defect: another writer stores a steer
    // with no target or two, a new row with a scope, or a whole-turn row
    // without its pin, which parse_row then cannot read.
    #[test]
    fn the_table_refuses_a_steer_with_no_target_or_two() {
        let whole_turn = [
            ("intent", text("steer")),
            ("scope", text("turn")),
            ("scope_turn_id", text("turn-1")),
        ];
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(&conn, &with(row("delivered"), &whole_turn)).unwrap();
        assert_eq!(
            get(&conn, &id('a')).unwrap().unwrap().status.intent(),
            Some(&Intent::Steer(SteerTarget::Turn {
                turn_id: "turn-1".into()
            }))
        );
        let both = [
            ("reply_to", text("m_aaaaaaaaaaaa")),
            ("hop", Sql::Integer(1)),
        ];
        assert_check_refuses(
            "delivered",
            &[&whole_turn[..], &both[..]].concat(),
            "reply_to IS NULL OR scope IS NULL",
        );
        assert_check_refuses(
            "delivered",
            &whole_turn[1..],
            "status = 'rejected' OR (intent = 'new') = (reply_to IS NULL AND scope IS NULL)",
        );
        let pin = "(scope_turn_id IS NOT NULL) = (scope IS NOT NULL AND status <> 'rejected')";
        assert_check_refuses("delivered", &whole_turn[..2], pin);
        assert_check_refuses("rejected", &whole_turn, pin);
        assert_check_refuses(
            "rejected",
            &[
                ("tool", text("reply")),
                ("intent", text("reply")),
                ("scope", text("turn")),
            ],
            "scope IS NULL OR tool = 'send_message'",
        );
        assert_check_refuses(
            "delivered",
            &[&whole_turn[..], &[("scope", text("session"))][..]].concat(),
            "scope IN ('turn')",
        );
    }

    // Defect: an agent's continue is stored as accepted and leaves the
    // question it names open with no error.
    #[test]
    fn check_refuses_an_accepted_continue() {
        assert_check_refuses(
            "delivered",
            &[
                ("intent", text("continue")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
            ],
            "status = 'rejected' OR intent <> 'continue'",
        );
    }

    #[test]
    fn check_refuses_a_reply_tool_row_that_is_not_a_reply() {
        assert_check_refuses(
            "rejected",
            &[("tool", text("reply"))],
            "tool <> 'reply' OR intent = 'reply'",
        );
    }

    // Defect: an answer bypasses the reply tool's open-handle rules.
    #[test]
    fn check_refuses_an_accepted_send_that_replies() {
        assert_check_refuses(
            "delivered",
            &[
                ("intent", text("reply")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
            ],
            "tool <> 'send_message' OR status = 'rejected' OR intent IN ('new', 'steer')",
        );
    }

    fn start_session() -> [(&'static str, Sql); 3] {
        [
            ("tool", text("start_session")),
            ("to_mono_id", Sql::Null),
            ("to_session_id", text("lead-1")),
        ]
    }

    // Defect: a started session's first prompt waits for an answer or names
    // a target, which no session start can do.
    #[test]
    fn check_refuses_a_start_session_row_that_waits_or_steers() {
        assert_check_refuses(
            "open",
            &start_session(),
            "tool <> 'start_session' OR status = 'rejected' OR (intent = 'new' AND wanting_reply = 0 AND to_session_id IS NOT NULL)",
        );
        assert_check_refuses(
            "rejected",
            &with(start_session().to_vec(), &[("intent", text("steer"))]),
            "tool IN ('send_message', 'reply') OR intent = 'new'",
        );
    }

    // Defect: an app action refusal is stored as accepted.
    #[test]
    fn check_refuses_an_accepted_app_action() {
        assert_check_refuses(
            "delivered",
            &[("tool", text("app_action"))],
            "tool <> 'app_action' OR status = 'rejected'",
        );
    }

    // Defect: a steer opens a turn of new work instead of joining the
    // running one (design 16.6).
    #[test]
    fn check_refuses_a_steer_that_opened_a_turn() {
        assert_check_refuses(
            "delivered",
            &[
                ("intent", text("steer")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
                ("delivery", text("submit")),
            ],
            "intent <> 'steer' OR status = 'rejected' OR delivery IN ('queue', 'steer')",
        );
    }

    #[test]
    fn one_call_cannot_write_the_same_part_twice() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(&conn, &row("delivered")).unwrap();
        let reply = with(
            row("delivered"),
            &[
                ("id", text("m_bbbbbbbbbbbb")),
                ("request_id", text("reply-1")),
                ("tool", text("reply")),
                ("intent", text("reply")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
            ],
        );
        raw_insert(&conn, &reply).unwrap();
        let again = with(reply, &[("id", text("m_cccccccccccc"))]);
        let error = raw_insert(&conn, &again).unwrap_err().to_string();
        assert!(
            error.contains(
                "UNIQUE constraint failed: mono_messages.from_session_id, mono_messages.request_id, mono_messages.part"
            ),
            "{error}"
        );
        raw_insert(&conn, &with(again, &[("part", Sql::Integer(1))])).unwrap();
    }

    #[test]
    fn check_refuses_a_second_part_of_a_send() {
        assert_check_refuses(
            "delivered",
            &[("part", Sql::Integer(3))],
            "part = 0 OR (tool = 'reply' AND status <> 'rejected')",
        );
    }

    #[test]
    fn check_refuses_a_second_part_of_a_rejected_reply() {
        assert_check_refuses(
            "rejected",
            &[
                ("tool", text("reply")),
                ("intent", text("reply")),
                ("part", Sql::Integer(2)),
            ],
            "part = 0 OR (tool = 'reply' AND status <> 'rejected')",
        );
    }

    #[test]
    fn check_refuses_an_accepted_reply_at_hop_0() {
        assert_check_refuses(
            "delivered",
            &[
                ("tool", text("reply")),
                ("intent", text("reply")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
            ],
            "status = 'rejected' OR (reply_to IS NULL) = (hop = 0)",
        );
    }

    #[test]
    fn check_refuses_an_accepted_send_with_a_hop_but_no_thread() {
        assert_check_refuses(
            "delivered",
            &[("hop", Sql::Integer(3))],
            "status = 'rejected' OR (reply_to IS NULL) = (hop = 0)",
        );
    }

    #[test]
    fn check_refuses_a_message_id_outside_base32_lower_case() {
        assert_check_refuses(
            "delivered",
            &[("id", text("m_ABCDEFGHIJKL"))],
            "id GLOB 'm_[a-z2-7]",
        );
    }

    #[test]
    fn check_refuses_a_payload_hash_that_is_not_hex() {
        assert_check_refuses(
            "delivered",
            &[("payload_hash", text(&"A".repeat(64)))],
            "payload_hash NOT GLOB '*[^0-9a-f]*'",
        );
    }

    #[test]
    fn check_refuses_a_request_id_with_a_space() {
        assert_check_refuses(
            "delivered",
            &[("request_id", text("req 2"))],
            "request_id NOT GLOB '*[^A-Za-z0-9_-]*'",
        );
    }

    #[test]
    fn check_refuses_a_rejected_row_whose_body_chars_disagree_with_its_body() {
        assert_check_refuses(
            "rejected",
            &[("body_chars", Sql::Integer(500))],
            "length(body) = min(body_chars, 8000)",
        );
    }

    #[test]
    fn check_refuses_too_large_for_a_body_within_the_limit() {
        assert_check_refuses(
            "rejected",
            &[("gate_result", text("too_large"))],
            "(gate_result = 'too_large') = (body_chars > 8000)",
        );
    }

    #[test]
    fn a_too_large_row_keeps_8000_characters_and_the_real_length() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        raw_insert(
            &conn,
            &with(
                row("rejected"),
                &[
                    ("gate_result", text("too_large")),
                    ("body", text(&"é".repeat(8000))),
                    ("body_chars", Sql::Integer(9000)),
                ],
            ),
        )
        .unwrap();
    }

    pub(crate) fn messaging_store() -> SessionStore {
        let store = SessionStore::open_in_memory().unwrap();
        seed(&store.lock_conn().unwrap());
        store
    }

    /// The Giga `giga` (chat `giga-chat`), the Mono `mono-a` (chat `a-chat`)
    /// and the project session `project`, all on codex.
    fn seed(conn: &Connection) {
        conn.execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('giga', 'Boss', 'giga', '{"sessionId":"giga-chat"}', 0, 1, 1),
                      ('mono-a', 'A', 'mono', '{"sessionId":"a-chat"}', 1, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('giga-chat', '/', 'codex', 'm', 'default', 'G', 1, 1),
                      ('a-chat', '/', 'codex', 'm', 'default', 'A', 1, 1),
                      ('project', '/p', 'codex', 'm', 'default', 'P', 1, 1);"#,
        )
        .unwrap();
    }

    /// A Mono chat's call bound to turn `turn-1`.
    fn caller_in(mono_id: &str, session_id: &str) -> Caller {
        caller_in_turn(mono_id, session_id, "turn-1")
    }

    fn caller_in_turn(mono_id: &str, session_id: &str, turn_id: &str) -> Caller {
        Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::MonoChat,
                mono_id: mono(mono_id),
            },
            session_id: session(session_id),
            turn_id: turn_id.into(),
        }
    }

    fn call(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        live: &[LiveSession],
    ) -> Submitted {
        call_with(conn, caller, request_id, tool, input, live, &[])
    }

    /// A call while the sessions in `running` run turn `turn-1`. Every test
    /// session runs on codex, which can steer.
    fn call_with(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        live: &[LiveSession],
        running: &[&str],
    ) -> Submitted {
        let running: Vec<(&str, &str)> = running.iter().map(|id| (*id, "turn-1")).collect();
        call_in_turns(conn, caller, request_id, tool, input, live, &running)
    }

    /// A call while each `(session, turn)` of `running` runs that turn.
    fn call_in_turns(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        live: &[LiveSession],
        running: &[(&str, &str)],
    ) -> Submitted {
        call_in_turns_at(conn, caller, request_id, tool, input, live, running, 100)
    }

    #[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
    fn call_in_turns_at(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        live: &[LiveSession],
        running: &[(&str, &str)],
        now: i64,
    ) -> Submitted {
        let running: HashMap<String, String> = running
            .iter()
            .map(|(id, turn)| (id.to_string(), turn.to_string()))
            .collect();
        submit(
            conn,
            caller,
            &RequestId::parse(request_id).unwrap(),
            mono_gate::parse(tool, &input),
            &Live {
                sessions: live,
                running: &running,
                steerable_harnesses: &["codex".to_string()],
            },
            now,
        )
        .unwrap()
    }

    fn listed(id: &str, steerable: bool, own_project: bool) -> LiveSession {
        LiveSession {
            session_id: id.into(),
            steerable,
            own_project,
        }
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM mono_messages", [], |row| row.get(0))
            .unwrap()
    }

    fn accepted_ids(submitted: &Submitted) -> Vec<MessageId> {
        match submitted {
            Submitted::Accepted { messages, .. } => {
                messages.iter().map(|row| row.message.id.clone()).collect()
            }
            Submitted::Failed { error } => panic!("refused: {error:?}"),
        }
    }

    fn failed(submitted: Submitted) -> ToolError {
        match submitted {
            Submitted::Failed { error } => error,
            accepted => panic!("accepted: {accepted:?}"),
        }
    }

    /// A session caller with `started` access in `cwd`, bound to turn
    /// `turn-1`. Its level comes from its `sessions` row.
    fn session_caller(session_id: &str, cwd: &str) -> Caller {
        Caller {
            speaker: Speaker::Session {
                access: Access::Started,
                cwd: cwd.into(),
            },
            session_id: session(session_id),
            turn_id: "turn-1".into(),
        }
    }

    /// `mono-a` works on `/p`, where the lead `lead` and its worker `worker`
    /// run.
    /// Seeded lead `lead` sends `report` to Mono `mono-a`.
    fn lead_reports(conn: &Connection) -> (Caller, MessageId) {
        let lead = session_caller("lead", "/p");
        let report = accepted_ids(&call(
            conn,
            &lead,
            "report",
            Tool::SendMessage,
            hi("mono-a", false),
            &[],
        ))
        .remove(0);
        (lead, report)
    }

    fn seed_levels(conn: &Connection) {
        conn.execute_batch(
            r#"UPDATE monos SET data_json = '{"sessionId":"a-chat","projects":["/p"]}'
                WHERE id = 'mono-a';
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at,
                                     updated_at, lead_session_id)
               VALUES ('lead', '/p', 'codex', 'm', 'default', 'L', 1, 1, NULL),
                      ('worker', '/p', 'codex', 'm', 'default', 'W', 1, 1, 'lead');"#,
        )
        .unwrap();
    }

    // Defect: U+0000 in a body reaches the insert, whose length CHECK fails:
    // the agent gets a raw SQLite error with no code, and a gate refusal of
    // that body loses its rejected row.
    #[test]
    fn a_message_with_u0000_is_invalid_input_and_writes_nothing() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        ask_delivered(&conn, "ask", "turn-1");
        let before = rows(&conn);
        for (caller, tool, input) in [
            (
                caller_in("giga", "giga-chat"),
                Tool::SendMessage,
                serde_json::json!({
                    "message": "Ship\u{0}it", "send_to": "mono-a", "wanting_reply": false, "intent": "new"
                }),
            ),
            (
                session_caller("worker", "/p"),
                Tool::SendMessage,
                serde_json::json!({
                    "message": "Ship\u{0}it", "send_to": "lead", "wanting_reply": false, "intent": "new"
                }),
            ),
            (
                caller_in("mono-a", "a-chat"),
                Tool::Reply,
                serde_json::json!({"message": "Ye\u{0}s", "to": "#1"}),
            ),
        ] {
            let error = failed(call(&conn, &caller, "nul", tool, input, &[]));
            assert_eq!(error.code, "invalid_input");
            assert!(error.hint.contains("U+0000"), "{}", error.hint);
        }
        assert_eq!(rows(&conn), before);
    }

    // Decision 617, PR 7 review r1 M2 and m7, PR7-5b review r1 m1. Defect:
    // `access` "started" parses as Operator, so A8 never binds an
    // app-started lead; a session caller with no or an unknown `access`
    // parses (as Operator it fails open from PR 9 on), or with no or an
    // empty `cwd`; R4: a caller that names a lead (`leadSessionId`) parses,
    // so an agent's claim could reach the gate; or it reads as
    // a lead; or a Mono caller with session fields (any of them, review r2
    // n1) or a session caller with a Mono parses.
    #[test]
    fn caller_arg_parses_a_session_caller_and_refuses_mixed_combinations() {
        let parse =
            |value: serde_json::Value| serde_json::from_value::<CallerArg>(value).unwrap().parse();
        for (access, parsed) in [("started", Access::Started), ("operator", Access::Operator)] {
            assert_eq!(
                parse(serde_json::json!({
                    "kind": "session", "sessionId": "lead", "turnId": "turn-1",
                    "access": access, "cwd": "/p"
                }))
                .unwrap(),
                Caller {
                    speaker: Speaker::Session {
                        access: parsed,
                        cwd: "/p".into(),
                    },
                    session_id: session("lead"),
                    turn_id: "turn-1".into(),
                }
            );
        }
        for (case, field, value, error) in [
            ("no access", "access", None, "Invalid caller access"),
            (
                "unknown access",
                "access",
                Some("admin"),
                "Invalid caller access",
            ),
            ("no cwd", "cwd", None, "Invalid caller cwd"),
            ("empty cwd", "cwd", Some(""), "Invalid caller cwd"),
        ] {
            let mut arg = serde_json::json!({
                "kind": "session", "sessionId": "lead", "turnId": "turn-1",
                "access": "started", "cwd": "/p"
            });
            match value {
                Some(value) => arg[field] = serde_json::json!(value),
                None => {
                    arg.as_object_mut().unwrap().remove(field);
                }
            }
            assert_eq!(parse(arg), Err(error.into()), "session caller with {case}");
        }
        for (field, value) in [("cwd", "/p"), ("access", "started")] {
            let mut arg = serde_json::json!({
                "kind": "mono_chat", "monoId": "giga", "sessionId": "giga-chat",
                "turnId": "turn-1"
            });
            arg[field] = serde_json::json!(value);
            assert!(parse(arg).is_err(), "Mono caller with {field}");
        }
        assert!(
            serde_json::from_value::<CallerArg>(serde_json::json!({
                "kind": "session", "sessionId": "worker", "turnId": "turn-1",
                "access": "started", "cwd": "/p", "leadSessionId": "other"
            }))
            .is_err(),
            "a caller that names its own lead"
        );
        assert!(
            parse(serde_json::json!({
                "kind": "session", "monoId": "giga", "sessionId": "lead",
                "turnId": "turn-1", "access": "started", "cwd": "/p"
            }))
            .is_err(),
            "session caller with monoId"
        );
    }

    // Design 13.4, 13.5 and C8. Defect: the row of a lead's report to its
    // Mono names a Mono, so a drop gives that Mono a notice about a message
    // it never sent.
    #[test]
    fn a_session_callers_send_has_no_mono_and_its_drop_owes_no_notice() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let (_, report) = lead_reports(&conn);
        let row = get(&conn, &report).unwrap().unwrap();
        assert_eq!(
            (row.from_mono_id, row.from_session_id),
            (None, session("lead"))
        );
        assert_eq!(clear(&conn, "a-chat", 200), Notices::default());
        assert_eq!(stored_row(&conn, &report).0, "dropped");
        for chat in ["a-chat", "giga-chat"] {
            assert!(
                pending_notices(&conn, &session(chat)).unwrap().is_empty(),
                "{chat}"
            );
        }
    }

    // L3 (design 6.3) with design 6.1 keys, decision 728. Defect: L3
    // compares the lead's cwd with the Mono's projects as written, so a lead
    // in "/p/" cannot report to the Mono of "/p", nor a lead in "/p" to a
    // Mono whose project reads "/p//".
    #[test]
    fn l3_compares_the_leads_cwd_and_the_monos_projects_by_path_key() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        accepted_ids(&call(
            &conn,
            &session_caller("lead", "/p/"),
            "slash-cwd",
            Tool::SendMessage,
            hi("mono-a", false),
            &[],
        ));
        conn.execute(
            r#"UPDATE monos SET data_json = '{"sessionId":"a-chat","projects":["/p//"]}'
                WHERE id = 'mono-a'"#,
            [],
        )
        .unwrap();
        accepted_ids(&call(
            &conn,
            &session_caller("lead", "/p"),
            "slash-project",
            Tool::SendMessage,
            hi("mono-a", false),
            &[],
        ));
    }

    // Design 6.1 "shares project", review r1 n3. Defect: Monos' projects are
    // compared as written, so two Monos on one folder written two ways cannot
    // talk; or an empty project keys to "/", so two Monos that each hold ""
    // share it.
    #[test]
    fn s8_compares_the_projects_of_two_monos_by_path_key() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            r#"UPDATE monos SET data_json = '{"sessionId":"a-chat","projects":["/w/app/",""]}'
                WHERE id = 'mono-a';
               INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat","projects":["/w/app"]}', 2, 1, 1),
                      ('mono-c', 'C', 'mono', '{"sessionId":"c-chat","projects":["/w/other",""]}', 3, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1),
                      ('c-chat', '/', 'codex', 'm', 'default', 'C', 1, 1);"#,
        )
        .unwrap();
        let a = caller_in("mono-a", "a-chat");
        accepted_ids(&call(
            &conn,
            &a,
            "to-b",
            Tool::SendMessage,
            hi("mono-b", false),
            &[],
        ));
        let to_c = failed(call(
            &conn,
            &a,
            "to-c",
            Tool::SendMessage,
            hi("mono-c", false),
            &[],
        ));
        assert_eq!(to_c.code, "direction_forbidden");
    }

    // Design 6.1 "allowlisted". Defect: `messageAllowlist` is not read, so
    // two Monos that the user let talk are refused for sharing no project.
    #[test]
    fn s8_lets_two_monos_talk_when_one_allowlists_the_other() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            r#"UPDATE monos SET data_json = '{"sessionId":"a-chat","projects":["/a"]}'
                WHERE id = 'mono-a';
               INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-b', 'B', 'mono',
                       '{"sessionId":"b-chat","projects":["/b"],"messageAllowlist":["mono-a"]}', 2, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1);"#,
        )
        .unwrap();
        accepted_ids(&call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "to-b",
            Tool::SendMessage,
            hi("mono-b", false),
            &[],
        ));
        accepted_ids(&call(
            &conn,
            &caller_in("mono-b", "b-chat"),
            "to-a",
            Tool::SendMessage,
            hi("mono-a", false),
            &[],
        ));
    }

    // Design 16.4, decision 506. Defect: a session caller cannot read the
    // message it sent, so the id its send returned names nothing.
    #[test]
    fn get_message_lets_a_session_caller_read_a_row_it_sent() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let (lead, report) = lead_reports(&conn);
        let read_back = found(read(&conn, &lead, report.as_str()));
        assert_eq!(read_back["message_id"], report.as_str());
        assert_eq!(read_back["from"], serde_json::json!({"session_id": "lead"}));
        assert_eq!(
            read_back["to"],
            serde_json::json!({"mono_id": "mono-a", "mono_name": "A"})
        );
    }

    // Design 16.4, decision 506. Defect: a lead cannot read a message its
    // session took, or another session reads it.
    #[test]
    fn get_message_lets_a_session_caller_read_a_row_its_session_handled() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let brief = accepted_ids(&call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "brief",
            Tool::SendMessage,
            hi("lead", false),
            &[listed("lead", true, true)],
        ))
        .remove(0);
        let read_back = found(read(&conn, &session_caller("lead", "/p"), brief.as_str()));
        assert_eq!(
            read_back["from"],
            serde_json::json!({"mono_id": "mono-a", "mono_name": "A"})
        );
        let other = read_error(read(&conn, &session_caller("worker", "/p"), brief.as_str()));
        assert_eq!(other.code, "unknown_message");
    }

    // Design 13.1, R4. Defect: a worker whose lead was deleted (which cleared
    // its stored link) is still refused as a worker; or a caller that
    // `sessions` lacks gets a level anyway (a lead's, or one it claims).
    #[test]
    fn a_session_caller_is_judged_by_its_stored_row_and_refused_without_one() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        conn.execute("DELETE FROM sessions WHERE id = 'lead'", [])
            .unwrap();
        let report = |caller: &Caller| {
            call(
                &conn,
                caller,
                "report",
                Tool::SendMessage,
                hi("mono-a", false),
                &[],
            )
        };
        let before = rows(&conn);
        accepted_ids(&report(&session_caller("worker", "/p")));
        let unpersisted = failed(report(&session_caller("remote", "/p")));
        assert_eq!(
            (unpersisted.code, unpersisted.message_id),
            ("unknown_caller", None)
        );
        assert_eq!(rows(&conn), before + 1, "the refusal writes no row");
    }

    // Design 13.1, R4. Defect: a recipient's level comes from anything but
    // its stored row, so a Mono messages a worker past its lead, or a session
    // with no row is judged by a lead it never stored.
    #[test]
    fn a_recipients_level_comes_only_from_its_stored_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let giga = caller_in("giga", "giga-chat");
        let live = |session_id: &str| listed(session_id, true, true);
        let to_worker = failed(call(
            &conn,
            &giga,
            "worker",
            Tool::SendMessage,
            hi("worker", false),
            &[live("worker")],
        ));
        assert_eq!(to_worker.code, "steer_via_lead");
        // A listed session with no row has no stored lead link: a lead.
        accepted_ids(&call(
            &conn,
            &giga,
            "remote",
            Tool::SendMessage,
            hi("remote", false),
            &[live("remote")],
        ));
        accepted_ids(&call(
            &conn,
            &giga,
            "lead",
            Tool::SendMessage,
            hi("lead", false),
            &[live("lead")],
        ));
    }

    // Design 6.1 (levels) and 13.5. Defect: the rate count keys a session
    // caller on `from_mono_id`, which its rows leave NULL, so a worker
    // floods its lead without a limit.
    #[test]
    fn a_workers_seventh_send_to_its_lead_in_ten_minutes_is_rate_limited() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let worker = session_caller("worker", "/p");
        let report = |request_id: &str| {
            call(
                &conn,
                &worker,
                request_id,
                Tool::SendMessage,
                hi("lead", false),
                &[listed("lead", true, true)],
            )
        };
        for request_id in ["r1", "r2", "r3", "r4", "r5", "r6"] {
            accepted_ids(&report(request_id));
        }
        assert_eq!(failed(report("r7")).code, "rate_limited");
    }

    // Design 6.1 (levels) and 13.5. Defect: the rate count of a session
    // caller keys on `from_mono_id IS NULL` alone, so every session of no
    // Mono shares one count, and one worker's flood silences another.
    #[test]
    fn two_sessions_of_no_mono_keep_separate_rate_counts() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let report = |worker: &Caller, request_id: &str| {
            call(
                &conn,
                worker,
                request_id,
                Tool::SendMessage,
                hi("lead", false),
                &[listed("lead", true, true)],
            )
        };
        let worker = session_caller("worker", "/p");
        for request_id in ["r1", "r2", "r3", "r4", "r5", "r6"] {
            accepted_ids(&report(&worker, request_id));
        }
        conn.execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at,
                                   updated_at, lead_session_id)
             VALUES ('remote', '/p', 'codex', 'm', 'default', 'R', 1, 1, 'lead')",
            [],
        )
        .unwrap();
        accepted_ids(&report(&session_caller("remote", "/p"), "r1"));
        assert_eq!(failed(report(&worker, "r7")).code, "rate_limited");
    }

    #[test]
    fn the_same_request_twice_writes_one_row_and_answers_duplicate() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let input = serde_json::json!({"message": "Ship it", "send_to": "mono-a", "wanting_reply": false, "intent": "new"});
        let first = call(&conn, &giga, "r1", Tool::SendMessage, input.clone(), &[]);
        let Submitted::Accepted {
            messages,
            duplicate: false,
            ..
        } = &first
        else {
            panic!("first call: {first:?}");
        };
        let again = call(&conn, &giga, "r1", Tool::SendMessage, input, &[]);
        assert_eq!(
            again,
            Submitted::Accepted {
                messages: messages.clone(),
                answered: Vec::new(),
                duplicate: true,
                routes: Vec::new(),
            }
        );
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn a_request_id_reused_with_other_input_is_refused_without_a_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let send = |message: &str| serde_json::json!({"message": message, "send_to": "mono-a", "wanting_reply": false, "intent": "new"});
        accepted_ids(&call(
            &conn,
            &giga,
            "r1",
            Tool::SendMessage,
            send("Ship it"),
            &[],
        ));
        let error = failed(call(
            &conn,
            &giga,
            "r1",
            Tool::SendMessage,
            send("Ship it now"),
            &[],
        ));
        assert_eq!((error.code, error.message_id), ("request_id_reused", None));
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn a_session_in_mono_session_ends_is_gone_even_when_the_live_list_has_it() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            "INSERT INTO mono_session_ends (session_id, ended_at) VALUES ('project', 1), ('a-chat', 1);",
        )
        .unwrap();
        let live = [
            LiveSession {
                session_id: "project".into(),
                steerable: true,
                own_project: true,
            },
            LiveSession {
                session_id: "a-chat".into(),
                steerable: true,
                own_project: false,
            },
        ];
        let giga = caller_in("giga", "giga-chat");
        let send = |to: &str| serde_json::json!({"message": "Hi", "send_to": to, "wanting_reply": false, "intent": "new"});
        let project = failed(call(
            &conn,
            &giga,
            "r1",
            Tool::SendMessage,
            send("project"),
            &live,
        ));
        assert_eq!(project.code, "unknown_target");
        let chat = failed(call(
            &conn,
            &giga,
            "r2",
            Tool::SendMessage,
            send("mono-a"),
            &live,
        ));
        assert_eq!(chat.code, "target_no_conversation");
        let stored = get(&conn, &chat.message_id.unwrap()).unwrap().unwrap();
        assert!(matches!(
            stored.status,
            Status::Rejected {
                gate_result: RejectCode::TargetNoConversation,
                ..
            }
        ));
    }

    #[test]
    fn a_rejected_call_made_again_returns_its_row_marked_duplicate() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let input = serde_json::json!({"message": "Hi", "send_to": "nope", "wanting_reply": false, "intent": "new"});
        let first = failed(call(
            &conn,
            &giga,
            "r1",
            Tool::SendMessage,
            input.clone(),
            &[],
        ));
        let again = failed(call(&conn, &giga, "r1", Tool::SendMessage, input, &[]));
        assert_eq!((first.code, first.duplicate), ("unknown_target", false));
        assert_eq!(
            (again.code, again.message_id, again.duplicate),
            ("unknown_target", first.message_id, true)
        );
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn an_accepted_reply_answers_the_waiting_message_and_goes_to_its_sender() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let ask = accepted_ids(&call(
            &conn,
            &giga,
            "ask",
            Tool::SendMessage,
            serde_json::json!({"message": "Is CI green?", "send_to": "mono-a", "wanting_reply": true, "intent": "new"}),
            &[],
        ))
        .remove(0);
        assert!(
            assign_slot(&conn, &ask, "turn-1", "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .is_some()
        );
        mark_delivery(&conn, &ask, "turn-1", 102).unwrap();
        let mono_a = caller_in("mono-a", "a-chat");
        let answer = call(
            &conn,
            &mono_a,
            "answer",
            Tool::Reply,
            serde_json::json!({"message": "Yes"}),
            &[LiveSession {
                session_id: "giga-chat".into(),
                steerable: true,
                own_project: false,
            }],
        );
        let Submitted::Accepted {
            messages,
            answered,
            duplicate: false,
            ..
        } = answer
        else {
            panic!("reply: {answer:?}");
        };
        let [Targeted { message: reply, .. }] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(
            (reply.status.intent(), reply.hop, &reply.to),
            (
                Some(&Intent::Reply(ask.clone())),
                1,
                &Some(Party::Mono {
                    mono_id: mono("giga")
                })
            )
        );
        assert!(matches!(
            &reply.status,
            Status::Delivered { routed, .. } if routed.handled_by_session == session("giga-chat")
        ));
        assert!(matches!(
            get(&conn, &ask).unwrap().unwrap().status,
            Status::Answered { closed_at: 100, .. }
        ));
        assert_eq!(
            answered,
            vec![Answered {
                handle: Handle::new(1),
                message_id: ask
            }]
        );
    }

    fn sent_to_a(conn: &Connection, request_id: &str, wanting_reply: bool) -> MessageId {
        sent(
            conn,
            ("giga", "giga-chat"),
            "mono-a",
            request_id,
            wanting_reply,
        )
    }

    /// A message `Hi` from the Mono `from` (Mono id, chat) to the Mono `to`.
    pub(crate) fn sent(
        conn: &Connection,
        from: (&str, &str),
        to: &str,
        request_id: &str,
        wanting_reply: bool,
    ) -> MessageId {
        accepted_ids(&call(
            conn,
            &caller_in(from.0, from.1),
            request_id,
            Tool::SendMessage,
            serde_json::json!({"message": "Hi", "send_to": to, "wanting_reply": wanting_reply, "intent": "new"}),
            &[],
        ))
        .remove(0)
    }

    fn slot_turn(conn: &Connection, id: &MessageId) -> Option<String> {
        conn.query_row(
            "SELECT turn_id FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn assign_slot_refuses_a_row_that_holds_another_turns_slot_and_keeps_that_slot() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let id = sent_to_a(&conn, "r1", true);
        assert!(
            assign_slot(&conn, &id, "turn-1", "main", Delivery::Steer, 101)
                .unwrap()
                .0
                .is_some()
        );
        assert!(
            assign_slot(&conn, &id, "turn-1", "main", Delivery::Steer, 102)
                .unwrap()
                .0
                .is_some()
        );
        assert!(
            assign_slot(&conn, &id, "turn-2", "main", Delivery::Submit, 103)
                .unwrap()
                .0
                .is_none()
        );
        assert_eq!(slot_turn(&conn, &id).as_deref(), Some("turn-1"));
        release(&conn, &id, "turn-2", 104).unwrap();
        assert_eq!(slot_turn(&conn, &id).as_deref(), Some("turn-1"));
        release(&conn, &id, "turn-1", 105).unwrap();
        assert_eq!(slot_turn(&conn, &id), None);
        assert!(
            assign_slot(&conn, &id, "turn-2", "main", Delivery::Submit, 106)
                .unwrap()
                .0
                .is_some()
        );
    }

    #[test]
    fn a_row_the_provider_took_keeps_its_slot_and_is_never_handed_off_again() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let id = sent_to_a(&conn, "r1", false);
        assert!(
            assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .is_some()
        );
        // Another turn's acceptance is not this hand-off's.
        mark_delivery(&conn, &id, "turn-2", 102).unwrap();
        assert!(matches!(
            get(&conn, &id).unwrap().unwrap().status,
            Status::Delivered {
                slot: Some(PlainSlot {
                    delivered_at: None,
                    ..
                }),
                ..
            }
        ));
        mark_delivery(&conn, &id, "turn-1", 102).unwrap();
        release(&conn, &id, "turn-1", 103).unwrap();
        assert_eq!(slot_turn(&conn, &id).as_deref(), Some("turn-1"));
        assert!(
            assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 104)
                .unwrap()
                .0
                .is_none()
        );
        assert!(matches!(
            get(&conn, &id).unwrap().unwrap().status,
            Status::Delivered {
                slot: Some(PlainSlot {
                    delivered_at: Some(102),
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn unqueued_lists_the_rows_of_a_session_that_no_slot_holds() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let held = sent_to_a(&conn, "r1", true);
        let waiting = sent_to_a(&conn, "r2", false);
        let dropped = sent_to_a(&conn, "r3", false);
        conn.execute(
            "UPDATE mono_messages SET status = 'dropped', closed_at = 1 WHERE id = ?1",
            [dropped.as_str()],
        )
        .unwrap();
        assert!(
            assign_slot(&conn, &held, "turn-1", "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .is_some()
        );
        let listed: Vec<MessageId> = unqueued(&conn, &session("a-chat"))
            .unwrap()
            .into_iter()
            .map(|row| row.message.id)
            .collect();
        assert_eq!(listed, vec![waiting]);
        assert!(unqueued(&conn, &session("giga-chat")).unwrap().is_empty());
    }

    fn ask_delivered(conn: &Connection, request_id: &str, turn_id: &str) -> MessageId {
        handed_off(conn, request_id, true, turn_id, "main", true)
    }

    /// A message from the Giga to `a-chat`, handed off as [`hand_off`] does.
    pub(crate) fn handed_off(
        conn: &Connection,
        request_id: &str,
        wanting_reply: bool,
        turn_id: &str,
        window: &str,
        taken: bool,
    ) -> MessageId {
        let id = sent_to_a(conn, request_id, wanting_reply);
        hand_off(conn, &id, turn_id, window, taken);
        id
    }

    /// Hands message `id` off as the opener of turn `turn_id` in window
    /// `window`; `taken` when its provider took the text.
    pub(crate) fn hand_off(
        conn: &Connection,
        id: &MessageId,
        turn_id: &str,
        window: &str,
        taken: bool,
    ) {
        assert!(
            assign_slot(conn, id, turn_id, window, Delivery::Submit, 101)
                .unwrap()
                .0
                .is_some()
        );
        if taken {
            mark_delivery(conn, id, turn_id, 102).unwrap();
        }
    }

    fn stored_delivery(conn: &Connection, id: &MessageId) -> Option<String> {
        conn.query_row(
            "SELECT delivery FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn hi(to: &str, wanting_reply: bool) -> serde_json::Value {
        serde_json::json!({"message": "Hi", "send_to": to, "wanting_reply": wanting_reply, "intent": "new"})
    }

    fn steering(to: &str, reply_to: &str) -> serde_json::Value {
        serde_json::json!({"message": "Stop", "send_to": to, "wanting_reply": false, "intent": "steer", "reply_to": reply_to})
    }

    /// A message from the Giga to `project` that turn `turn-1` of `project`
    /// opened with, so a steer can name its running work.
    fn opened_project(conn: &Connection, request_id: &str) -> MessageId {
        let id = accepted_ids(&call(
            conn,
            &caller_in("giga", "giga-chat"),
            request_id,
            Tool::SendMessage,
            hi("project", false),
            &[listed("project", true, true)],
        ))
        .remove(0);
        hand_off(conn, &id, "turn-1", "main", true);
        id
    }

    fn yes() -> serde_json::Value {
        serde_json::json!({"message": "Yes"})
    }

    /// The route of a call that goes to one session, if it routes.
    fn route_of(submitted: &Submitted) -> Option<Routing> {
        match submitted {
            Submitted::Accepted { routes, .. } => match routes.as_slice() {
                [] => None,
                [group] => Some(group.route.clone()),
                groups => panic!("several routes: {groups:?}"),
            },
            Submitted::Failed { error } => panic!("refused: {error:?}"),
        }
    }

    // Review PR 4a M5. Defect: a reply answers a message whose hand-off holds
    // a slot but whose text the provider never took.
    #[test]
    fn rv_submit_reply_open_set_needs_delivered_at() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let ask = sent_to_a(&conn, "ask", true);
        // A hand-off holds the slot; the provider has not taken the text.
        assert!(
            assign_slot(&conn, &ask, "turn-1", "main", Delivery::Steer, 101)
                .unwrap()
                .0
                .is_some()
        );
        let error = failed(call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "answer",
            Tool::Reply,
            yes(),
            &[],
        ));
        assert_eq!(error.code, "no_open_handle");
    }

    // Review PR 4a M7. Defect: a Mono messages or steers a session that is
    // loaded in its window but is not in one of its projects.
    #[test]
    fn a_send_to_a_listed_session_outside_the_callers_projects_is_direction_forbidden() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let error = failed(call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            steering("project", "m_aaaaaaaaaaaa"),
            &[listed("project", true, false)],
        ));
        assert_eq!(error.code, "direction_forbidden");
    }

    // Review PR 4a M7. Defect: a reply from one Mono answers the question
    // that another Mono was asked.
    #[test]
    fn a_reply_cannot_answer_a_message_another_mono_was_asked() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let ask = ask_delivered(&conn, "ask", "turn-1");
        let error = failed(call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "answer",
            Tool::Reply,
            yes(),
            &[],
        ));
        assert_eq!(
            (error.code, error.open_handles),
            ("no_open_handle", Some(Vec::new()))
        );
        assert!(matches!(
            get(&conn, &ask).unwrap().unwrap().status,
            Status::Open { .. }
        ));
    }

    // Review PR 4a m1. Defect: a running chat that the caller's window has
    // not loaded counts as unable to steer, so a steer to it is refused and a
    // reply to its sender waits for the turn end.
    #[test]
    fn a_session_this_window_has_not_loaded_steers_by_its_harness() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = handed_off(&conn, "work", false, "turn-1", "main", true);
        let steer = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        );
        assert_eq!(
            route_of(&steer).map(|route| route.via),
            Some(Delivery::Steer)
        );
        ask_delivered(&conn, "ask", "turn-1");
        let reply = call_with(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "answer",
            Tool::Reply,
            yes(),
            &[],
            &["giga-chat"],
        );
        assert_eq!(
            route_of(&reply).map(|route| route.via),
            Some(Delivery::Steer)
        );
        conn.execute("UPDATE sessions SET harness = 'fx' WHERE id = 'a-chat'", [])
            .unwrap();
        let refused = failed(call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r2",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        ));
        assert_eq!(refused.code, "cannot_steer");
    }

    // Review PR 4a decision 167. Defect: a row claims a steer before any
    // hand-off steered it, or the caller loses the route it must run.
    #[test]
    fn an_accepted_row_waits_as_queue_and_the_call_reports_its_route() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = handed_off(&conn, "work", false, "turn-1", "main", true);
        let outcome = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        );
        let id = accepted_ids(&outcome).remove(0);
        assert_eq!(stored_delivery(&conn, &id).as_deref(), Some("queue"));
        assert_eq!(
            route_of(&outcome),
            Some(Routing {
                session_id: session("a-chat"),
                via: Delivery::Steer
            })
        );
        let again = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        );
        assert_eq!(route_of(&again), None);
    }

    // Review PR 4a m7 (M03). Defect: a hand-off takes the slot of a row that
    // is already answered and delivers the question again.
    #[test]
    fn assign_slot_refuses_an_answered_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let id = sent_to_a(&conn, "r1", true);
        conn.execute(
            "UPDATE mono_messages SET status = 'answered', closed_at = 1 WHERE id = ?1",
            [id.as_str()],
        )
        .unwrap();
        assert!(
            assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .is_none()
        );
        assert_eq!(slot_turn(&conn, &id), None);
    }

    /// A call at time `now`, with nothing live and nothing running.
    fn call_at(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        now: i64,
    ) -> Submitted {
        submit(
            conn,
            caller,
            &RequestId::parse(request_id).unwrap(),
            mono_gate::parse(tool, &input),
            &Live {
                sessions: &[],
                running: &HashMap::new(),
                steerable_harnesses: &["codex".to_string()],
            },
            now,
        )
        .unwrap()
    }

    /// `turn_id`, `handle` and `slot_window` as stored.
    fn slot_columns(
        conn: &Connection,
        id: &MessageId,
    ) -> (Option<String>, Option<i64>, Option<String>) {
        conn.query_row(
            "SELECT turn_id, handle, slot_window FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    fn sorted(mut ids: Vec<MessageId>) -> Vec<String> {
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids.into_iter().map(|id| id.as_str().to_string()).collect()
    }

    fn ids(messages: Vec<MonoMessage>) -> Vec<MessageId> {
        messages.into_iter().map(|message| message.id).collect()
    }

    // Design 5.2. Defect: handles count per session, so they grow across
    // turns until the `#99` CHECK fails every later hand-off; or a hand-off
    // that runs again for the same turn gives the row another handle.
    #[test]
    fn handles_count_from_1_in_each_turn_and_a_row_keeps_its_handle() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let handle = |id: &MessageId, turn_id: &str| {
            let row = assign_slot(&conn, id, turn_id, "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .expect("a slot")
                .message;
            match row.status {
                Status::Open {
                    slot: Some(slot), ..
                } => slot.handle.to_string(),
                status => panic!("{status:?}"),
            }
        };
        let first = sent_to_a(&conn, "r1", true);
        let second = sent_to_a(&conn, "r2", true);
        let next_turn = sent_to_a(&conn, "r3", true);
        assert_eq!(handle(&first, "turn-1"), "#1");
        assert_eq!(handle(&second, "turn-1"), "#2");
        assert_eq!(handle(&first, "turn-1"), "#1");
        assert_eq!(handle(&next_turn, "turn-2"), "#1");
    }

    // Design 16.7, 16.12.5, PR I4 row. Defect: a hand-off gets no target, so
    // the steer reaches the agent with no line; the target loses the handle
    // and turn that make `#n` valid; or a new row or a whole-turn steer gets
    // a target it does not name.
    #[test]
    fn assign_slot_answers_the_target_with_its_slot_and_none_for_a_new_row_or_a_whole_turn_steer() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let target = |id: &MessageId, turn_id: &str, delivery| {
            let assigned = assign_slot(&conn, id, turn_id, "main", delivery, 103)
                .unwrap()
                .0
                .expect("a slot");
            serde_json::to_value(&assigned).unwrap()["target"].clone()
        };
        let work = handed_off(&conn, "work", true, "turn-1", "main", true);
        let steer = accepted_ids(&call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "steer",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        ))
        .remove(0);
        assert_eq!(
            target(&steer, "turn-1", Delivery::Steer),
            serde_json::json!({
                "messageId": work.as_str(),
                "body": "Hi",
                "handledBySession": "a-chat",
                "slot": {"turnId": "turn-1", "handle": "#1"},
            })
        );
        let whole = steered_turn(&conn, "whole");
        assert_eq!(
            target(&whole, "turn-1", Delivery::Steer),
            serde_json::Value::Null
        );
        let plain = sent_to_a(&conn, "plain", false);
        assert_eq!(
            target(&plain, "turn-2", Delivery::Submit),
            serde_json::Value::Null
        );
    }

    // PR I5 (PR I4 review r1 m4). Defect: a route or a reload queues a row
    // without the message it names, so its bubble gets no link.
    #[test]
    fn submit_and_unqueued_answer_each_row_with_its_target_and_none_without_one() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = handed_off(&conn, "work", true, "turn-1", "main", true);
        let submitted = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "steer",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        );
        let Submitted::Accepted { messages, .. } = &submitted else {
            panic!("{submitted:?}");
        };
        let wire = serde_json::to_value(messages).unwrap();
        assert_eq!(
            (&wire[0]["target"]["messageId"], &wire[0]["target"]["body"]),
            (&serde_json::json!(work.as_str()), &serde_json::json!("Hi"))
        );
        let steer = messages[0].message.id.clone();
        let whole = steered_turn(&conn, "whole");
        let plain = sent_to_a(&conn, "plain", false);
        let listed: Vec<(MessageId, serde_json::Value)> = unqueued(&conn, &session("a-chat"))
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.message.id.clone(),
                    serde_json::to_value(row).unwrap()["target"]["messageId"].clone(),
                )
            })
            .collect();
        for expected in [
            (steer, serde_json::json!(work.as_str())),
            (whole, serde_json::Value::Null),
            (plain, serde_json::Value::Null),
        ] {
            assert!(listed.contains(&expected), "{expected:?} in {listed:?}");
        }
    }

    // Design 5.3. Defect: a question the agent saw stays open after its
    // turn, so the sender waits for good; or a hand-off whose text the
    // provider never took keeps its slot, so reconcile never queues it again.
    #[test]
    fn a_turn_end_closes_the_questions_its_agent_saw_and_frees_the_rest() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let seen = handed_off(&conn, "seen", true, "turn-1", "main", true);
        let unseen = handed_off(&conn, "unseen", true, "turn-1", "main", false);
        let plain = handed_off(&conn, "plain", false, "turn-1", "main", false);
        let later = handed_off(&conn, "later", true, "turn-2", "main", true);
        let notices = turn_ended(&conn, &session("a-chat"), "turn-1", 200).unwrap();
        assert!(matches!(
            get(&conn, &seen).unwrap().unwrap().status,
            Status::Unanswered {
                closed_at: 200,
                notified_at: None,
                ..
            }
        ));
        for id in [&unseen, &plain] {
            assert_eq!(slot_columns(&conn, id), (None, None, None));
        }
        assert_eq!(
            sorted(
                unqueued(&conn, &session("a-chat"))
                    .unwrap()
                    .into_iter()
                    .map(|row| row.message.id)
                    .collect()
            ),
            sorted(vec![unseen, plain])
        );
        assert!(matches!(
            get(&conn, &later).unwrap().unwrap().status,
            Status::Open {
                slot: Some(WaitingSlot {
                    delivered_at: Some(_),
                    ..
                }),
                ..
            }
        ));
        // The owner of a-chat reconciles it; the Giga's chat is told.
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // Design 5.3, startup. Defect: after a restart, a turn that was running
    // keeps its questions open and its hand-offs held, so their senders wait
    // for good; or a message to a session that is gone waits for a window
    // that never loads it, and its sender is never told.
    #[test]
    fn reopening_the_store_ends_every_turn_and_drops_messages_to_sessions_that_are_gone() {
        let path =
            std::env::temp_dir().join(format!("monocode-mono-startup-{}.db", uuid::Uuid::new_v4()));
        let (seen, unseen, kept, unpersisted, removed) = {
            let store = SessionStore::open(path.clone()).unwrap();
            let conn = store.lock_conn().unwrap();
            seed(&conn);
            let seen = handed_off(&conn, "seen", true, "turn-1", "main", true);
            let unseen = handed_off(&conn, "unseen", true, "turn-1", "main", false);
            let kept = sent_to_a(&conn, "kept", false);
            let to_session = |request_id: &str, id: &str| {
                accepted_ids(&call(
                    &conn,
                    &caller_in("giga", "giga-chat"),
                    request_id,
                    Tool::SendMessage,
                    hi(id, false),
                    &[listed(id, true, true)],
                ))
                .remove(0)
            };
            let unpersisted = to_session("unpersisted", "scratch-session");
            let removed = to_session("removed", "project");
            conn.execute(
                "UPDATE sessions SET worktree_removed = 1 WHERE id = 'project'",
                [],
            )
            .unwrap();
            (seen, unseen, kept, unpersisted, removed)
        };
        let store = SessionStore::open(path.clone()).unwrap();
        let conn = store.lock_conn().unwrap();
        assert!(matches!(
            get(&conn, &seen).unwrap().unwrap().status,
            Status::Unanswered { .. }
        ));
        assert_eq!(slot_columns(&conn, &unseen), (None, None, None));
        assert_eq!(
            sorted(
                unqueued(&conn, &session("a-chat"))
                    .unwrap()
                    .into_iter()
                    .map(|row| row.message.id)
                    .collect()
            ),
            sorted(vec![unseen, kept])
        );
        for id in [&unpersisted, &removed] {
            assert!(matches!(
                get(&conn, id).unwrap().unwrap().status,
                Status::Dropped {
                    notified_at: None,
                    ..
                }
            ));
        }
        assert_eq!(
            sorted(ids(pending_notices(&conn, &session("giga-chat")).unwrap())),
            sorted(vec![seen, unpersisted, removed])
        );
        drop(conn);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    // Design 5.3. Defect: a second mark moves notified_at, or a mark for a
    // status the row does not have marks it, so the pump cannot tell which
    // notice it already enqueued.
    #[test]
    fn mark_notified_marks_a_row_once_and_only_in_the_status_it_names() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let seen = handed_off(&conn, "seen", true, "turn-1", "main", true);
        turn_ended(&conn, &session("a-chat"), "turn-1", 200).unwrap();
        assert!(!mark_notified(&conn, &seen, NoticeStatus::Dropped, 300).unwrap());
        assert!(mark_notified(&conn, &seen, NoticeStatus::Unanswered, 301).unwrap());
        assert!(!mark_notified(&conn, &seen, NoticeStatus::Unanswered, 302).unwrap());
        assert!(matches!(
            get(&conn, &seen).unwrap().unwrap().status,
            Status::Unanswered {
                notified_at: Some(301),
                ..
            }
        ));
        assert!(pending_notices(&conn, &session("giga-chat"))
            .unwrap()
            .is_empty());
    }

    // Design 5.3 review r4. Defect: a habit run sends with its own session
    // id, so a notice keyed on the sending session names the habit run,
    // which no window reconciles as a Mono chat, and the Mono hears of it
    // only after some unrelated reconcile of its chat.
    #[test]
    fn a_notice_for_a_row_a_habit_run_sent_goes_to_the_monos_current_chat() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let ask = MonoMessage {
            from_session_id: session("giga-run"),
            ..message(
                'h',
                Status::Open {
                    intent: Intent::New,
                    wanting_reply: Wanting,
                    routed: Routed {
                        handled_by_session: session("a-chat"),
                        delivery: Delivery::Submit,
                    },
                    slot: Some(waiting(1, Some(20))),
                },
            )
        };
        insert(&conn, &ask).unwrap();
        let notices = turn_ended(&conn, &session("a-chat"), "turn-1", 200).unwrap();
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["a-chat", "giga-chat"]})
        );
        assert_eq!(
            ids(pending_notices(&conn, &session("giga-chat")).unwrap()),
            vec![ask.id]
        );
        assert!(pending_notices(&conn, &session("giga-run"))
            .unwrap()
            .is_empty());
    }

    // Design 5.4 (2). Defect: on a provider that never sends turn.ready, the
    // text that opened the turn is never confirmed inside it, so the agent
    // cannot answer it; or a steer that may still wait in the provider
    // counts as seen.
    #[test]
    fn a_call_in_a_turn_confirms_the_text_that_opened_it_but_not_a_steer() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let opener = sent_to_a(&conn, "opener", true);
        assert!(
            assign_slot(&conn, &opener, "turn-1", "main", Delivery::Submit, 101)
                .unwrap()
                .0
                .is_some()
        );
        let steer = sent_to_a(&conn, "steer", true);
        assert!(
            assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 102)
                .unwrap()
                .0
                .is_some()
        );
        // PR 5 review m6 (R-h): the opener of the next turn, which still
        // waits for its authorize, is not this call's to confirm.
        let next = handed_off(&conn, "next", true, "turn-2", "main", false);
        let answer = call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "answer",
            Tool::Reply,
            yes(),
            &[],
        );
        let Submitted::Accepted { answered, .. } = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(
            answered,
            vec![Answered {
                handle: Handle::new(1),
                message_id: opener
            }]
        );
        assert!(matches!(
            get(&conn, &next).unwrap().unwrap().status,
            Status::Open {
                slot: Some(WaitingSlot {
                    delivered_at: None,
                    ..
                }),
                ..
            }
        ));
        assert!(matches!(
            get(&conn, &steer).unwrap().unwrap().status,
            Status::Open {
                slot: Some(WaitingSlot {
                    delivered_at: None,
                    ..
                }),
                ..
            }
        ));
    }

    // Design 6.4 R5, R8. Defect: a handle that a reply closed answers a
    // second reply, so its sender gets two answers to one question.
    #[test]
    fn a_reply_without_to_answers_the_one_open_handle_and_a_second_reply_finds_it_closed() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let ask = ask_delivered(&conn, "ask", "turn-1");
        let mono_a = caller_in("mono-a", "a-chat");
        let first = call(&conn, &mono_a, "first", Tool::Reply, yes(), &[]);
        let Submitted::Accepted { answered, .. } = first else {
            panic!("{first:?}");
        };
        assert_eq!(
            answered,
            vec![Answered {
                handle: Handle::new(1),
                message_id: ask.clone()
            }]
        );
        assert!(matches!(
            get(&conn, &ask).unwrap().unwrap().status,
            Status::Answered { .. }
        ));
        let again = failed(call(
            &conn,
            &mono_a,
            "again",
            Tool::Reply,
            serde_json::json!({"message": "Still yes", "to": "#1"}),
            &[],
        ));
        assert_eq!(
            (again.code, again.hint.as_str(), again.open_handles),
            ("handle_closed", "#1 is already answered.", None)
        );
    }

    // Design 6.4 R6 (PR 4a review m16). Defect: with two questions open, a
    // reply without `to` answers one of them, or the hint does not tell
    // the agent to pass `to`.
    #[test]
    fn a_reply_without_to_among_two_open_handles_is_ambiguous_and_names_both() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let first = ask_delivered(&conn, "ask-1", "turn-1");
        let second = ask_delivered(&conn, "ask-2", "turn-1");
        let error = failed(call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "answer",
            Tool::Reply,
            yes(),
            &[],
        ));
        let open = |handle, message_id| OpenHandle {
            handle: Handle::new(handle).unwrap(),
            message_id,
            from: "Boss".into(),
        };
        assert_eq!(
            (error.code, error.hint.as_str(), error.open_handles),
            (
                "ambiguous_reply",
                "Several handles are open: #1 (from Boss: Hi), #2 (from Boss: Hi). Pass to.",
                Some(vec![open(1, first), open(2, second)])
            )
        );
    }

    // Design 6.4 R11 (PR 5). Defect: a reply to handles from two senders
    // sends both answers to the first sender, or answers only one.
    #[test]
    fn a_reply_to_two_handles_answers_each_sender_in_its_own_session() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat","messageAllowlist":["mono-a"]}', 2, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1);"#,
        )
        .unwrap();
        let from_giga = ask_delivered(&conn, "ask", "turn-1");
        let from_b = accepted_ids(&call(
            &conn,
            &caller_in("mono-b", "b-chat"),
            "ask-b",
            Tool::SendMessage,
            hi("mono-a", true),
            &[],
        ))
        .remove(0);
        assert!(
            assign_slot(&conn, &from_b, "turn-1", "main", Delivery::Steer, 103)
                .unwrap()
                .0
                .is_some()
        );
        mark_delivery(&conn, &from_b, "turn-1", 104).unwrap();
        let reply = call_with(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "answer",
            Tool::Reply,
            serde_json::json!({"message": "Yes", "to": ["#1", "#2"]}),
            &[],
            &["giga-chat"],
        );
        let Submitted::Accepted {
            messages,
            answered,
            routes,
            ..
        } = reply
        else {
            panic!("{reply:?}");
        };
        let sent: Vec<_> = messages
            .iter()
            .map(|Targeted { message, .. }| {
                (
                    message.status.intent().cloned(),
                    message.to.clone(),
                    message.hop,
                )
            })
            .collect();
        let mono_party = |id: &str| Some(Party::Mono { mono_id: mono(id) });
        assert_eq!(
            sent,
            vec![
                (
                    Some(Intent::Reply(from_giga.clone())),
                    mono_party("giga"),
                    1
                ),
                (Some(Intent::Reply(from_b.clone())), mono_party("mono-b"), 1),
            ]
        );
        let group = |id: &str, via, row: &Targeted| RouteGroup {
            route: Routing {
                session_id: session(id),
                via,
            },
            message_ids: vec![row.message.id.clone()],
            owner_window: None,
        };
        assert_eq!(
            routes,
            vec![
                group("giga-chat", Delivery::Steer, &messages[0]),
                group("b-chat", Delivery::Queue, &messages[1]),
            ]
        );
        assert_eq!(
            answered,
            vec![
                Answered {
                    handle: Handle::new(1),
                    message_id: from_giga.clone()
                },
                Answered {
                    handle: Handle::new(2),
                    message_id: from_b.clone()
                },
            ]
        );
        for id in [&from_giga, &from_b] {
            assert!(matches!(
                get(&conn, id).unwrap().unwrap().status,
                Status::Answered { .. }
            ));
        }
    }

    // Design 5.2, 6.4 R5, R7. Defect: a reply in a later turn answers a
    // question that an earlier turn was given, or names a handle whose text
    // the provider never took.
    #[test]
    fn a_reply_sees_only_the_handles_its_own_turn_was_given() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let earlier = ask_delivered(&conn, "earlier", "turn-1");
        let unseen = sent_to_a(&conn, "unseen", true);
        assert!(
            assign_slot(&conn, &unseen, "turn-2", "main", Delivery::Steer, 103)
                .unwrap()
                .0
                .is_some()
        );
        let in_turn_2 = caller_in_turn("mono-a", "a-chat", "turn-2");
        let without_to = failed(call(&conn, &in_turn_2, "r1", Tool::Reply, yes(), &[]));
        assert_eq!(
            (without_to.code, without_to.open_handles),
            ("no_open_handle", Some(Vec::new()))
        );
        let named = failed(call(
            &conn,
            &in_turn_2,
            "r2",
            Tool::Reply,
            serde_json::json!({"message": "Yes", "to": "#1"}),
            &[],
        ));
        assert_eq!(
            (named.code, named.hint.as_str(), named.open_handles),
            (
                "unknown_handle",
                "No handle #1 in this turn. Open handles: none.",
                Some(Vec::new())
            )
        );
        assert!(matches!(
            get(&conn, &earlier).unwrap().unwrap().status,
            Status::Open { .. }
        ));
    }

    // PR 4a review m3. Defect: a retry of a rejected reply (app-mcp retries
    // with the same request id) lists the handles open now instead of the
    // ones the first call was told, so the two answers disagree.
    #[test]
    fn a_rejected_reply_made_again_lists_the_handles_the_first_call_listed() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        ask_delivered(&conn, "ask-1", "turn-1");
        ask_delivered(&conn, "ask-2", "turn-1");
        let mono_a = caller_in("mono-a", "a-chat");
        let first = failed(call_at(&conn, &mono_a, "answer", Tool::Reply, yes(), 200));
        accepted_ids(&call_at(
            &conn,
            &mono_a,
            "close",
            Tool::Reply,
            serde_json::json!({"message": "Yes", "to": "#1"}),
            250,
        ));
        let again = failed(call_at(&conn, &mono_a, "answer", Tool::Reply, yes(), 300));
        assert_eq!(first.code, "ambiguous_reply");
        assert_eq!(
            again,
            ToolError {
                duplicate: true,
                ..first
            }
        );
    }

    // Design 5.3 (review r6), PR 5 review m6 (R-c). Defect: a row sent from
    // a chat that was reset, deleted or archived since still owes a notice,
    // which then opens a turn in the Mono's new chat about a message it never
    // sent.
    #[test]
    fn a_row_sent_from_an_ended_chat_owes_no_notice() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        ask_delivered(&conn, "ask-1", "turn-1");
        turn_ended(&conn, &session("a-chat"), "turn-1", 200).unwrap();
        assert_eq!(
            pending_notices(&conn, &session("giga-chat")).unwrap().len(),
            1
        );
        conn.execute(
            "INSERT INTO mono_session_ends (session_id, ended_at) VALUES ('giga-chat', 300)",
            [],
        )
        .unwrap();
        assert_eq!(
            pending_notices(&conn, &session("giga-chat")).unwrap(),
            Vec::new()
        );
    }

    // PR 5 review m5. Defect: a retry of a rejected reply in a later turn
    // lists the first turn's handle numbers, which now name other messages,
    // so a follow-up `to` answers the wrong one.
    #[test]
    fn a_rejected_reply_made_again_in_a_later_turn_lists_no_old_handles() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        ask_delivered(&conn, "ask-1", "turn-1");
        ask_delivered(&conn, "ask-2", "turn-1");
        let mono_a = caller_in("mono-a", "a-chat");
        let first = failed(call_at(&conn, &mono_a, "answer", Tool::Reply, yes(), 200));
        assert_eq!(first.open_handles.as_ref().map(Vec::len), Some(2));
        turn_ended(&conn, &session("a-chat"), "turn-1", 250).unwrap();
        let third = ask_delivered(&conn, "ask-3", "turn-2");
        conn.execute(
            "UPDATE mono_messages SET created_at = 260, delivered_at = 270 WHERE id = ?1",
            [third.as_str()],
        )
        .unwrap();
        let later = Caller {
            turn_id: "turn-2".into(),
            ..mono_a
        };
        let again = failed(call_at(&conn, &later, "answer", Tool::Reply, yes(), 300));
        assert_eq!(
            again,
            ToolError {
                hint: RejectCode::AmbiguousReply.hint().into(),
                open_handles: Some(Vec::new()),
                duplicate: true,
                ..first
            }
        );
    }

    /// The error of a rejected call, checked against the one row it wrote.
    fn rejected_row(conn: &Connection, submitted: Submitted, code: RejectCode) -> MonoMessage {
        let error = failed(submitted);
        let row = get(conn, error.message_id.as_ref().expect("a row"))
            .unwrap()
            .unwrap();
        assert_eq!(
            error,
            ToolError {
                code: code.as_str(),
                hint: code.hint().into(),
                message_id: Some(row.id.clone()),
                open_handles: None,
                duplicate: false,
            }
        );
        assert!(
            matches!(row.status, Status::Rejected { gate_result, .. } if gate_result == code),
            "{row:?}"
        );
        assert_eq!(rows(conn), 1);
        row
    }

    // PR 4a review m7 (S4 before S13). Defect: a message over the limit is
    // judged on its recipient first, so the agent is told to drop
    // wanting_reply and then gets too_large on the retry.
    #[test]
    fn s4_before_s13_a_too_large_question_to_a_project_session_is_too_large() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let input = serde_json::json!({"message": "x".repeat(8001), "send_to": "project", "wanting_reply": true, "intent": "new"});
        let row = rejected_row(
            &conn,
            call(
                &conn,
                &caller_in("giga", "giga-chat"),
                "r1",
                Tool::SendMessage,
                input,
                &[listed("project", true, true)],
            ),
            RejectCode::TooLarge,
        );
        assert_eq!(
            row.to,
            Some(Party::Session {
                session_id: session("project")
            })
        );
    }

    // PR 4a review m7 (S13 before S14). Defect: a habit run that asks a
    // project session for a reply is told it cannot wait, and its retry
    // without waiting reaches a session that could never answer anyway.
    #[test]
    fn s13_before_s14_a_habit_run_asking_a_project_session_is_reply_unsupported_target() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let run = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("giga"),
            },
            ..caller_in("giga", "giga-run")
        };
        rejected_row(
            &conn,
            call(
                &conn,
                &run,
                "r1",
                Tool::SendMessage,
                hi("project", true),
                &[listed("project", true, true)],
            ),
            RejectCode::ReplyUnsupportedTarget,
        );
    }

    // PR 4a review m7 (S6 before S7). Defect: a Mono that messages its own
    // chat is told the chat is outside its projects, not that it messaged
    // itself.
    #[test]
    fn s6_before_s7_messaging_your_own_chat_is_self_send() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        rejected_row(
            &conn,
            call(
                &conn,
                &caller_in("giga", "giga-chat"),
                "r1",
                Tool::SendMessage,
                hi("giga-chat", false),
                &[listed("giga-chat", true, false)],
            ),
            RejectCode::SelfSend,
        );
    }

    // PR 4a review m7 (S1, R1). Defect: input that fails to parse leaves a
    // row, or loses the hint that names the field to fix.
    #[test]
    fn s1_r1_input_that_fails_to_parse_is_refused_with_its_hint_and_no_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for (tool, input) in [
            (
                Tool::SendMessage,
                serde_json::json!({"message": "Hi", "send_to": "mono-a"}),
            ),
            (
                Tool::Reply,
                serde_json::json!({"message": "Yes", "to": "#0"}),
            ),
        ] {
            let hint = mono_gate::parse(tool, &input).unwrap_err();
            assert_eq!(
                call(
                    &conn,
                    &caller_in("mono-a", "a-chat"),
                    "r1",
                    tool,
                    input,
                    &[]
                ),
                Submitted::Failed {
                    error: ToolError {
                        code: "invalid_input",
                        hint,
                        message_id: None,
                        open_handles: None,
                        duplicate: false,
                    }
                }
            );
        }
        assert_eq!(rows(&conn), 0);
    }

    // PR 4a review m7 (M12). Defect: a running session that the caller's
    // window lists as unable to steer is steered anyway, and the text is
    // lost.
    #[test]
    fn a_steer_into_a_running_session_listed_as_unable_to_steer_is_cannot_steer() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = opened_project(&conn, "work");
        let error = failed(call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            steering("project", work.as_str()),
            &[listed("project", false, true)],
            &["project"],
        ));
        assert_eq!(error.code, "cannot_steer");
    }

    // Design 16.2 (intents review r1). Defect: a refused steer is stored as a
    // refused new message, or an accepted send is stored with a target-less
    // steer that no row may hold.
    #[test]
    fn a_refused_steer_keeps_its_intent_and_target_and_an_accepted_send_is_new() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = opened_project(&conn, "work");
        let asked = |request, input, steerable| {
            call_with(
                &conn,
                &caller_in("giga", "giga-chat"),
                request,
                Tool::SendMessage,
                input,
                &[listed("project", steerable, true)],
                &["project"],
            )
        };
        let stored = |submitted| {
            let id = failed(submitted).message_id.expect("a rejected row");
            get(&conn, &id).unwrap().expect("the row").status
        };
        assert_eq!(
            stored(asked("r1", steering("project", work.as_str()), false)),
            Status::Rejected {
                gate_result: RejectCode::CannotSteer,
                wanting_reply: false,
                intent: IntentKind::Steer,
                reply_to: Some(work.clone()),
                scope: None,
            }
        );
        assert!(matches!(
            stored(call_with(
                &conn,
                &caller_in("giga", "giga-chat"),
                "r2",
                Tool::SendMessage,
                hi("nope", false),
                &[],
                &[],
            )),
            Status::Rejected {
                gate_result: RejectCode::UnknownTarget,
                intent: IntentKind::New,
                reply_to: None,
                ..
            }
        ));
        let intent = |submitted: Submitted| {
            let id = accepted_ids(&submitted).remove(0);
            get(&conn, &id).unwrap().unwrap().status.intent().cloned()
        };
        assert_eq!(
            intent(asked("r3", hi("project", false), true)),
            Some(Intent::New)
        );
        assert_eq!(
            intent(asked("r4", steering("project", work.as_str()), true)),
            Some(Intent::Steer(SteerTarget::Message(work)))
        );
    }

    // PR 4a review m7 (M14). Defect: a session whose worktree was removed
    // still exists for the gate, so a message waits for a session no window
    // can open.
    #[test]
    fn a_session_whose_worktree_was_removed_is_an_unknown_target() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute(
            "UPDATE sessions SET worktree_removed = 1 WHERE id = 'project'",
            [],
        )
        .unwrap();
        let error = failed(call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            hi("project", false),
            &[],
        ));
        assert_eq!(error.code, "unknown_target");
    }

    // PR 4a review m7 (M20). Defect: the command reads a habit run as a
    // Mono chat, so S14 never fires and the run waits for an answer that
    // arrives after it ended.
    #[test]
    fn a_habit_run_caller_from_the_web_app_cannot_wait_for_a_reply() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let caller: CallerArg = serde_json::from_value(serde_json::json!({
            "kind": "habit_run",
            "monoId": "giga",
            "sessionId": "giga-run",
            "turnId": "turn-1",
        }))
        .unwrap();
        let error = failed(call(
            &conn,
            &caller.parse().unwrap(),
            "r1",
            Tool::SendMessage,
            hi("mono-a", true),
            &[],
        ));
        assert_eq!(error.code, "habit_cannot_wait");
    }

    /// A row's id, status, receiver, close, notice and update time.
    type RowState = (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        i64,
    );

    fn snapshot(conn: &Connection) -> Vec<RowState> {
        conn.prepare(
            "SELECT id, status, handled_by_session, closed_at, notified_at, updated_at
               FROM mono_messages ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    fn ended_at(conn: &Connection, session_id: &str) -> Option<i64> {
        conn.query_row(
            "SELECT ended_at FROM mono_session_ends WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
    }

    /// The clear as the delete or archive transaction runs it.
    fn clear(conn: &Connection, session_id: &str, now: i64) -> Notices {
        let tx = conn.unchecked_transaction().unwrap();
        let notices = clear_session(&tx, &session(session_id), now).unwrap();
        tx.commit().unwrap();
        notices
    }

    // Design 7, 5.1. Defect: a reset, delete or archive leaves the messages
    // that the chat still had to take open or queued with nothing to take
    // them, keeps the chat as their receiver, drops an answered or already
    // delivered message, or changes rows again on a second clear.
    #[test]
    fn a_clear_drops_what_the_session_still_had_to_take_once() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let waiting = sent_to_a(&conn, "waiting", true);
        let untaken = handed_off(&conn, "untaken", false, "turn-2", "main", false);
        let taken = handed_off(&conn, "taken", false, "turn-1", "main", true);
        let answered = ask_delivered(&conn, "answered", "turn-1");
        accepted_ids(&call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "re",
            Tool::Reply,
            yes(),
            &[],
        ));
        let notices = clear(&conn, "a-chat", 200);
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["giga-chat"]})
        );
        let status = |id: &MessageId| get(&conn, id).unwrap().unwrap().status;
        let receiver = |id: &MessageId| -> Option<String> {
            conn.query_row(
                "SELECT handled_by_session FROM mono_messages WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .unwrap()
        };
        for id in [&waiting, &untaken] {
            assert!(matches!(
                status(id),
                Status::Dropped {
                    closed_at: 200,
                    notified_at: None,
                    ..
                }
            ));
            assert_eq!(receiver(id), None);
        }
        assert!(matches!(status(&taken), Status::Delivered { .. }));
        assert!(matches!(status(&answered), Status::Answered { .. }));
        assert_eq!(
            sorted(ids(pending_notices(&conn, &session("giga-chat")).unwrap())),
            sorted(vec![waiting.clone(), untaken.clone()])
        );
        let before = snapshot(&conn);
        assert_eq!(clear(&conn, "a-chat", 300), Notices::default());
        assert_eq!(snapshot(&conn), before);
        assert_eq!(ended_at(&conn, "a-chat"), Some(200));
    }

    // Design 5.1, Q8. Defect: a question that the cleared chat sent stays
    // open, so a late reply lands in a chat that no longer has its context,
    // or its sender's next chat, or the chat itself once unarchived, is told
    // about a message that the chat no longer waits for.
    #[test]
    fn a_clear_closes_the_questions_the_session_sent_without_a_notice() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let question = sent(&conn, ("mono-a", "a-chat"), "giga", "q1", true);
        hand_off(&conn, &question, "turn-1", "main", true);
        let notices = clear(&conn, "a-chat", 200);
        assert_eq!(notices, Notices::default());
        assert!(matches!(
            get(&conn, &question).unwrap().unwrap().status,
            Status::Dropped { .. }
        ));
        assert!(pending_notices(&conn, &session("a-chat"))
            .unwrap()
            .is_empty());
        // Unarchived, the chat receives again, but owes no old notice.
        crate::session_store::set_archived(&conn, "a-chat", false).unwrap();
        assert!(pending_notices(&conn, &session("a-chat"))
            .unwrap()
            .is_empty());
        let late = failed(call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "late",
            Tool::Reply,
            serde_json::json!({"message": "Yes", "to": "#1"}),
            &[],
        ));
        assert_eq!(late.code, "handle_closed");
    }

    // Design 5.3 (review r6). Defect: a message that a reset chat sent and
    // the receiver's clear drops later still owes a notice, which opens a
    // turn in the Mono's new chat about a message that chat never sent; or
    // the rule also silences the notice of a message the Mono's habit run
    // sent.
    #[test]
    fn after_a_clear_of_the_sender_only_its_habit_runs_rows_still_owe_a_notice() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let from_chat = sent(&conn, ("mono-a", "a-chat"), "giga", "chat", false);
        let run = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("mono-a"),
            },
            session_id: session("a-run"),
            turn_id: "turn-1".into(),
        };
        let from_run = accepted_ids(&call(
            &conn,
            &run,
            "run",
            Tool::SendMessage,
            serde_json::json!({"message": "Hi", "send_to": "giga", "wanting_reply": false, "intent": "new"}),
            &[],
        ))
        .remove(0);
        // The reset of A's chat: the clear, then A's new chat.
        clear(&conn, "a-chat", 200);
        conn.execute(
            r#"UPDATE monos SET data_json = '{"sessionId":"a-new"}' WHERE id = 'mono-a'"#,
            [],
        )
        .unwrap();
        let notices = clear(&conn, "giga-chat", 300);
        for id in [&from_chat, &from_run] {
            assert!(matches!(
                get(&conn, id).unwrap().unwrap().status,
                Status::Dropped { .. }
            ));
        }
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["a-new"]})
        );
        assert_eq!(
            ids(pending_notices(&conn, &session("a-new")).unwrap()),
            vec![from_run]
        );
    }

    // Design 5.5 rule 8 (review r5, r6). Defect: Remove drops a row that a
    // hand-off holds, so its sender is told `dropped` about a text the
    // receiver got; drops one the provider already took; or a hand-off
    // after the drop still delivers it.
    #[test]
    fn remove_drops_only_a_row_that_no_hand_off_holds() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let held = handed_off(&conn, "held", true, "turn-1", "main", false);
        let seen = ask_delivered(&conn, "seen", "turn-1");
        let waiting = sent_to_a(&conn, "waiting", true);
        let drop = |id: &MessageId| drop_message(&conn, id, 200).unwrap();
        let status = |id: &MessageId| get(&conn, id).unwrap().unwrap().status;

        assert_eq!(drop(&held), (DropAnswer::InFlight, Notices::default()));
        assert!(matches!(status(&held), Status::Open { slot: Some(_), .. }));
        assert_eq!(drop(&seen), (DropAnswer::Closed, Notices::default()));
        assert!(matches!(status(&seen), Status::Open { slot: Some(_), .. }));

        let (answer, notices) = drop(&waiting);
        assert_eq!(answer, DropAnswer::Dropped);
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["giga-chat"]})
        );
        assert!(matches!(
            status(&waiting),
            Status::Dropped { closed_at: 200, .. }
        ));
        assert!(
            assign_slot(&conn, &waiting, "turn-2", "main", Delivery::Submit, 201)
                .unwrap()
                .0
                .is_none()
        );
        assert_eq!(drop(&waiting), (DropAnswer::Closed, Notices::default()));
        assert_eq!(drop(&id('z')), (DropAnswer::Closed, Notices::default()));
    }

    // Design 7 (review r1). Defect: an archived session stays in
    // `mono_session_ends` after it is unarchived, so every message to it is
    // refused as if it were gone.
    #[test]
    fn an_unarchived_session_receives_again() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let to_project = |request_id: &str| {
            call(
                &conn,
                &caller_in("giga", "giga-chat"),
                request_id,
                Tool::SendMessage,
                hi("project", false),
                &[listed("project", true, true)],
            )
        };
        crate::session_store::set_archived(&conn, "project", true).unwrap();
        assert_eq!(failed(to_project("r1")).code, "unknown_target");
        crate::session_store::set_archived(&conn, "project", false).unwrap();
        accepted_ids(&to_project("r2"));
    }

    // PR 6 review r1 M1. Defect: the clear commits on its own and the delete
    // or archive after it fails, so a chat that stays live is gone to the
    // gate: every message to it is refused and its rows owe no notice.
    #[test]
    fn a_failed_delete_or_archive_leaves_the_session_receiving() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let queued = sent_to_a(&conn, "queued", true);
        conn.execute_batch(
            "CREATE TEMP TRIGGER no_archive BEFORE UPDATE OF archived ON sessions
               BEGIN SELECT RAISE(ABORT, 'disk full'); END;
             CREATE TEMP TRIGGER no_delete BEFORE DELETE ON sessions
               BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
        let still_live = |step: &str| {
            assert_eq!(ended_at(&conn, "a-chat"), None, "{step}");
            assert!(
                matches!(
                    get(&conn, &queued).unwrap().unwrap().status,
                    Status::Open { .. }
                ),
                "{step}"
            );
        };
        assert!(crate::session_store::set_archived(&conn, "a-chat", true).is_err());
        still_live("archive");
        assert!(crate::session_store::delete_session(
            &conn,
            "a-chat",
            crate::session_store::Removal::End
        )
        .is_err());
        still_live("delete");
        sent(&conn, ("giga", "giga-chat"), "mono-a", "after", false);
        conn.execute_batch("DROP TRIGGER no_archive; DROP TRIGGER no_delete;")
            .unwrap();
        let notices = crate::session_store::delete_session(
            &conn,
            "a-chat",
            crate::session_store::Removal::End,
        )
        .unwrap();
        assert!(ended_at(&conn, "a-chat").is_some());
        assert!(matches!(
            get(&conn, &queued).unwrap().unwrap().status,
            Status::Dropped { .. }
        ));
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"sessions": ["giga-chat"]})
        );
    }

    // PR 6 review r2 m1, decision 407. Defect: a discarded draft clears its
    // session like a delete, so a Mono whose fresh chat only held a draft
    // can no longer be reached and the question queued for it is dropped.
    #[test]
    fn a_discarded_draft_stays_receiving() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let queued = sent_to_a(&conn, "queued", true);
        let notices = crate::session_store::delete_session(
            &conn,
            "a-chat",
            crate::session_store::Removal::DiscardDraft,
        )
        .unwrap();
        assert_eq!(notices, Notices::default());
        assert_eq!(ended_at(&conn, "a-chat"), None);
        assert!(matches!(
            get(&conn, &queued).unwrap().unwrap().status,
            Status::Open { .. }
        ));
        accepted_ids(&call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "after",
            Tool::SendMessage,
            serde_json::json!({"message": "Hi", "send_to": "mono-a", "wanting_reply": true, "intent": "new"}),
            &[listed("a-chat", true, true)],
        ));
    }

    // PR 6 review r1 m1. Defect: a call that the control host let through
    // before the clear forgot its session lands after the clear, and its row
    // is never dropped, because nothing clears the session again.
    #[test]
    fn a_call_from_a_cleared_session_is_refused() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        clear(&conn, "a-chat", 200);
        let refused = failed(call(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "late",
            Tool::SendMessage,
            serde_json::json!({"message": "Q?", "send_to": "giga", "wanting_reply": true, "intent": "new"}),
            &[],
        ));
        assert_eq!(refused.code, "unknown_token");
        assert!(snapshot(&conn).is_empty());
    }

    // PR 6 review r1 n1, design 16.6 (intents review r2) M3. Defect: the
    // late confirm of a held row the clear dropped fails on the CHECK
    // `status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL`.
    #[test]
    fn a_confirm_after_the_clear_leaves_the_dropped_row_alone() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let held = handed_off(&conn, "held", false, "turn-9", "main", false);
        clear(&conn, "a-chat", 200);
        mark_delivery(&conn, &held, "turn-9", 201).unwrap();
        assert!(matches!(
            get(&conn, &held).unwrap().unwrap().status,
            Status::Dropped { closed_at: 200, .. }
        ));
        let delivered_at: Option<i64> = conn
            .query_row(
                "SELECT delivered_at FROM mono_messages WHERE id = ?1",
                [held.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(delivered_at, None);
    }

    /// `status`, `turn_id`, `delivered_at` and `reply_to` as stored.
    pub(crate) fn stored_row(
        conn: &Connection,
        id: &MessageId,
    ) -> (String, Option<String>, Option<i64>, Option<String>) {
        conn.query_row(
            "SELECT status, turn_id, delivered_at, reply_to FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    /// Work the Giga sent `a-chat`, which turn `turn-1` took, and a steer of
    /// the Giga at it, accepted while that turn runs and not handed off yet.
    fn steered(conn: &Connection, request_id: &str) -> (MessageId, MessageId) {
        let work = handed_off(
            conn,
            &format!("{request_id}-work"),
            false,
            "turn-1",
            "main",
            true,
        );
        let steer = accepted_ids(&call_with(
            conn,
            &caller_in("giga", "giga-chat"),
            request_id,
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &["a-chat"],
        ))
        .remove(0);
        (work, steer)
    }

    // PR I2, S9 through resolve_ref. Defect: a steer names an unknown id or a
    // rejected row and still goes somewhere, or an accepted row that is not
    // between the two loses its target.
    #[test]
    fn a_steer_reply_to_resolves_only_to_an_accepted_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let rejected = failed(call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "nowhere",
            Tool::SendMessage,
            hi("nope", false),
            &[],
        ))
        .message_id
        .unwrap();
        // A asks the Giga.
        let asked = sent(&conn, ("mono-a", "a-chat"), "giga", "asked", true);
        hand_off(&conn, &asked, "turn-1", "main", true);
        let steer_from = |caller: &Caller, request: &str, reply_to: &str| {
            failed(call_with(
                &conn,
                caller,
                request,
                Tool::SendMessage,
                steering("mono-a", reply_to),
                &[],
                &["a-chat"],
            ))
        };
        let giga = caller_in("giga", "giga-chat");
        for (request, reply_to) in [
            ("unknown", "m_zzzzzzzzzzzz"),
            ("refused", rejected.as_str()),
        ] {
            let error = steer_from(&giga, request, reply_to);
            assert_eq!(error.code, "unknown_message", "{reply_to}");
            let row = stored_row(&conn, &error.message_id.unwrap());
            assert_eq!(row.3, None, "{reply_to}");
        }
        // The id resolves to the message A sent; a steer about it is not
        // between the Giga and A in the Giga's direction, and the row keeps
        // its target.
        let error = steer_from(&giga, "held", asked.as_str());
        assert_eq!(error.code, "steer_target_mismatch");
        assert_eq!(
            stored_row(&conn, &error.message_id.unwrap()).3.as_deref(),
            Some(asked.as_str())
        );
    }

    // PR I2, S20 and record. Defect: an accepted steer goes to the
    // recipient's current turn rather than the session handling its target,
    // is stored without its target, or its hop does not grow.
    #[test]
    fn an_accepted_steer_is_stored_with_its_target_one_hop_deeper_and_routed_to_its_session() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (work, steer) = steered(&conn, "steer-1");
        let message = get(&conn, &steer).unwrap().unwrap();
        assert_eq!(message.hop, 1);
        assert_eq!(
            message.status.intent(),
            Some(&Intent::Steer(SteerTarget::Message(work.clone())))
        );
        assert_eq!(stored_delivery(&conn, &steer).as_deref(), Some("queue"));
        let (steer_slot, notices) =
            assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        assert!(steer_slot.is_some() && notices == Notices::default());
        mark_delivery(&conn, &steer, "turn-1", 102).unwrap();
        let again = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "steer-2",
            Tool::SendMessage,
            steering("mono-a", steer.as_str()),
            &[],
            &["a-chat"],
        );
        assert_eq!(
            route_of(&again),
            Some(Routing {
                session_id: session("a-chat"),
                via: Delivery::Steer
            })
        );
        let second = accepted_ids(&again).remove(0);
        assert_eq!(get(&conn, &second).unwrap().unwrap().hop, 2);
    }

    /// Rule (b): the Giga's question, unanswered once `a-chat`'s turn-1
    /// ended, and the Mono's late answer, accepted while the Giga's chat ran
    /// `giga_turn`.
    pub(crate) fn late_answer(conn: &Connection, giga_turn: &str) -> (MessageId, MessageId) {
        let asked = ask_delivered(conn, "ask", "turn-1");
        turn_ended(conn, &session("a-chat"), "turn-1", 100).unwrap();
        let late = call_in_turns(
            conn,
            &caller_in_turn("mono-a", "a-chat", "turn-2"),
            "late",
            Tool::SendMessage,
            steering("giga", asked.as_str()),
            &[],
            &[("giga-chat", giga_turn)],
        );
        assert_eq!(
            route_of(&late),
            Some(Routing {
                session_id: session("giga-chat"),
                via: Delivery::Steer
            })
        );
        (asked, accepted_ids(&late).remove(0))
    }

    // Design 6.1 rule (b), PR 7 review r1 B2. Defect: the late answer may
    // join only the turn of the Mono's chat that held the question, so the
    // hand-off into the Giga's running turn drops it. PR I4 review n3: its
    // target holds #1 in the Mono's chat, not in the Giga's turn, so the
    // reader must name it by message id.
    #[test]
    fn a_monos_late_answer_is_delivered_in_the_gigas_running_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (asked, late) = late_answer(&conn, "turn-4");
        let (slot, notices) =
            assign_slot(&conn, &late, "turn-4", "main", Delivery::Steer, 150).unwrap();
        assert_eq!(notices, Notices::default());
        let target = slot.expect("the late answer joins turn-4").target.unwrap();
        assert_eq!(
            (target.message_id, target.handled_by_session, target.slot),
            (
                asked,
                Some(session("a-chat")),
                Some(TargetSlot {
                    turn_id: "turn-1".into(),
                    handle: Handle::parse("#1").unwrap()
                })
            )
        );
        mark_delivery(&conn, &late, "turn-4", 151).unwrap();
        assert_eq!(stored_row(&conn, &late).0, "delivered");
    }

    // Design 16.6, PR 7 review r1 B2. Defect: a waiting late answer
    // outlives the Giga's turn, or the end of a turn of the Mono's chat with
    // the same turn id drops it.
    #[test]
    fn a_late_answer_still_waiting_when_the_gigas_turn_ends_is_dropped() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, late) = late_answer(&conn, "turn-4");
        turn_ended(&conn, &session("a-chat"), "turn-4", 200).unwrap();
        assert_eq!(stored_row(&conn, &late).0, "delivered");
        let notices = turn_ended(&conn, &session("giga-chat"), "turn-4", 201).unwrap();
        assert_eq!(stored_row(&conn, &late).0, "dropped");
        assert!(notices.sessions.contains(&session("a-chat")), "{notices:?}");
    }

    // Design 16.6, PR 7 review r2 m6. Defect: after Stop or a switch the web
    // app's next turn takes a late answer accepted in the turn that is still
    // ending, whose end then no longer drops it.
    #[test]
    fn a_late_answer_joins_only_the_turn_it_was_accepted_for() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, late) = late_answer(&conn, "turn-4");
        let (slot, notices) =
            assign_slot(&conn, &late, "turn-5", "main", Delivery::Steer, 150).unwrap();
        assert!(slot.is_none());
        assert!(notices.sessions.contains(&session("a-chat")), "{notices:?}");
        assert_eq!(stored_row(&conn, &late).0, "dropped");
    }

    // Design 16.12.3.3 and rule (b), review r2 m1. Defect: the Giga's turn-4
    // ends after the gate read `running` and before the insert, the recheck
    // skips the late answer pinned to turn-4, and no later turn end drops
    // it: the caller gets a route into an ended turn and no notice.
    #[test]
    fn the_submit_recheck_drops_a_late_answer_whose_turn_ended_meanwhile() {
        let store = messaging_store();
        let asked = {
            let conn = store.lock_conn().unwrap();
            let asked = ask_delivered(&conn, "ask", "turn-1");
            turn_ended(&conn, &session("a-chat"), "turn-1", 100).unwrap();
            turn_ended(&conn, &session("giga-chat"), "turn-4", 105).unwrap();
            asked
        };
        let reads = std::cell::Cell::new(0);
        let running = || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                return HashMap::from([("giga-chat".to_string(), "turn-4".to_string())]);
            }
            HashMap::new()
        };
        let (first, notices) = submit_and_recheck(
            &store,
            running,
            &caller_in_turn("mono-a", "a-chat", "turn-2"),
            &RequestId::parse("c0").unwrap(),
            mono_gate::parse(Tool::SendMessage, &steering("giga", asked.as_str())),
            &[],
            &["codex".to_string()],
            110,
        )
        .unwrap();
        let conn = store.lock_conn().unwrap();
        assert_eq!(stored_row(&conn, &accepted_ids(&first)[0]).0, "dropped");
        assert_eq!(route_of(&first), None);
        assert!(notices.sessions.contains(&session("a-chat")), "{notices:?}");
    }

    // Review r1 m1 (store level). Defect: the end of the Giga's turn-4,
    // recorded after turn-5 runs (Stop, then the next queued turn), drops a
    // late answer accepted for turn-5, and the Mono gets a false `dropped`
    // notice.
    #[test]
    fn an_old_turns_end_leaves_a_late_answer_accepted_for_the_next_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, late) = late_answer(&conn, "turn-5");
        let notices = turn_ended(&conn, &session("giga-chat"), "turn-4", 120).unwrap();
        assert_eq!(stored_row(&conn, &late).0, "delivered", "{notices:?}");
    }

    // Design E25 and 6 (`Reject { code, hop, to }`), PR 7 review r1 m1 and
    // decision 628. Defect: a rejected `hop_limit` row stores hop 0.
    #[test]
    fn a_hop_limit_row_stores_the_hop_the_steer_would_have_had() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let ask = ask_delivered(&conn, "ask", "turn-1");
        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        conn.execute(
            "UPDATE mono_messages SET hop = 4 WHERE id = ?1",
            [ask.as_str()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
            .unwrap();
        let error = failed(call_in_turns(
            &conn,
            &caller_in("giga", "giga-chat"),
            "steer",
            Tool::SendMessage,
            steering("mono-a", ask.as_str()),
            &[],
            &[("a-chat", "turn-1")],
        ));
        assert_eq!(error.code, "hop_limit");
        let message = get(&conn, &error.message_id.unwrap()).unwrap().unwrap();
        assert!(
            matches!(message.status, Status::Rejected { .. }),
            "{:?}",
            message.status
        );
        assert_eq!(message.hop, 5);
    }

    // PR I2, S15 with the turn ids of Live.running. Defect: a steer joins a
    // newer turn than the one that runs its target (unrelated work), or
    // reaches the model before the queued message it changes.
    #[test]
    fn a_steer_whose_target_turn_is_not_the_running_one_is_target_gone() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = handed_off(&conn, "work", false, "turn-1", "main", true);
        let queued = sent_to_a(&conn, "queued", false);
        for (request, target, turn) in [
            ("newer", &work, "turn-2"),
            ("queued-target", &queued, "turn-1"),
        ] {
            let error = failed(call_in_turns(
                &conn,
                &caller_in("giga", "giga-chat"),
                request,
                Tool::SendMessage,
                steering("mono-a", target.as_str()),
                &[],
                &[("a-chat", turn)],
            ));
            assert_eq!(error.code, "target_gone", "{request}");
            let row = stored_row(&conn, &error.message_id.unwrap());
            assert_eq!(
                (row.0.as_str(), row.3.as_deref()),
                ("rejected", Some(target.as_str()))
            );
        }
        let ok = call_in_turns(
            &conn,
            &caller_in("giga", "giga-chat"),
            "same",
            Tool::SendMessage,
            steering("mono-a", work.as_str()),
            &[],
            &[("a-chat", "turn-1")],
        );
        assert_eq!(accepted_ids(&ok).len(), 1);
    }

    // PR I2 (intents review r1 H2, r2 L2). Defect: a steer whose turn ended
    // opens the next turn as new work, or joins another turn, and its sender
    // hears of the drop only at its next reconcile.
    #[test]
    fn a_steer_row_takes_a_slot_only_as_a_steer_into_its_targets_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for (request, turn, delivery) in [
            ("other-turn", "turn-2", Delivery::Steer),
            ("opener", "turn-1", Delivery::Submit),
        ] {
            let (_, steer) = steered(&conn, request);
            let (slot, notices) = assign_slot(&conn, &steer, turn, "main", delivery, 101).unwrap();
            assert_eq!(slot, None, "{request}");
            assert_eq!(notices.sessions, vec![session("giga-chat")], "{request}");
            assert_eq!(stored_row(&conn, &steer).0, "dropped", "{request}");
            assert_eq!(stored_row(&conn, &steer).1, None, "{request}");
            assert!(
                pending_notices(&conn, &session("giga-chat"))
                    .unwrap()
                    .iter()
                    .any(|message| message.id == steer),
                "{request}"
            );
        }
        let (_, steer) = steered(&conn, "right-turn");
        let (slot, notices) =
            assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        assert_eq!(notices, Notices::default());
        assert_eq!(
            slot.map(|assigned| assigned.message.id),
            Some(steer.clone())
        );
        assert_eq!(stored_delivery(&conn, &steer).as_deref(), Some("steer"));
    }

    // PR I2 (intents review r1 H2, r2 M3). Defect: the turn end gives an
    // undelivered steer its slot back, so reconcile queues it for the next
    // turn; or the late confirm of the dropped steer fails the CHECK
    // `status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL`
    // inside handOff's finally.
    #[test]
    fn the_turn_end_drops_an_undelivered_steer_and_a_late_confirm_leaves_it_dropped() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, steer) = steered(&conn, "steer");
        assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        let waiting = handed_off(&conn, "waiting", false, "turn-1", "main", false);
        let notices = turn_ended(&conn, &session("a-chat"), "turn-1", 150).unwrap();
        assert!(
            notices.sessions.contains(&session("giga-chat")),
            "{notices:?}"
        );
        let (status, turn, delivered_at, _) = stored_row(&conn, &steer);
        assert_eq!(
            (status.as_str(), turn.as_deref(), delivered_at),
            ("dropped", Some("turn-1"), None)
        );
        assert_eq!(stored_row(&conn, &waiting).0, "delivered");
        assert_eq!(stored_row(&conn, &waiting).1, None);
        mark_delivery(&conn, &steer, "turn-1", 151).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "dropped");
        assert_eq!(stored_row(&conn, &steer).2, None);
        assert!(unqueued(&conn, &session("a-chat"))
            .unwrap()
            .iter()
            .all(|row| row.message.id != steer));
    }

    // PR I2 review r1 M1. Defect: a steer accepted for turn T that no
    // hand-off slotted yet (it waits behind a question, or in a queue that
    // Stop paused) survives the end of T, and its sender gets no notice; or
    // the drop runs after the turn end freed the slot of a target that T
    // never took, and misses the steer at it.
    #[test]
    fn the_turn_end_drops_a_steer_accepted_for_it_that_no_hand_off_slotted() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, steer) = steered(&conn, "steer");
        let untaken = handed_off(&conn, "untaken", false, "turn-1", "main", false);
        let at_untaken = accepted_ids(&call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "at-untaken",
            Tool::SendMessage,
            steering("mono-a", untaken.as_str()),
            &[],
            &["a-chat"],
        ))
        .remove(0);
        let other = turn_ended(&conn, &session("a-chat"), "turn-0", 140).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "delivered", "{other:?}");
        let notices = turn_ended(&conn, &session("a-chat"), "turn-1", 150).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "dropped", "{notices:?}");
        assert_eq!(stored_row(&conn, &at_untaken).0, "dropped");
        assert!(
            notices.sessions.contains(&session("a-chat"))
                && notices.sessions.contains(&session("giga-chat")),
            "{notices:?}"
        );
        let (slot, _) = assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 151).unwrap();
        assert!(slot.is_none());
    }

    // PR I2 (intents review r1, r2 L2). Defect: a failed steer hand-off puts
    // the row back to wait, so the drain opens a turn with it.
    #[test]
    fn a_failed_steer_hand_off_drops_the_row_and_names_its_senders_chat() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, steer) = steered(&conn, "steer");
        assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        let notices = release(&conn, &steer, "turn-1", 102).unwrap();
        assert_eq!(notices.sessions, vec![session("giga-chat")]);
        assert_eq!(stored_row(&conn, &steer).0, "dropped");
        let plain = handed_off(&conn, "plain", false, "turn-1", "main", false);
        assert_eq!(
            release(&conn, &plain, "turn-1", 103).unwrap(),
            Notices::default()
        );
        assert_eq!(
            stored_row(&conn, &plain),
            ("delivered".into(), None, None, None)
        );
    }

    fn read(conn: &Connection, caller: &Caller, handle: &str) -> Got {
        get_message(
            conn,
            caller,
            mono_gate::parse_get(&serde_json::json!({ "handle": handle })),
        )
        .unwrap()
    }

    fn found(got: Got) -> serde_json::Value {
        match got {
            Got::Found { message } => serde_json::to_value(message).unwrap(),
            failed => panic!("{failed:?}"),
        }
    }

    fn read_error(got: Got) -> ToolError {
        match got {
            Got::Failed { error } => error,
            found => panic!("{found:?}"),
        }
    }

    /// The Mono `mono-c` (chat `c-chat`), a party to nothing yet.
    fn seed_c(conn: &Connection) {
        conn.execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-c', 'C', 'mono', '{"sessionId":"c-chat"}', 2, 1, 1);"#,
        )
        .unwrap();
    }

    /// Every column of every `mono_messages` row, in id order.
    fn dump(conn: &Connection) -> Vec<Vec<Sql>> {
        let mut statement = conn
            .prepare("SELECT * FROM mono_messages ORDER BY id")
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns).map(|index| row.get::<_, Sql>(index)).collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    // Design 16.4. Defect: the sender cannot read what it sent, the receiver
    // cannot read a message by the handle its turn shows, the answer leaves
    // out a field, or the read changes a row (PR I3 review r1 m2: a full
    // dump, not the row count, so an UPDATE is caught too).
    #[test]
    fn get_message_reads_a_row_by_id_for_its_sender_and_by_handle_in_the_receivers_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let asked = ask_delivered(&conn, "ask", "turn-1");
        let before = dump(&conn);
        assert_eq!(
            found(read(&conn, &caller_in("giga", "giga-chat"), asked.as_str())),
            serde_json::json!({
                "message_id": asked.as_str(),
                "from": {"mono_id": "giga", "mono_name": "Boss"},
                "to": {"mono_id": "mono-a", "mono_name": "A"},
                "intent": "new",
                "reply_to": null,
                "body": "Hi",
                "wanting_reply": true,
                "status": "open",
                "created_at": 100,
            })
        );
        // A habit run of the Giga is another session of the same Mono.
        let habit = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("giga"),
            },
            session_id: session("giga-habit"),
            ..caller_in("giga", "giga-chat")
        };
        assert_eq!(
            found(read(&conn, &habit, asked.as_str()))["message_id"],
            asked.as_str()
        );
        let by_handle = found(read(&conn, &caller_in("mono-a", "a-chat"), "#1"));
        assert_eq!(by_handle["message_id"], asked.as_str());
        assert_eq!(by_handle["handle"], "#1");
        assert_eq!(by_handle["body"], "Hi");
        assert_eq!(dump(&conn), before);
    }

    // Design 16.4. Defect: a Mono reads messages between two others, or the
    // answer tells it that the row exists.
    #[test]
    fn get_message_answers_unknown_message_to_a_mono_that_is_not_a_party() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_c(&conn);
        let asked = ask_delivered(&conn, "ask", "turn-1");
        let outsider = caller_in("mono-c", "c-chat");
        let hidden = read_error(read(&conn, &outsider, asked.as_str()));
        assert_eq!(hidden.code, "unknown_message");
        assert_eq!(hidden.hint, "No message you can read has that id.");
        let missing = read_error(read(&conn, &outsider, "m_aaaaaaaaaaaa"));
        assert_eq!(hidden, missing);
    }

    // Design 16.4, PR I3 review r1 m1. A Mono caller's party is its Mono,
    // never its session. Defect: a caller reads a message between two other
    // Monos because it runs in the session that sent or handled it.
    #[test]
    fn get_message_keys_the_party_on_the_callers_mono_not_its_session() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_c(&conn);
        let asked = ask_delivered(&conn, "ask", "turn-1");
        for session_id in ["giga-chat", "a-chat"] {
            let error = read_error(read(
                &conn,
                &caller_in("mono-c", session_id),
                asked.as_str(),
            ));
            assert_eq!(error.code, "unknown_message", "{session_id}");
        }
    }

    // Design 16.4: only accepted rows. Defect: a refused call reads as a
    // message that was sent.
    #[test]
    fn get_message_answers_unknown_message_for_a_rejected_row_of_the_caller() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let error = failed(call(
            &conn,
            &giga,
            "self",
            Tool::SendMessage,
            hi("giga", false),
            &[],
        ));
        assert_eq!(error.code, "self_send");
        let rejected = error.message_id.expect("a rejected row");
        assert_eq!(
            read_error(read(&conn, &giga, rejected.as_str())).code,
            "unknown_message"
        );
    }

    // Design 16.4. Defect: a handle that names nothing reads as a missing
    // id, so the hint does not send the model to the message_id.
    #[test]
    fn get_message_answers_unknown_handle_for_a_handle_not_in_this_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        ask_delivered(&conn, "ask", "turn-1");
        let error = read_error(read(&conn, &caller_in("mono-a", "a-chat"), "#9"));
        assert_eq!(error.code, "unknown_handle");
        assert_eq!(
            error.hint,
            "No handle #9 in this turn. Pass the message's message_id instead."
        );
    }

    // Design 16.4 (intents review r1). Handles restart at #1 in each turn.
    // Defect: a "#1" read in turn 2 names the turn 1 message, or the answer
    // shows a handle for a row that holds it only in an earlier turn, so
    // nothing tells the model which message it got.
    #[test]
    fn get_message_reads_a_handle_in_the_callers_turn_only() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let first = ask_delivered(&conn, "first", "turn-1");
        turn_ended(&conn, &session("a-chat"), "turn-1", 200).unwrap();
        let second = ask_delivered(&conn, "second", "turn-2");
        let a = caller_in_turn("mono-a", "a-chat", "turn-2");
        let current = found(read(&conn, &a, "#1"));
        assert_eq!(current["message_id"], second.as_str());
        assert_eq!(current["handle"], "#1");
        let earlier = found(read(&conn, &a, first.as_str()));
        assert_eq!(earlier["status"], "unanswered");
        assert!(earlier.get("handle").is_none(), "{earlier}");
        // A turn that was given no handle: "#1" of the turns before it names
        // nothing here.
        let later = caller_in_turn("mono-a", "a-chat", "turn-3");
        assert_eq!(read_error(read(&conn, &later, "#1")).code, "unknown_handle");
    }

    // Design 16.4. Defect: a message whose chat was deleted, or whose sender
    // Mono was deleted, can no longer be read by the Mono it is about.
    #[test]
    fn get_message_reads_a_dropped_row_and_a_row_from_a_deleted_mono() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let queued = sent_to_a(&conn, "queued", true);
        let asked = sent_to_a(&conn, "asked", false);
        clear(&conn, "a-chat", 150);
        let dropped = found(read(
            &conn,
            &caller_in("giga", "giga-chat"),
            queued.as_str(),
        ));
        assert_eq!(dropped["status"], "dropped");
        assert_eq!(dropped["body"], "Hi");
        conn.execute("DELETE FROM monos WHERE id = 'giga'", [])
            .unwrap();
        let orphan = found(read(&conn, &caller_in("mono-a", "a-chat"), asked.as_str()));
        assert_eq!(
            orphan["from"],
            serde_json::json!({"session_id": "giga-chat"})
        );
    }

    // Design 16.4. Defect: a field other than handle, or a value that is not
    // one reference, reaches the store instead of invalid_input.
    #[test]
    fn get_message_refuses_input_that_is_not_one_reference() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        for input in [
            serde_json::json!({"id": "m_aaaaaaaaaaaa"}),
            serde_json::json!({"handle": "#1", "id": "m_aaaaaaaaaaaa"}),
            serde_json::json!({"handle": ["#1"]}),
            serde_json::json!({"handle": "#0"}),
            serde_json::json!({}),
        ] {
            let got = get_message(&conn, &giga, mono_gate::parse_get(&input)).unwrap();
            assert_eq!(read_error(got).code, "invalid_input", "{input}");
        }
    }

    // PR I2 (intents review r1). Defect: a steer that waited across a
    // restart opens a turn as new work after the work it named is gone.
    #[test]
    fn startup_drops_every_steer_that_still_waits() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, queued) = steered(&conn, "queued");
        let (_, held) = steered(&conn, "held");
        assign_slot(&conn, &held, "turn-1", "main", Delivery::Steer, 101).unwrap();
        let plain = sent_to_a(&conn, "plain", false);
        startup(&conn, 200).unwrap();
        assert_eq!(stored_row(&conn, &queued).0, "dropped");
        assert_eq!(stored_row(&conn, &held).0, "dropped");
        assert_eq!(stored_row(&conn, &plain).0, "delivered");
    }

    // PR I2 (intents review r2 M3, decision 411). Defect: a dropped row that
    // keeps its opener slot fails the CHECK when the next call of that turn
    // confirms openers, so every call of the turn fails.
    #[test]
    fn confirm_openers_leaves_a_dropped_row_alone() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let held = handed_off(&conn, "held", false, "turn-1", "main", false);
        conn.execute(
            "UPDATE mono_messages SET status = 'dropped', closed_at = 5 WHERE id = ?1",
            [held.as_str()],
        )
        .unwrap();
        confirm_openers(&conn, &session("a-chat"), "turn-1", 6).unwrap();
        assert_eq!(stored_row(&conn, &held).2, None);
    }

    // PR I2 (review r1 m1). Many steers for different targets, in one fixed
    // shuffled order. The store mutex makes every submit serial, here as in
    // the app, so the order is what varies, not timing. Defect: a steer lands
    // in a session other than the one handling its reply_to (another Mono's
    // chat, the recipient's current chat, or the busiest session), or a steer
    // at queued work or at an older turn of a running session is let through.
    #[test]
    #[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
    fn a_shuffled_run_of_steers_reaches_only_the_sessions_that_handle_their_targets() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat"}', 2, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1),
                      ('a-old', '/', 'codex', 'm', 'default', 'A old', 1, 1);"#,
        )
        .unwrap();
        let to = |to: &str, request: &str, session: &str, turn: &str| {
            let id = sent(&conn, ("giga", "giga-chat"), to, request, false);
            if !turn.is_empty() {
                hand_off(&conn, &id, turn, "main", true);
            }
            (to.to_string(), id, session.to_string())
        };
        let live = [listed("project", true, true)];
        let project = accepted_ids(&call(
            &conn,
            &caller_in("giga", "giga-chat"),
            "to-p",
            Tool::SendMessage,
            hi("project", false),
            &live,
        ))
        .remove(0);
        hand_off(&conn, &project, "turn-3", "main", true);
        // A's earlier chat `a-old` still runs the work the Giga sent it.
        let chat_of_a = |chat: &str| {
            conn.execute(
                "UPDATE monos SET data_json = json_object('sessionId', ?1) WHERE id = 'mono-a'",
                [chat],
            )
            .unwrap()
        };
        chat_of_a("a-old");
        let in_old_chat = to("mono-a", "to-old-chat", "a-old", "turn-5");
        chat_of_a("a-chat");
        let targets = [
            to("mono-a", "to-a", "a-chat", "turn-1"),
            to("mono-b", "to-b", "b-chat", "turn-7"),
            ("project".to_string(), project, "project".to_string()),
            to("mono-a", "to-q", "a-chat", ""),
            in_old_chat,
            to("mono-a", "to-older-turn", "a-chat", "turn-0"),
        ];
        let running = [
            ("a-chat", "turn-1"),
            ("b-chat", "turn-7"),
            ("project", "turn-3"),
            ("a-old", "turn-5"),
        ];
        // Class i % 7: 0 to 2 name running work; 3 names queued work; 4 names
        // work that A's earlier chat runs, not its current chat; 5 names a
        // turn of A's chat older than the one it runs; 6 names B's work but
        // is sent to A.
        for (step, i) in (0..49).map(|i| (i * 17 + 5) % 49).enumerate() {
            let (to, target) = match i % 7 {
                6 => ("mono-a", &targets[1].1),
                class => (targets[class].0.as_str(), &targets[class].1),
            };
            let outcome = call_in_turns_at(
                &conn,
                &caller_in("giga", "giga-chat"),
                &format!("s{i}"),
                Tool::SendMessage,
                serde_json::json!({
                    "message": format!("s{i}-{to}"),
                    "send_to": to,
                    "wanting_reply": false,
                    "intent": "steer",
                    "reply_to": target.as_str(),
                }),
                &live,
                &running,
                // Each steer in a rate window of its own: S19 is not under test.
                100 + (step as i64 + 1) * mono_gate::RATE_WINDOW_MS,
            );
            let refused = match i % 7 {
                3 | 5 => Some("target_gone"),
                6 => Some("steer_target_mismatch"),
                _ => None,
            };
            if let Some(code) = refused {
                let error = failed(outcome);
                assert_eq!(error.code, code, "s{i}");
                let (status, _, _, reply_to) = stored_row(&conn, &error.message_id.unwrap());
                assert_eq!(
                    (status.as_str(), reply_to.as_deref()),
                    ("rejected", Some(target.as_str())),
                    "s{i}"
                );
                continue;
            }
            let handler = &targets[i % 7].2;
            assert_eq!(
                route_of(&outcome),
                Some(Routing {
                    session_id: session(handler),
                    via: Delivery::Steer
                }),
                "s{i}"
            );
            let id = accepted_ids(&outcome).remove(0);
            let message = get(&conn, &id).unwrap().unwrap();
            assert_eq!(message.body.text(), format!("s{i}-{to}"));
            assert_eq!(
                message.status.intent(),
                Some(&Intent::Steer(SteerTarget::Message(target.clone())))
            );
        }
        let per_session: HashMap<String, i64> = conn
            .prepare(
                "SELECT handled_by_session, count(*) FROM mono_messages
                  WHERE intent = 'steer' AND status <> 'rejected' GROUP BY 1",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            per_session,
            HashMap::from([
                ("a-chat".to_string(), 7),
                ("b-chat".to_string(), 7),
                ("project".to_string(), 7),
                ("a-old".to_string(), 7),
            ])
        );
    }

    /// The table as PR I1 (290183e) left it: intents, no `scope`.
    const I1_TABLE: &str = "CREATE TABLE IF NOT EXISTS mono_messages (
           id                 TEXT PRIMARY KEY CHECK (length(id) = 14 AND substr(id, 1, 2) = 'm_'),
           request_id         TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
           part               INTEGER NOT NULL DEFAULT 0 CHECK (part BETWEEN 0 AND 9),
           payload_hash       TEXT NOT NULL CHECK (length(payload_hash) = 64),
           tool               TEXT NOT NULL CHECK (tool IN ('send_message', 'reply', 'start_session', 'app_action')),
           from_mono_id       TEXT REFERENCES monos(id) ON DELETE SET NULL,
           from_session_id    TEXT NOT NULL,
           to_mono_id         TEXT REFERENCES monos(id) ON DELETE SET NULL,
           to_session_id      TEXT,
           addressed          TEXT NOT NULL,
           body               TEXT NOT NULL CHECK (length(body) <= 8000),
           body_chars         INTEGER NOT NULL CHECK (body_chars >= 0),
           wanting_reply      INTEGER NOT NULL CHECK (wanting_reply IN (0, 1)),
           intent             TEXT NOT NULL CHECK (intent IN ('new', 'reply', 'steer', 'continue')),
           reply_to           TEXT REFERENCES mono_messages(id),
           hop                INTEGER NOT NULL CHECK (hop >= 0),
           status             TEXT NOT NULL CHECK (status IN
                                ('open', 'answered', 'delivered', 'unanswered', 'dropped', 'rejected')),
           gate_result        TEXT NOT NULL,
           handled_by_session TEXT,
           delivery           TEXT CHECK (delivery IN ('submit', 'queue', 'steer')),
           turn_id            TEXT,
           slot_window        TEXT,
           handle             INTEGER CHECK (handle BETWEEN 1 AND 99),
           delivered_at       INTEGER,
           closed_at          INTEGER,
           notified_at        INTEGER,
           created_at         INTEGER NOT NULL,
           updated_at         INTEGER NOT NULL,
           UNIQUE (from_session_id, request_id, part),
           CHECK ((status = 'rejected') = (gate_result <> 'ok')),
           CHECK (status NOT IN ('open', 'unanswered') OR wanting_reply = 1),
           CHECK (status <> 'delivered' OR wanting_reply = 0),
           CHECK (handle IS NULL OR (turn_id IS NOT NULL AND wanting_reply = 1)),
           CHECK (status <> 'rejected' OR (handled_by_session IS NULL AND delivery IS NULL AND turn_id IS NULL)),
           CHECK (to_mono_id IS NULL OR to_session_id IS NULL),
           CHECK (status <> 'answered' OR wanting_reply = 1),
           CHECK (status IN ('rejected', 'dropped') OR (handled_by_session IS NOT NULL AND delivery IS NOT NULL)),
           CHECK (delivered_at IS NULL OR turn_id IS NOT NULL),
           CHECK (turn_id IS NULL OR wanting_reply = 0 OR handle IS NOT NULL),
           CHECK (status <> 'unanswered' OR delivered_at IS NOT NULL),
           CHECK ((slot_window IS NULL) = (turn_id IS NULL)),
           CHECK ((closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))),
           CHECK (status = 'rejected' OR body_chars = length(body)),
           CHECK (tool <> 'send_message' OR length(body) > 0),
           CHECK (notified_at IS NULL OR status IN ('unanswered', 'dropped')),
           CHECK (turn_id IS NULL OR delivery IN ('submit', 'steer')),
           CHECK (status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL),
           CHECK (part = 0 OR (tool = 'reply' AND status <> 'rejected')),
           CHECK (status = 'rejected' OR (reply_to IS NULL) = (hop = 0)),
           CHECK (status = 'rejected' OR (intent = 'new') = (reply_to IS NULL)),
           CHECK (status = 'rejected' OR intent <> 'continue'),
           CHECK (tool <> 'reply' OR intent = 'reply'),
           CHECK (tool <> 'send_message' OR status = 'rejected' OR intent IN ('new', 'steer')),
           CHECK (tool <> 'app_action' OR status = 'rejected'),
           CHECK (tool <> 'start_session' OR status = 'rejected' OR (intent = 'new' AND wanting_reply = 0 AND to_session_id IS NOT NULL)),
           CHECK (tool IN ('send_message', 'reply') OR intent = 'new'),
           CHECK (intent <> 'steer' OR status = 'rejected' OR delivery IN ('queue', 'steer')),
           CHECK (id GLOB 'm_[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
           CHECK (payload_hash NOT GLOB '*[^0-9a-f]*'),
           CHECK (request_id NOT GLOB '*[^A-Za-z0-9_-]*'),
           CHECK (length(body) = min(body_chars, 8000)),
           CHECK ((gate_result = 'too_large') = (body_chars > 8000))
         );";

    /// A store as PR I1 left it, with Monos `giga` and `mono-a`.
    fn i1_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::mono_roster::ensure_tables(&conn).unwrap();
        conn.execute_batch(I1_TABLE).unwrap();
        conn.execute_batch(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
             VALUES ('giga', 'G', 'giga', '{}', 0, 1, 1),
                    ('mono-a', 'A', 'mono', '{}', 1, 1, 1);",
        )
        .unwrap();
        conn
    }

    /// Every row by column name, without the columns in `without`.
    fn all_columns(conn: &Connection, without: &[&str]) -> Vec<Vec<(String, Sql)>> {
        let mut statement = conn
            .prepare("SELECT * FROM mono_messages ORDER BY id")
            .unwrap();
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        statement
            .query_map([], |row| {
                names
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| !without.contains(&name.as_str()))
                    .map(|(index, name)| Ok((name.clone(), row.get::<_, Sql>(index)?)))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    // Design 16.12.3.2 (whole-turn). Defect: an I1 database never gets the
    // columns, so every whole-turn insert fails; the rebuild loses or
    // changes a row; or it runs again on every open.
    #[test]
    fn the_rebuild_adds_scope_to_an_intents_table_once() {
        let conn = i1_database();
        raw_insert(&conn, &row("delivered")).unwrap();
        let named = |letter: char| text(id(letter).as_str());
        raw_insert(
            &conn,
            &with(
                row("delivered"),
                &[
                    ("id", named('b')),
                    ("request_id", text("req-b")),
                    ("intent", text("steer")),
                    ("reply_to", named('a')),
                    ("hop", Sql::Integer(1)),
                ],
            ),
        )
        .unwrap();
        raw_insert(
            &conn,
            &with(
                row("delivered"),
                &[
                    ("id", named('c')),
                    ("request_id", text("req-c")),
                    ("tool", text("reply")),
                    ("intent", text("reply")),
                    ("reply_to", named('a')),
                    ("hop", Sql::Integer(1)),
                ],
            ),
        )
        .unwrap();
        raw_insert(
            &conn,
            &with(
                row("rejected"),
                &[("id", named('d')), ("request_id", text("req-d"))],
            ),
        )
        .unwrap();
        let before = all_columns(&conn, &[]);
        ensure_tables(&conn).unwrap();
        assert!(has_column(&conn, "scope") && has_column(&conn, "scope_turn_id"));
        assert_eq!(foreign_keys(&conn), 1);
        assert_eq!(
            all_columns(&conn, &["scope", "scope_turn_id", "pin_turn_id"]),
            before
        );
        let scopes: Vec<(Option<String>, Option<String>)> = conn
            .prepare("SELECT scope, scope_turn_id FROM mono_messages")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(scopes, vec![(None, None); 4]);
        raw_insert(
            &conn,
            &with(
                row("delivered"),
                &[
                    ("id", named('e')),
                    ("request_id", text("req-e")),
                    ("intent", text("steer")),
                    ("scope", text("turn")),
                    ("scope_turn_id", text("turn-1")),
                ],
            ),
        )
        .unwrap();
        // A rebuild gives the table a new root page, so this shows the
        // second open rebuilt nothing.
        let root = || -> i64 {
            conn.query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'mono_messages'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let (schema_once, rows_once, root_once) = (schema(&conn), all_columns(&conn, &[]), root());
        ensure_tables(&conn).unwrap();
        assert_eq!(schema(&conn), schema_once);
        assert_eq!(all_columns(&conn, &[]), rows_once);
        assert_eq!(root(), root_once);
    }

    fn whole_turn(to: &str) -> serde_json::Value {
        serde_json::json!({"message": "Stop", "send_to": to, "wanting_reply": false, "intent": "steer", "scope": "turn"})
    }

    /// `scope` and `scope_turn_id` as stored.
    fn stored_scope(conn: &Connection, id: &MessageId) -> (Option<String>, Option<String>) {
        conn.query_row(
            "SELECT scope, scope_turn_id FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    /// A whole-turn steer of the Giga at `mono-a`, accepted while `a-chat`
    /// runs `turn-1`, not handed off yet.
    fn steered_turn(conn: &Connection, request_id: &str) -> MessageId {
        accepted_ids(&call_with(
            conn,
            &caller_in("giga", "giga-chat"),
            request_id,
            Tool::SendMessage,
            whole_turn("mono-a"),
            &[],
            &["a-chat"],
        ))
        .remove(0)
    }

    // Design 16.12.2.3, 16.12.3.1 (whole-turn). Defect: the steer fans out
    // to the Mono's sessions, is stored without its pin, or the web app
    // reads it as a steer with no target.
    #[test]
    fn an_accepted_whole_turn_steer_is_one_row_pinned_to_the_running_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let submitted = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "whole",
            Tool::SendMessage,
            whole_turn("mono-a"),
            &[listed("project", true, true)],
            &["a-chat", "project"],
        );
        assert_eq!(
            route_of(&submitted),
            Some(Routing {
                session_id: session("a-chat"),
                via: Delivery::Steer
            })
        );
        let steer = accepted_ids(&submitted).remove(0);
        let message = get(&conn, &steer).unwrap().unwrap();
        assert_eq!(message.hop, 0);
        assert_eq!(
            message.status.intent(),
            Some(&Intent::Steer(SteerTarget::Turn {
                turn_id: "turn-1".into()
            }))
        );
        assert_eq!(
            stored_row(&conn, &steer),
            ("delivered".into(), None, None, None)
        );
        assert_eq!(
            stored_scope(&conn, &steer),
            (Some("turn".into()), Some("turn-1".into()))
        );
        assert_eq!(stored_delivery(&conn, &steer).as_deref(), Some("queue"));
        let count = |filter: &str| -> i64 {
            conn.query_row(
                &format!("SELECT count(*) FROM mono_messages WHERE request_id = 'whole' {filter}"),
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(count(""), 1);
        assert_eq!(count("AND handled_by_session <> 'a-chat'"), 0);
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            (&json["intent"], &json["replyTo"], &json["scope"]),
            (
                &serde_json::json!("steer"),
                &serde_json::Value::Null,
                &serde_json::json!("turn")
            )
        );
        assert!(json.get("scopeTurnId").is_none(), "{json}");
    }

    // Design 16.12.2 (whole-turn). Defect: a refused whole-turn steer reads
    // as a refused steer with no target, or a steer with no target reads as
    // a whole-turn one.
    #[test]
    fn a_refused_whole_turn_steer_keeps_its_scope() {
        let untargeted = serde_json::json!({"message": "Stop", "send_to": "mono-a", "wanting_reply": false, "intent": "steer"});
        for (caller, input, running, code, scope) in [
            (
                ("giga", "giga-chat"),
                whole_turn("mono-a"),
                None,
                RejectCode::NothingRunning,
                Some(Scope::Turn),
            ),
            (
                ("mono-a", "a-chat"),
                whole_turn("giga"),
                Some("giga-chat"),
                RejectCode::SteerScopeNotAllowed,
                Some(Scope::Turn),
            ),
            (
                ("giga", "giga-chat"),
                untargeted,
                Some("a-chat"),
                RejectCode::SteerNeedsTarget,
                None,
            ),
        ] {
            let store = messaging_store();
            let conn = store.lock_conn().unwrap();
            let refused = rejected_row(
                &conn,
                call_with(
                    &conn,
                    &caller_in(caller.0, caller.1),
                    "refused",
                    Tool::SendMessage,
                    input,
                    &[],
                    running.as_slice(),
                ),
                code,
            );
            assert_eq!(
                refused.status,
                Status::Rejected {
                    gate_result: code,
                    wanting_reply: false,
                    intent: IntentKind::Steer,
                    reply_to: None,
                    scope,
                },
                "{code:?}"
            );
            assert_eq!(
                stored_scope(&conn, &refused.id),
                (scope.map(|scope| scope.as_str().to_string()), None),
                "{code:?}"
            );
        }
    }

    // Design 16.12.3.3 (whole-turn). Defect: a whole-turn steer joins a
    // later turn, or opens one as new work.
    #[test]
    fn a_whole_turn_steer_takes_a_slot_only_as_a_steer_into_its_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for (request, turn, delivery) in [
            ("other-turn", "turn-2", Delivery::Steer),
            ("opener", "turn-1", Delivery::Submit),
        ] {
            let steer = steered_turn(&conn, request);
            let (slot, notices) = assign_slot(&conn, &steer, turn, "main", delivery, 101).unwrap();
            assert_eq!(slot, None, "{request}");
            assert_eq!(notices.sessions, vec![session("giga-chat")], "{request}");
            assert_eq!(stored_row(&conn, &steer).0, "dropped", "{request}");
        }
        let steer = steered_turn(&conn, "right-turn");
        let (slot, notices) =
            assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        assert_eq!(notices, Notices::default());
        assert_eq!(
            slot.map(|assigned| assigned.message.id),
            Some(steer.clone())
        );
        assert_eq!(stored_delivery(&conn, &steer).as_deref(), Some("steer"));
    }

    // Design 16.12.3.3 (whole-turn). Defect: a whole-turn steer waits past
    // its turn with no notice, or another session's turn end drops it.
    #[test]
    fn the_turn_end_drops_a_waiting_whole_turn_steer_of_that_turn() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let steer = steered_turn(&conn, "steer");
        turn_ended(&conn, &session("a-chat"), "turn-2", 140).unwrap();
        turn_ended(&conn, &session("project"), "turn-1", 141).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "delivered");
        let notices = turn_ended(&conn, &session("a-chat"), "turn-1", 150).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "dropped");
        assert!(
            notices.sessions.contains(&session("giga-chat")),
            "{notices:?}"
        );
        assert!(pending_notices(&conn, &session("giga-chat"))
            .unwrap()
            .iter()
            .any(|message| message.id == steer));
    }

    // Design 16.12.3.1 (whole-turn). Defect: a closed window frees the steer
    // into the next turn.
    #[test]
    fn a_cleared_slot_keeps_the_pin() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let steer = steered_turn(&conn, "steer");
        let (slot, _) = assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 101).unwrap();
        assert!(slot.is_some());
        release_window_slots(&conn, "main", 102).unwrap();
        assert_eq!(stored_row(&conn, &steer).1, None);
        assert_eq!(stored_scope(&conn, &steer).1.as_deref(), Some("turn-1"));
        let (slot, notices) =
            assign_slot(&conn, &steer, "turn-2", "other", Delivery::Steer, 103).unwrap();
        assert_eq!(slot, None);
        assert_eq!(notices.sessions, vec![session("giga-chat")]);
        assert_eq!(stored_row(&conn, &steer).0, "dropped");
    }

    // Whole-turn review r1 M1 (its probe at 484b64f,
    // wtr1_probe_a_steer_accepted_after_its_turn_end_committed_is_not_dropped:
    // the row stays delivered). Defect: the gate reads `running` before the
    // store lock, a turn end commits in between, and the steer it accepts
    // for that turn waits with no notice, in a queue that Stop paused until
    // resume or restart.
    #[test]
    fn a_steer_accepted_after_its_turn_end_committed_is_dropped_by_the_submit() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let work = handed_off(&conn, "work", false, "turn-1", "main", true);
        let untaken = handed_off(&conn, "untaken", false, "turn-1", "main", false);
        turn_ended(&conn, &session("a-chat"), "turn-1", 150).unwrap();
        let ended = (stored_row(&conn, &work), stored_row(&conn, &untaken));
        let first: HashMap<String, String> = [("a-chat".to_string(), "turn-1".to_string())].into();
        for (request, input) in [
            ("whole", whole_turn("mono-a")),
            ("targeted", steering("mono-a", work.as_str())),
        ] {
            let mut submitted = call_in_turns(
                &conn,
                &caller_in("giga", "giga-chat"),
                request,
                Tool::SendMessage,
                input,
                &[],
                &[("a-chat", "turn-1")],
            );
            let steer = accepted_ids(&submitted).remove(0);
            // The probe: the gate accepted a steer for a turn that ended.
            assert_eq!(stored_row(&conn, &steer).0, "delivered", "{request}");
            // The second read still shows the turn: its own turn end follows.
            let kept =
                drop_steers_of_ended_turns(&conn, &mut submitted, &first, &first, 160).unwrap();
            assert_eq!(kept, Notices::default(), "{request}");
            assert_eq!(stored_row(&conn, &steer).0, "delivered", "{request}");
            assert!(route_of(&submitted).is_some(), "{request}");
            let notices =
                drop_steers_of_ended_turns(&conn, &mut submitted, &first, &HashMap::new(), 161)
                    .unwrap();
            assert!(
                notices.sessions.contains(&session("giga-chat")),
                "{request}: {notices:?}"
            );
            assert_eq!(stored_row(&conn, &steer).0, "dropped", "{request}");
            let Submitted::Accepted {
                messages, routes, ..
            } = &submitted
            else {
                panic!("{submitted:?}");
            };
            assert!(routes.is_empty(), "{request}");
            assert!(
                matches!(messages[0].message.status, Status::Dropped { .. }),
                "{request}"
            );
            assert_eq!(
                (stored_row(&conn, &work), stored_row(&conn, &untaken)),
                ended,
                "{request}"
            );
        }
    }

    // PR I2 review r2 m1. Defect: the command passes its first `running`
    // read twice, or skips the drop, so a steer accepted for a turn that
    // ended while the gate ran waits with no notice.
    #[test]
    fn the_submit_reads_running_again_and_drops_a_steer_whose_turn_ended() {
        let store = messaging_store();
        {
            let conn = store.lock_conn().unwrap();
            handed_off(&conn, "work", false, "turn-1", "main", true);
            turn_ended(&conn, &session("a-chat"), "turn-1", 150).unwrap();
        }
        let reads = std::cell::Cell::new(0);
        let running = || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                HashMap::from([("a-chat".to_string(), "turn-1".to_string())])
            } else {
                HashMap::new()
            }
        };
        let (submitted, notices) = submit_and_recheck(
            &store,
            running,
            &caller_in("giga", "giga-chat"),
            &RequestId::parse("whole").unwrap(),
            mono_gate::parse(Tool::SendMessage, &whole_turn("mono-a")),
            &[],
            &["codex".to_string()],
            160,
        )
        .unwrap();
        assert_eq!(reads.get(), 2);
        let Submitted::Accepted {
            messages, routes, ..
        } = &submitted
        else {
            panic!("{submitted:?}");
        };
        assert!(routes.is_empty(), "{routes:?}");
        assert!(matches!(messages[0].message.status, Status::Dropped { .. }));
        let conn = store.lock_conn().unwrap();
        assert_eq!(stored_row(&conn, &messages[0].message.id).0, "dropped");
        assert!(
            notices.sessions.contains(&session("giga-chat")),
            "{notices:?}"
        );
    }

    /// A `hi` from the Giga's chat to `mono-a` at `now`.
    fn giga_to_a_at(conn: &Connection, request_id: &str, now: i64) -> Submitted {
        call_at(
            conn,
            &caller_in("giga", "giga-chat"),
            request_id,
            Tool::SendMessage,
            hi("mono-a", false),
            now,
        )
    }

    // Design 6.1 "pair count" (S19). Defect: the gate gets no count, or not
    // the oldest counted send, so a Mono floods one recipient or is told to
    // wait the wrong time.
    #[test]
    fn a_monos_seventh_send_to_one_recipient_in_ten_minutes_is_rate_limited() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        // Ten seconds apart, so the hint shows which send it counts from.
        for n in 0..6 {
            accepted_ids(&giga_to_a_at(&conn, &format!("r{n}"), 100 + n * 10_000));
        }
        let refused = failed(giga_to_a_at(&conn, "r6", 60_100));
        assert_eq!(refused.code, "rate_limited");
        assert!(
            refused.hint.contains("Retry after 540 s"),
            "{}",
            refused.hint
        );
    }

    // Design 6.1: the count covers 10 minutes. Defect: older sends still
    // count, so a Mono stays limited after the window.
    #[test]
    fn a_send_after_the_ten_minute_window_is_accepted() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for request_id in ["r1", "r2", "r3", "r4", "r5", "r6"] {
            accepted_ids(&giga_to_a_at(&conn, request_id, 100));
        }
        assert_eq!(
            failed(giga_to_a_at(&conn, "inside", 100 + 599_999)).code,
            "rate_limited"
        );
        accepted_ids(&giga_to_a_at(&conn, "after", 100 + 600_001));
    }

    // Design 6.1 pair count (`created_at > now - 600 000`), PR 7 review r1
    // n5. Defect: a send exactly 10 minutes old still counts.
    #[test]
    fn a_send_exactly_ten_minutes_old_no_longer_counts() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for request_id in ["r1", "r2", "r3", "r4", "r5", "r6"] {
            accepted_ids(&giga_to_a_at(&conn, request_id, 100));
        }
        accepted_ids(&giga_to_a_at(&conn, "edge", 100 + 600_000));
    }

    // Design 6.1: the pair is (sender, recipient), PR7-4 review r1 m1.
    // Defect: the count drops the sender, so one Mono's sends rate-limit
    // every other Mono that writes to the same recipient.
    #[test]
    fn another_monos_sends_do_not_count_toward_the_rate_limit() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat","messageAllowlist":["mono-a"]}', 2, 1, 1);
               INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
               VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1);"#,
        )
        .unwrap();
        let b = caller_in("mono-b", "b-chat");
        for n in 0..6 {
            accepted_ids(&giga_to_a_at(&conn, &format!("g{n}"), 100));
        }
        for n in 0..6 {
            accepted_ids(&call(
                &conn,
                &b,
                &format!("b{n}"),
                Tool::SendMessage,
                hi("mono-a", false),
                &[],
            ));
        }
    }

    // Design 6.1: a project session is a recipient of its own, PR7-4 review
    // r1 m1. Defect: sends to a session never count, so a Mono floods a
    // project session; or every session shares one count.
    #[test]
    fn a_monos_seventh_send_to_one_project_session_is_rate_limited() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let live = [listed("project", true, true), listed("other", true, true)];
        let send = |request_id: &str, to: &str| {
            call(
                &conn,
                &giga,
                request_id,
                Tool::SendMessage,
                hi(to, false),
                &live,
            )
        };
        for n in 0..6 {
            accepted_ids(&send(&format!("p{n}"), "project"));
        }
        assert_eq!(failed(send("p6", "project")).code, "rate_limited");
        accepted_ids(&send("o0", "other"));
    }

    // Design 6.1: every accepted send counts, whatever its status, PR7-4
    // review r1 m1. Defect: only `delivered` rows count, so sends that want
    // a reply (`open`, `answered`, `unanswered`) never count.
    #[test]
    fn sends_that_want_a_reply_count_toward_the_rate_limit() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let ask = |request_id: &str| {
            call(
                &conn,
                &giga,
                request_id,
                Tool::SendMessage,
                hi("mono-a", true),
                &[],
            )
        };
        for n in 0..6 {
            accepted_ids(&ask(&format!("w{n}")));
        }
        assert_eq!(failed(ask("w6")).code, "rate_limited");
    }

    // Design 6.1: only accepted sends count. Defect: refused sends count, so
    // a Mono that fixes a refused call is then rate-limited.
    #[test]
    fn refused_sends_do_not_count_toward_the_rate_limit() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let long = serde_json::json!({"message": "x".repeat(8001), "send_to": "mono-a", "wanting_reply": false, "intent": "new"});
        for request_id in ["r1", "r2", "r3", "r4", "r5"] {
            accepted_ids(&call(
                &conn,
                &giga,
                request_id,
                Tool::SendMessage,
                hi("mono-a", false),
                &[],
            ));
        }
        for request_id in ["long-1", "long-2", "long-3"] {
            let error = failed(call(
                &conn,
                &giga,
                request_id,
                Tool::SendMessage,
                long.clone(),
                &[],
            ));
            assert_eq!(error.code, "too_large");
        }
        accepted_ids(&call(
            &conn,
            &giga,
            "r6",
            Tool::SendMessage,
            hi("mono-a", false),
            &[],
        ));
    }

    // Design 6.4: a reply is never rate-limited and never counts. Defect: a
    // Mono that answered its Giga's questions cannot send it new work.
    #[test]
    fn replies_do_not_count_toward_the_rate_limit() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let a = caller_in("mono-a", "a-chat");
        for n in 1..=3 {
            ask_delivered(&conn, &format!("ask-{n}"), "turn-1");
            accepted_ids(&call(
                &conn,
                &a,
                &format!("answer-{n}"),
                Tool::Reply,
                yes(),
                &[],
            ));
        }
        for n in 1..=6 {
            accepted_ids(&call(
                &conn,
                &a,
                &format!("send-{n}"),
                Tool::SendMessage,
                hi("giga", false),
                &[],
            ));
        }
    }

    // Design 6.1 pair count (the sender key of a Mono is its Mono), PR 7
    // review r1 m11. Defect: a Mono's chat and its habit runs count apart,
    // so together they send twice the limit.
    #[test]
    fn a_monos_chat_and_habit_runs_share_one_rate_count() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let chat = caller_in("giga", "giga-chat");
        let run = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("giga"),
            },
            ..caller_in("giga", "giga-run")
        };
        for n in 0..3 {
            for (caller, key) in [(&chat, "chat"), (&run, "run")] {
                accepted_ids(&call(
                    &conn,
                    caller,
                    &format!("{key}-{n}"),
                    Tool::SendMessage,
                    hi("mono-a", false),
                    &[],
                ));
            }
        }
        assert_eq!(
            failed(call(
                &conn,
                &chat,
                "seventh",
                Tool::SendMessage,
                hi("mono-a", false),
                &[]
            ))
            .code,
            "rate_limited"
        );
    }

    fn start_input(prompt: &str) -> serde_json::Value {
        serde_json::json!({"prompt": prompt, "harness": "codex", "model": "codex:gpt-5"})
    }

    /// Asserts each `(column, value)` of row `id`.
    fn assert_columns(conn: &Connection, id: &MessageId, expected: &[(&str, Sql)]) {
        for (column, value) in expected {
            let stored: Sql = conn
                .query_row(
                    &format!("SELECT {column} FROM mono_messages WHERE id = ?1"),
                    [id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(&stored, value, "{column}");
        }
    }

    fn failed_check(checked: ActionChecked) -> ToolError {
        match checked {
            ActionChecked::Failed { error } => error,
            allowed => panic!("{allowed:?}"),
        }
    }

    // Design 6.6 A3 and 13.5. Defect: a worker's start is refused without
    // its row, or the row is not the `start_session` refusal of 13.5.
    #[test]
    fn a_workers_start_writes_one_worker_cannot_spawn_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let input = start_input("Spawn a helper");
        let error = failed_check(
            check_action(
                &conn,
                &session_caller("worker", "/p"),
                "sessions.start",
                None,
                None,
                &RequestId::parse("spawn").unwrap(),
                &input,
                100,
            )
            .unwrap(),
        );
        assert_eq!(
            (error.code, error.duplicate),
            ("worker_cannot_spawn", false)
        );
        assert_eq!(rows(&conn), 1);
        let hash = mono_gate::action_hash("sessions.start", &input);
        assert_columns(
            &conn,
            error.message_id.as_ref().expect("a row"),
            &[
                ("request_id", text("spawn")),
                ("part", Sql::Integer(0)),
                ("payload_hash", text(hash.as_str())),
                ("tool", text("start_session")),
                ("from_mono_id", Sql::Null),
                ("from_session_id", text("worker")),
                ("to_mono_id", Sql::Null),
                ("to_session_id", Sql::Null),
                ("addressed", text("sessions.start")),
                ("body", text("Spawn a helper")),
                ("body_chars", Sql::Integer(14)),
                ("wanting_reply", Sql::Integer(0)),
                ("intent", text("new")),
                ("reply_to", Sql::Null),
                ("hop", Sql::Integer(0)),
                ("status", text("rejected")),
                ("gate_result", text("worker_cannot_spawn")),
                ("handled_by_session", Sql::Null),
                ("delivery", Sql::Null),
            ],
        );
    }

    // Design 6.6 (levels review r1). Defect: the A5 refusal of a Mono that
    // the roster lost fails the foreign key instead of writing its row.
    #[test]
    fn a_steer_via_lead_refusal_of_a_mono_whose_row_is_gone_has_no_mono() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        conn.execute("DELETE FROM monos WHERE id = 'mono-a'", [])
            .unwrap();
        let error = failed_check(
            check_action(
                &conn,
                &caller_in("mono-a", "a-chat"),
                "sessions.send",
                Some("worker"),
                None,
                &RequestId::parse("nudge").unwrap(),
                &serde_json::json!({"sessionId": "worker", "prompt": "Stop"}),
                100,
            )
            .unwrap(),
        );
        assert_eq!(error.code, "steer_via_lead");
        assert_columns(
            &conn,
            error.message_id.as_ref().expect("a row"),
            &[
                ("tool", text("app_action")),
                ("from_mono_id", Sql::Null),
                ("from_session_id", text("a-chat")),
                ("to_session_id", text("worker")),
                ("addressed", text("sessions.send worker")),
                ("body", text("Stop")),
                ("gate_result", text("steer_via_lead")),
            ],
        );
    }

    // Design 1.4 (levels review r1): an app action is only ever refused, and
    // a refused start row reads back as written. The CHECKs keep an accepted
    // app_action row out, so they are off for it. Defect: `parse_row` reads
    // an accepted app_action row.
    #[test]
    fn an_app_action_row_parses_only_as_rejected() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let refused = |letter, tool, gate_result, addressed: &str, body| MonoMessage {
            tool,
            from_mono_id: None,
            from_session_id: session("worker"),
            to: None,
            addressed: addressed.into(),
            body: Body::stored(body),
            ..message(
                letter,
                Status::Rejected {
                    gate_result,
                    wanting_reply: false,
                    intent: IntentKind::New,
                    reply_to: None,
                    scope: None,
                },
            )
        };
        for row in [
            refused(
                'a',
                Tool::AppAction,
                RejectCode::WorkerScope,
                "notes.list",
                "",
            ),
            refused(
                'b',
                Tool::StartSession,
                RejectCode::WorkerCannotSpawn,
                "sessions.start",
                "Spawn",
            ),
        ] {
            insert(&conn, &row).unwrap();
            assert_eq!(get(&conn, &row.id).unwrap(), Some(row));
        }
        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        raw_insert(
            &conn,
            &with(
                row("delivered"),
                &[
                    ("id", text(id('c').as_str())),
                    ("request_id", text("req-c")),
                    ("tool", text("app_action")),
                ],
            ),
        )
        .unwrap();
        let error = get(&conn, &id('c')).unwrap_err().to_string();
        assert!(error.contains("invalid mono_messages.tool"), "{error}");
    }

    /// A habit run of `mono-a` in session `a-run`, and an /operator `lead`.
    fn run_and_operator_lead() -> [Caller; 2] {
        [
            Caller {
                speaker: Speaker::Mono {
                    kind: CallerKind::HabitRun,
                    mono_id: mono("mono-a"),
                },
                session_id: session("a-run"),
                turn_id: "turn-1".into(),
            },
            Caller {
                speaker: Speaker::Session {
                    access: Access::Operator,
                    cwd: "/p".into(),
                },
                session_id: session("lead"),
                turn_id: "turn-1".into(),
            },
        ]
    }

    // Design 6.6 A5, PR 7 review r1 B1. Defect: a target that does not parse
    // reads as "no target", so A5 never runs for " worker" while the action
    // trims it and reaches the worker.
    #[test]
    fn check_action_refuses_a_target_that_is_no_session_id_and_writes_nothing() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let [run, operator_lead] = run_and_operator_lead();
        let input = serde_json::json!({"sessionId": " worker", "prompt": "Rebase first"});
        for caller in [caller_in("mono-a", "a-chat"), run, operator_lead] {
            for target in [" worker", "worker\n", ""] {
                let error = failed_check(
                    check_action(
                        &conn,
                        &caller,
                        "sessions.send",
                        Some(target),
                        None,
                        &RequestId::parse("send").unwrap(),
                        &input,
                        100,
                    )
                    .unwrap(),
                );
                assert_eq!(
                    error.code, "invalid_input",
                    "{:?} {target:?}",
                    caller.session_id
                );
            }
        }
        assert_eq!(rows(&conn), 0);
    }

    // Design 7, PR 7 review r1 m2. Defect: an app action of a caller whose
    // session a clear ended still runs its A rows, so an archived lead's
    // in-flight start makes a worker of it, and a cleared Mono chat's A5
    // send writes a rejected row (review r2 m8).
    #[test]
    fn app_calls_of_a_caller_a_clear_ended_are_unknown_token_and_write_nothing() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let lead = session_caller("lead", "/p");
        let mono = caller_in("mono-a", "a-chat");
        clear(&conn, "lead", 200);
        clear(&conn, "a-chat", 200);
        let codes = [
            check_action(
                &conn,
                &lead,
                "sessions.start",
                None,
                None,
                &RequestId::parse("start").unwrap(),
                &start_input("Build"),
                300,
            ),
            check_action(
                &conn,
                &mono,
                "sessions.send",
                Some("worker"),
                None,
                &RequestId::parse("send").unwrap(),
                &serde_json::json!({"sessionId": "worker", "prompt": "Rebase first"}),
                300,
            ),
        ]
        .map(|checked| failed_check(checked.unwrap()).code);
        assert_eq!(codes, ["unknown_token"; 2]);
        // PR7-10: a start whose caller a clear ended during the launch.
        assert_eq!(
            record_start(
                &conn,
                &lead,
                &session("w2"),
                &RequestId::parse("start").unwrap(),
                &start_input("Build"),
                300,
            )
            .unwrap(),
            StartRecorded::Failed {
                error: caller_ended()
            }
        );
        assert_eq!(rows(&conn), 0);
    }

    // Design 13.5, PR 7 review r1 m12. Defect: an A3 row stores the whole
    // prompt, so a prompt over 8000 code points fails the CHECK and the
    // refusal is an SQLite error.
    #[test]
    fn a_workers_long_start_prompt_still_writes_its_refusal() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let error = failed_check(
            check_action(
                &conn,
                &session_caller("worker", "/p"),
                "sessions.start",
                None,
                None,
                &RequestId::parse("spawn").unwrap(),
                &start_input(&"x".repeat(8001)),
                100,
            )
            .unwrap(),
        );
        assert_eq!(error.code, "worker_cannot_spawn");
        assert_columns(
            &conn,
            error.message_id.as_ref().expect("a row"),
            &[("body_chars", Sql::Integer(8000))],
        );
    }

    // Design 6.6 A4 and 13.5. Defect: the `app_action` row of a worker's
    // refused action keeps its prompt, or names the session the action named
    // as if the worker had messaged it.
    #[test]
    fn a_workers_app_action_row_keeps_no_prompt_and_names_no_session() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let error = failed_check(
            check_action(
                &conn,
                &session_caller("worker", "/p"),
                "sessions.read",
                Some("lead"),
                None,
                &RequestId::parse("peek").unwrap(),
                &serde_json::json!({"sessionId": "lead", "prompt": "Show me"}),
                100,
            )
            .unwrap(),
        );
        assert_eq!(error.code, "worker_scope");
        assert_columns(
            &conn,
            error.message_id.as_ref().expect("a row"),
            &[
                ("tool", text("app_action")),
                ("gate_result", text("worker_scope")),
                ("to_session_id", Sql::Null),
                ("addressed", text("sessions.read lead")),
                ("body", text("")),
                ("body_chars", Sql::Integer(0)),
            ],
        );
    }

    // Design 13.1 and 6.6, R4. Defect: `check_action` gives a target or a
    // caller with no `sessions` row a level from anything but a stored row,
    // or lets an unsaved caller act; or it gives a Mono chat its session's
    // level, so a Mono's start makes a worker of the chat.
    #[test]
    fn check_action_reads_levels_only_from_stored_rows_and_none_for_a_mono() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let chat_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'a-chat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(chat_rows, 1, "the Mono chat has a sessions row");
        let mono = caller_in("mono-a", "a-chat");
        // An unsaved target has no stored lead link: a lead, so A5 lets it.
        assert_eq!(
            check_action(
                &conn,
                &mono,
                "sessions.send",
                Some("ghost"),
                None,
                &RequestId::parse("nudge").unwrap(),
                &serde_json::json!({"sessionId": "ghost", "prompt": "Stop"}),
                100,
            )
            .unwrap(),
            ActionChecked::Allowed {
                lead_session_id: None,
                ask: None
            },
            "unsaved target"
        );
        let spawn = failed_check(
            check_action(
                &conn,
                &session_caller("ghost-w", "/p"),
                "sessions.start",
                None,
                None,
                &RequestId::parse("spawn").unwrap(),
                &start_input("Help"),
                100,
            )
            .unwrap(),
        );
        assert_eq!(
            (spawn.code, spawn.message_id),
            ("unknown_caller", None),
            "unsaved caller"
        );
        assert_eq!(
            check_action(
                &conn,
                &mono,
                "sessions.start",
                None,
                None,
                &RequestId::parse("start").unwrap(),
                &start_input("Build"),
                100,
            )
            .unwrap(),
            ActionChecked::Allowed {
                lead_session_id: None,
                ask: None
            },
            "Mono chat with a sessions row"
        );
        assert_eq!(rows(&conn), 0);
    }

    /// The start row `record_start` writes when `caller`'s `sessions.start`
    /// launched `started` with `prompt`.
    fn start_row(
        conn: &Connection,
        caller: &Caller,
        started: &str,
        request_id: &str,
        prompt: &str,
    ) -> MessageId {
        match record_start(
            conn,
            caller,
            &session(started),
            &RequestId::parse(request_id).unwrap(),
            &start_input(prompt),
            100,
        )
        .unwrap()
        {
            StartRecorded::Recorded { message_id } => message_id,
            failed => panic!("{failed:?}"),
        }
    }

    // Design 16.2 and 7. Defect: deleting or archiving a lead drops its start
    // row as a waiting message, and its Mono gets a `dropped` notice about
    // its own start prompt, which opens a Mono turn.
    #[test]
    fn clearing_a_lead_leaves_its_start_row_delivered_and_names_no_chat() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let start = start_row(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "lead",
            "start",
            "Build",
        );
        assert_eq!(clear(&conn, "lead", 200), Notices::default());
        assert_eq!(
            stored_row(&conn, &start),
            ("delivered".into(), None, None, None)
        );
        assert!(pending_notices(&conn, &session("a-chat"))
            .unwrap()
            .is_empty());
    }

    // Design 16.2. Defect: the store's open drops the start row of a lead
    // that `sessions` lacks, as if it were a message no one will take, and
    // its Mono gets a `dropped` notice.
    #[test]
    fn startup_keeps_the_start_row_of_a_lead_missing_from_sessions() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let start = start_row(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "gone",
            "start",
            "Build",
        );
        startup(&conn, 200).unwrap();
        assert_eq!(stored_row(&conn, &start).0, "delivered");
        assert!(pending_notices(&conn, &session("a-chat"))
            .unwrap()
            .is_empty());
    }

    // Design 16.2 and 5.5 rule 8. Defect: Remove drops the start row as a
    // queued message and gives its Mono a `dropped` notice.
    #[test]
    fn remove_of_a_start_row_answers_closed() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let start = start_row(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "lead",
            "start",
            "Build",
        );
        assert_eq!(
            drop_message(&conn, &start, 200).unwrap(),
            (DropAnswer::Closed, Notices::default())
        );
        assert_eq!(stored_row(&conn, &start).0, "delivered");
    }

    // Design 16.2. Defect: a hand-off takes the start row as a queue item
    // and gives the lead its start prompt again.
    #[test]
    fn assign_slot_refuses_a_start_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let start = start_row(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "lead",
            "start",
            "Build",
        );
        assert_eq!(
            assign_slot(&conn, &start, "turn-1", "main", Delivery::Submit, 200).unwrap(),
            (None, Notices::default())
        );
        assert_eq!(slot_columns(&conn, &start), (None, None, None));
    }

    // Design 16.2. Defect: reconcile of the lead queues its start row, so
    // the start prompt is delivered twice.
    #[test]
    fn reconcile_never_queues_the_start_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let mono = caller_in("mono-a", "a-chat");
        start_row(&conn, &mono, "lead", "start", "Build");
        let brief = accepted_ids(&call(
            &conn,
            &mono,
            "brief",
            Tool::SendMessage,
            hi("lead", false),
            &[listed("lead", true, true)],
        ))
        .remove(0);
        let queued: Vec<MessageId> = unqueued(&conn, &session("lead"))
            .unwrap()
            .into_iter()
            .map(|row| row.message.id)
            .collect();
        assert_eq!(queued, vec![brief]);
    }

    // Design 9. Defect: a retried start fails on the UNIQUE key, or never
    // learns the id of the row the first call wrote.
    #[test]
    fn record_start_twice_answers_the_first_message_id_and_keeps_one_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let mono = caller_in("mono-a", "a-chat");
        let first = start_row(&conn, &mono, "lead", "start", "Build");
        let again = record_start(
            &conn,
            &mono,
            &session("lead"),
            &RequestId::parse("start").unwrap(),
            &start_input("Build"),
            300,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            serde_json::json!({"outcome": "recorded", "messageId": first.as_str()})
        );
        assert_eq!(rows(&conn), 1);
    }

    // Design 9. Defect: a start under a request id that another call wrote
    // is answered with that call's row, so the model steers work its start
    // never began.
    #[test]
    fn record_start_under_a_key_with_other_input_is_request_id_reused() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let mono = caller_in("mono-a", "a-chat");
        start_row(&conn, &mono, "lead", "start", "Build");
        let before = dump(&conn);
        let other = record_start(
            &conn,
            &mono,
            &session("lead-2"),
            &RequestId::parse("start").unwrap(),
            &start_input("Build something else"),
            300,
        )
        .unwrap();
        assert_eq!(
            other,
            StartRecorded::Failed {
                error: refusal(
                    Refusal::RequestIdReused,
                    mono_gate::REQUEST_ID_REUSED.into()
                )
            }
        );
        assert_eq!(dump(&conn), before);
    }

    // #93 review r1 m1. Defect: a start the gate refused once, then allowed
    // under the same request id, is recorded as the refused row: the web app
    // hands the agent a message id whose row has no session, so a steer
    // through it is target_gone.
    #[test]
    fn record_start_never_answers_a_refused_row_under_the_key() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let key = RequestId::parse("spawn").unwrap();
        let input = start_input("Build");
        let check = |caller: &Caller| {
            check_action(
                &conn,
                caller,
                "sessions.start",
                None,
                None,
                &key,
                &input,
                100,
            )
            .unwrap()
        };
        conn.execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at,
                                   updated_at, lead_session_id)
             VALUES ('fresh', '/p', 'codex', 'm', 'default', 'F', 1, 1, 'lead')",
            [],
        )
        .unwrap();
        let lead = session_caller("fresh", "/p");
        let refused = failed_check(check(&lead));
        assert_eq!(refused.code, "worker_cannot_spawn");
        // Its lead is deleted, which clears the link: now a lead.
        conn.execute(
            "UPDATE sessions SET lead_session_id = NULL WHERE id = 'fresh'",
            [],
        )
        .unwrap();
        // PR 11's A1 answers the refusal again before any launch.
        let replayed = failed_check(check(&lead));
        assert_eq!(
            (replayed.code, replayed.message_id, replayed.duplicate),
            ("worker_cannot_spawn", refused.message_id.clone(), true)
        );
        assert_eq!(
            record_start(&conn, &lead, &session("app-fresh-spawn"), &key, &input, 200).unwrap(),
            StartRecorded::Failed {
                error: refusal(
                    Refusal::RequestIdReused,
                    mono_gate::REQUEST_ID_REUSED.into()
                )
            }
        );
        assert_eq!(rows(&conn), 1);
    }

    // Design 6.6 A8. Defect: the web app's `started` access reaches the gate
    // as another access, so an app-started lead lists notes; or an
    // /operator lead loses them.
    #[test]
    fn a_started_lead_from_the_web_app_gets_lead_scope_for_notes_list() {
        let store = messaging_store();
        seed_levels(&store.lock_conn().unwrap());
        let check = |access: &str| {
            serde_json::from_value::<AppActionCall>(serde_json::json!({
                "caller": {"kind": "session", "sessionId": "lead", "turnId": "turn-1",
                    "access": access, "cwd": "/p"},
                "action": "notes.list", "target": null,
                "startedSessionId": null, "requestId": format!("notes-{access}"), "input": {}
            }))
            .unwrap()
            .check(&store, 100)
            .unwrap()
        };
        assert_eq!(failed_check(check("started")).code, "lead_scope");
        assert_eq!(
            check("operator"),
            ActionChecked::Allowed {
                lead_session_id: None,
                ask: None
            }
        );
    }

    // #93 review r1 N3. Defect: a level gate command reads a call that names
    // an argument it does not know, so a renamed field on either side is
    // dropped (an `Option`) or defaulted instead of failing the call.
    #[test]
    fn the_level_gate_commands_refuse_an_unknown_argument() {
        let caller = serde_json::json!({
            "kind": "mono_chat", "monoId": "mono-a", "sessionId": "a-chat", "turnId": "turn-1"
        });
        let check = serde_json::json!({
            "caller": caller, "action": "sessions.list", "target": null,
            "requestId": "r1", "input": {}
        });
        let record = serde_json::json!({
            "caller": caller, "sessionId": "lead", "requestId": "r1", "input": {}
        });
        assert!(serde_json::from_value::<AppActionCall>(check.clone()).is_ok());
        assert!(serde_json::from_value::<RecordStartCall>(record.clone()).is_ok());
        for (mut call, kind) in [(check, "check"), (record, "record")] {
            call["leadCopy"] = serde_json::json!("lead");
            let parsed = match kind {
                "check" => serde_json::from_value::<AppActionCall>(call).err(),
                _ => serde_json::from_value::<RecordStartCall>(call).err(),
            };
            assert!(
                parsed.is_some_and(|error| error.to_string().contains("unknown field")),
                "{kind}"
            );
        }
    }

    // Design 16.2 and 13.5. Defect: the start row stores the prompt's real
    // length, which the CHECK ties to `too_large`, so a long start prompt
    // fails; its hash is not the check's, so A1 cannot match it; or a column
    // differs from the row a steer and the delivery paths read.
    #[test]
    fn record_start_writes_the_accepted_start_session_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let prompt = "é".repeat(8001);
        let start = start_row(
            &conn,
            &caller_in("mono-a", "a-chat"),
            "lead",
            "start",
            &prompt,
        );
        let hash = mono_gate::action_hash("sessions.start", &start_input(&prompt));
        assert_columns(
            &conn,
            &start,
            &[
                ("request_id", text("start")),
                ("part", Sql::Integer(0)),
                ("payload_hash", text(hash.as_str())),
                ("tool", text("start_session")),
                ("from_mono_id", text("mono-a")),
                ("from_session_id", text("a-chat")),
                ("to_mono_id", Sql::Null),
                ("to_session_id", text("lead")),
                ("addressed", text("sessions.start")),
                ("body", text(&"é".repeat(8000))),
                ("body_chars", Sql::Integer(8000)),
                ("wanting_reply", Sql::Integer(0)),
                ("intent", text("new")),
                ("reply_to", Sql::Null),
                ("scope", Sql::Null),
                ("hop", Sql::Integer(0)),
                ("status", text("delivered")),
                ("gate_result", text("ok")),
                ("handled_by_session", text("lead")),
                ("delivery", text("submit")),
                ("turn_id", Sql::Null),
                ("slot_window", Sql::Null),
                ("handle", Sql::Null),
                ("delivered_at", Sql::Null),
                ("closed_at", Sql::Null),
                ("created_at", Sql::Integer(100)),
            ],
        );
        let worker = start_row(&conn, &session_caller("lead", "/p"), "worker", "start", "");
        assert_columns(
            &conn,
            &worker,
            &[
                ("from_mono_id", Sql::Null),
                ("from_session_id", text("lead")),
                ("to_session_id", text("worker")),
                ("body", text("")),
                ("body_chars", Sql::Integer(0)),
            ],
        );
    }

    // Design 9 and 16.2. Defect: no row names the work a start began, or a
    // steer through it is not pinned to the turn the lead runs (PR 8's
    // `pin_turn_id`), so a Mono cannot steer the lead it started. A lead that
    // runs no turn has no work to steer (S15); a cleared lead is no
    // recipient (S5).
    #[test]
    fn a_mono_steers_its_lead_through_the_start_row_while_the_lead_runs() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let mono = caller_in("mono-a", "a-chat");
        let start = start_row(&conn, &mono, "lead", "start", "Build");
        let steer = |request_id: &str, running: &[&str]| {
            call_with(
                &conn,
                &mono,
                request_id,
                Tool::SendMessage,
                steering("lead", start.as_str()),
                &[listed("lead", true, true)],
                running,
            )
        };
        let running = steer("running", &["lead"]);
        assert_eq!(
            route_of(&running),
            Some(Routing {
                session_id: session("lead"),
                via: Delivery::Steer
            })
        );
        let row = get(&conn, &accepted_ids(&running)[0]).unwrap().unwrap();
        assert_eq!(
            (row.hop, row.status.intent()),
            (
                1,
                Some(&Intent::Steer(SteerTarget::Pinned {
                    message: start.clone(),
                    turn_id: "turn-1".into()
                }))
            )
        );
        assert_eq!(failed(steer("idle", &[])).code, "target_gone");
        clear(&conn, "lead", 200);
        assert_eq!(failed(steer("ended", &["lead"])).code, "unknown_target");
    }

    /// The Mono's accepted steer into `lead`, running turn-1, through its
    /// start row.
    fn steer_through_start_row(conn: &Connection) -> (MessageId, MessageId) {
        seed_levels(conn);
        let mono = caller_in("mono-a", "a-chat");
        let start = start_row(conn, &mono, "lead", "start", "Build");
        let steer = accepted_ids(&call_with(
            conn,
            &mono,
            "steer",
            Tool::SendMessage,
            steering("lead", start.as_str()),
            &[listed("lead", true, true)],
            &["lead"],
        ))
        .remove(0);
        (start, steer)
    }

    // Design 16.2 and 16.6. Defect: a steer joins only its target's turn and
    // a start row has none, so the hand-off drops the Mono's accepted steer
    // into its running lead; or the pin lets a later turn take it.
    #[test]
    fn a_monos_steer_through_the_start_row_is_delivered_in_the_leads_running_turn() {
        {
            let store = messaging_store();
            let conn = store.lock_conn().unwrap();
            let (_, steer) = steer_through_start_row(&conn);
            let (slot, notices) =
                assign_slot(&conn, &steer, "turn-2", "main", Delivery::Steer, 150).unwrap();
            assert!(slot.is_none());
            assert!(notices.sessions.contains(&session("a-chat")), "{notices:?}");
            assert_eq!(stored_row(&conn, &steer).0, "dropped");
        }
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (start, steer) = steer_through_start_row(&conn);
        let (slot, notices) =
            assign_slot(&conn, &steer, "turn-1", "main", Delivery::Steer, 150).unwrap();
        assert_eq!(notices, Notices::default());
        assert_eq!(slot.map(|row| row.message.id), Some(steer.clone()));
        mark_delivery(&conn, &steer, "turn-1", 151).unwrap();
        assert_eq!(
            stored_row(&conn, &steer),
            (
                "delivered".into(),
                Some("turn-1".into()),
                Some(151),
                Some(start.as_str().into())
            )
        );
    }

    // Design 16.6. Defect: the turn end selects waiting steers only through
    // their target's turn, so a steer through a start row outlives the
    // lead's turn and the next turn's hand-off takes it.
    #[test]
    fn a_steer_through_a_start_row_still_waiting_when_the_leads_turn_ends_is_dropped() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let (_, steer) = steer_through_start_row(&conn);
        let notices = turn_ended(&conn, &session("lead"), "turn-1", 200).unwrap();
        assert_eq!(stored_row(&conn, &steer).0, "dropped");
        assert!(notices.sessions.contains(&session("a-chat")), "{notices:?}");
        assert!(unqueued(&conn, &session("lead")).unwrap().is_empty());
    }

    /// The session id `caller`'s `sessions.start` with `key` launches
    /// (`appLaunchId` in agentApp.ts).
    fn launch(caller: &Caller, key: &RequestId) -> SessionId {
        session(&format!(
            "app-{}-{}",
            caller.session_id.as_str(),
            key.as_str()
        ))
    }

    // Design 9 and 6.6 A1. Defect: the start row's hash is not the check's,
    // so a lead's retry of a start it launched is refused, or the retry loses
    // the lead id the first call wrote; or other input under the key passes.
    #[test]
    fn a_leads_repeated_start_passes_a1_with_its_own_lead_id() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let lead = session_caller("lead", "/p");
        let key = RequestId::parse("spawn").unwrap();
        let check = |input: &serde_json::Value| {
            check_action(
                &conn,
                &lead,
                "sessions.start",
                None,
                Some(&launch(&lead, &key)),
                &key,
                input,
                100,
            )
            .unwrap()
        };
        let allowed = ActionChecked::Allowed {
            lead_session_id: Some(session("lead")),
            ask: Some(StartAsk {
                ask_id: start_message_id(&session("lead"), &key),
                due_in_ms: Some(3_600_000),
            }),
        };
        assert_eq!(check(&start_input("Fix the build")), allowed);
        start_row(
            &conn,
            &lead,
            launch(&lead, &key).as_str(),
            "spawn",
            "Fix the build",
        );
        assert_eq!(check(&start_input("Fix the build")), allowed);
        assert_eq!(
            failed_check(check(&start_input("Fix the tests"))),
            refusal(
                Refusal::RequestIdReused,
                mono_gate::REQUEST_ID_REUSED.into()
            )
        );
        assert_eq!(rows(&conn), 1);
    }

    // Design 9, A1. Defect: a retry of a start whose session the user
    // archived or deleted passes the key check, so `host.start` launches the
    // id again.
    #[test]
    fn a_start_retried_after_its_session_ended_is_refused_before_any_launch() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        seed_levels(&conn);
        let ended = refusal(Refusal::RequestIdReused, mono_gate::START_ENDED.into());
        for (caller, key) in [
            (caller_in("mono-a", "a-chat"), "start"),
            (session_caller("lead", "/p"), "spawn"),
        ] {
            let key = RequestId::parse(key).unwrap();
            let started = launch(&caller, &key);
            start_row(&conn, &caller, started.as_str(), key.as_str(), "Build");
            clear(&conn, started.as_str(), 200);
            assert_eq!(
                failed_check(
                    check_action(
                        &conn,
                        &caller,
                        "sessions.start",
                        None,
                        Some(&started),
                        &key,
                        &start_input("Build"),
                        300,
                    )
                    .unwrap()
                ),
                ended,
                "{:?}",
                caller.session_id
            );
        }
        assert_eq!(rows(&conn), 2);
    }

    // Design 9, A1 (a draft, or a failed record, writes no row). Defect: a
    // retry of a start with no row whose session the user deleted passes,
    // so `host.start` launches the deleted id again; or the command drops
    // the `startedSessionId` the web app sends.
    #[test]
    fn a_rowless_start_whose_session_ended_is_refused_before_any_launch() {
        let store = messaging_store();
        seed_levels(&store.lock_conn().unwrap());
        let ended = refusal(Refusal::RequestIdReused, mono_gate::START_ENDED.into());
        for (caller, from, lead, started) in [
            (
                serde_json::json!({"kind": "mono_chat", "monoId": "mono-a", "sessionId": "a-chat", "turnId": "turn-1"}),
                "a-chat",
                None,
                "app-a-chat-r1",
            ),
            (
                serde_json::json!({"kind": "session", "access": "started", "cwd": "/p", "sessionId": "lead", "turnId": "turn-1"}),
                "lead",
                Some(session("lead")),
                "app-lead-r1",
            ),
        ] {
            // The command the web app calls, with the id `appLaunchId` sends.
            let check = || {
                serde_json::from_value::<AppActionCall>(serde_json::json!({
                    "caller": caller, "action": "sessions.start", "target": null,
                    "startedSessionId": started,
                    "requestId": "r1", "input": start_input("Fix CI")
                }))
                .unwrap()
                .check(&store, 300)
                .unwrap()
            };
            assert_eq!(
                check(),
                ActionChecked::Allowed {
                    lead_session_id: lead,
                    ask: Some(StartAsk {
                        ask_id: start_message_id(&session(from), &RequestId::parse("r1").unwrap()),
                        due_in_ms: Some(3_600_000),
                    }),
                }
            );
            clear(&store.lock_conn().unwrap(), started, 200);
            assert_eq!(failed_check(check()), ended, "{started}");
        }
        assert_eq!(rows(&store.lock_conn().unwrap()), 0);
    }

    /// `command`'s answer to `args` through Tauri's IPC, as the web app calls it.
    #[cfg(not(windows))]
    pub(crate) fn invoke_command(
        webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        command: &str,
        args: &Value,
    ) -> Result<Value, Value> {
        tauri::test::get_ipc_response(webview, ipc_request(command, args))
            .map(|body| body.deserialize::<Value>().unwrap())
    }

    /// The IPC message the web app sends to call `command` with `args`.
    #[cfg(not(windows))]
    pub(crate) fn ipc_request(command: &str, args: &Value) -> tauri::webview::InvokeRequest {
        tauri::webview::InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(args.clone()),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        }
    }

    // #91 N1 and #97 N4. Tauri binds each command argument by its parameter
    // name. Defect: a command's parameter is renamed from `call` (or the web
    // app sends another key), so every app action or start record fails in
    // the app while every other test, which calls the Rust functions or
    // mocks `invoke`, passes. Not on Windows: see `tauri` under
    // `[target.'cfg(not(windows))'.dev-dependencies]` in Cargo.toml.
    #[cfg(not(windows))]
    #[test]
    fn each_level_gate_command_binds_the_call_key_the_web_app_sends() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../src/features/agent-app/model/appActionCheck.fixtures.json"
        ))
        .unwrap();
        let calls: Vec<Value> = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["name"].as_str().unwrap().starts_with("A6"))
            .flat_map(|case| case["calls"].as_array().unwrap().clone())
            .collect();
        // R4: the caller is judged by its stored row, so it needs one.
        let app = tauri::test::mock_builder()
            .manage(fixture_store(&fixture))
            .invoke_handler(tauri::generate_handler![
                super::mono_app_action_check,
                super::mono_message_record_start
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
            .build()
            .unwrap();
        let invoke = |command: &str, args: &Value| invoke_command(&webview, command, args);
        let commands: Vec<&str> = calls
            .iter()
            .map(|call| call["command"].as_str().unwrap())
            .collect();
        assert_eq!(
            commands,
            ["mono_app_action_check", "mono_message_record_start"]
        );
        for call in &calls {
            let command = call["command"].as_str().unwrap();
            let answer = invoke(command, &call["args"]).unwrap();
            assert_eq!(answer["outcome"], call["reply"]["outcome"], "{command}");
        }
    }

    /// A store with a `sessions` row for each `saved` session and a `monos`
    /// row for each Mono of the fixture (`appActionCheck.fixtures.json`).
    fn fixture_store(fixture: &Value) -> SessionStore {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        for row in fixture["sessions"].as_array().unwrap() {
            if row["saved"] == true {
                conn.execute(
                    "INSERT OR IGNORE INTO sessions (id, cwd, harness, model, runtime_mode,
                       title, created_at, updated_at, lead_session_id)
                     VALUES (?1, ?2, 'codex', 'm', 'default', 'S', 1, 1, ?3)",
                    params![
                        row["id"].as_str(),
                        row["cwd"].as_str(),
                        row["leadSessionId"].as_str()
                    ],
                )
                .unwrap();
            }
        }
        for mono in fixture["monos"].as_array().unwrap() {
            let data = serde_json::json!({
                "sessionId": mono["sessionId"],
                "projects": mono["projects"]
            })
            .to_string();
            conn.execute(
                "INSERT OR REPLACE INTO monos (id, name, role, data_json, sort_order,
                   created_at, updated_at)
                 VALUES (?1, ?1, ?2, ?3, 0, 1, 1)",
                params![mono["id"].as_str(), mono["role"].as_str(), data],
            )
            .unwrap();
        }
        drop(conn);
        store
    }

    // PR7-9b and PR7-10 real-state proof. `appActionCheck.fixtures.json`
    // holds the exact `mono_app_action_check` and `mono_message_record_start`
    // calls that the web app's real `levelGate` sends (agentAppLevels.test.ts
    // records and checks them) and the replies it acts on. Each case's calls
    // run in order on one store. With WRITE_APP_ACTION_FIXTURE=1 this test
    // writes each reply the commands answer into the file instead of
    // comparing, and drops the calls after a refusal. Defect: a command reads
    // another argument name, shape or lead copy than the web app sends,
    // answers in a shape or with a code, hint, lead id or message id the web
    // app does not expect, a repeated start writes a second row, or the Giga
    // row reads another role than the caller's `monos` row.
    #[test]
    fn mono_app_action_check_answers_every_call_the_web_app_sends() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/features/agent-app/model/appActionCheck.fixtures.json"
        );
        let write = std::env::var("WRITE_APP_ACTION_FIXTURE").as_deref() == Ok("1");
        let mut fixture: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let world =
            serde_json::json!({ "sessions": fixture["sessions"], "monos": fixture["monos"] });
        for case in fixture["cases"].as_array_mut().unwrap() {
            let store = fixture_store(&world);
            let name = case["name"].as_str().unwrap().to_string();
            // The ids the replies name, in the order they first appear; the
            // fixture writes the n-th as `m_` and the n-th letter 12 times.
            let mut ids: Vec<MessageId> = Vec::new();
            let calls = case["calls"].as_array_mut().unwrap();
            // A step starts with its check; after a refusal the web app makes
            // no more calls in that step (agentAppLevels.test.ts checks it).
            let mut kept = vec![true; calls.len()];
            let mut refused = false;
            for (index, call) in calls.iter_mut().enumerate() {
                let checks = call["command"] == "mono_app_action_check";
                if refused && !checks {
                    kept[index] = false;
                    continue;
                }
                // Tauri hands the command the `call` key of the invoke arguments.
                let args = call["args"].as_object().unwrap();
                assert_eq!(args.keys().collect::<Vec<_>>(), ["call"], "{name}");
                let arg = args["call"].clone();
                let mut answer = match call["command"].as_str().unwrap() {
                    "mono_app_action_check" => serde_json::to_value(
                        serde_json::from_value::<AppActionCall>(arg)
                            .unwrap()
                            .check(&store, 100)
                            .unwrap(),
                    ),
                    "mono_message_record_start" => serde_json::to_value(
                        serde_json::from_value::<RecordStartCall>(arg)
                            .unwrap()
                            .record(&store, 100)
                            .unwrap(),
                    ),
                    other => panic!("{name}: unknown command {other}"),
                }
                .unwrap();
                // A check's start ask names the row its record writes next.
                for pointer in ["/error/message_id", "/messageId", "/ask/askId"] {
                    if let Some(id) = answer.pointer_mut(pointer) {
                        let written = MessageId::parse(id.as_str().unwrap()).unwrap();
                        let conn = store.lock_conn().unwrap();
                        let row = get(&conn, &written).unwrap();
                        assert!(row.is_some() || pointer == "/ask/askId", "{name}");
                        let index = match ids.iter().position(|seen| *seen == written) {
                            Some(index) => index,
                            None => {
                                ids.push(written);
                                ids.len() - 1
                            }
                        };
                        let letter = char::from(b'a' + u8::try_from(index).unwrap());
                        *id = serde_json::json!(format!("m_{}", letter.to_string().repeat(12)));
                    }
                }
                refused = answer["outcome"] == "failed";
                if write {
                    call["reply"] = answer;
                } else {
                    assert_eq!(answer, call["reply"], "{name}");
                }
            }
            if write {
                let mut index = 0;
                calls.retain(|_| {
                    index += 1;
                    kept[index - 1]
                });
            } else {
                assert!(
                    kept.iter().all(|kept| *kept),
                    "{name}: a call after a refusal"
                );
            }
            // One row per id: a repeated start keeps its first row.
            let rows: i64 = store
                .lock_conn()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM mono_messages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(usize::try_from(rows).unwrap(), ids.len(), "{name}");
        }
        if write {
            std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
        }
    }

    // Defect: the gate takes the role from anywhere but the caller's monos
    // row, so it refuses the real Giga or lets a real Mono file without a
    // rejected row.
    #[test]
    #[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
    fn gate_on_a_copy_of_the_app_database() {
        let store = SessionStore::open(std::env::var("MONOCODE_DB_COPY").unwrap().into()).unwrap();
        let run = uuid::Uuid::new_v4().simple().to_string();
        let check = |role: &str| {
            let (mono, data): (String, String) = store
                .lock_conn()
                .unwrap()
                .query_row(
                    "SELECT id, data_json FROM monos
                     WHERE role = ?1 AND json_extract(data_json, '$.sessionId') IS NOT NULL
                     ORDER BY sort_order, id LIMIT 1",
                    [role],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let data: Value = serde_json::from_str(&data).unwrap();
            let call: AppActionCall = serde_json::from_value(serde_json::json!({
                "caller": {"kind": "mono_chat", "monoId": mono, "sessionId": data["sessionId"], "turnId": "t"},
                "action": "approvals.file",
                "target": null,
                "startedSessionId": null,
                "requestId": format!("{run}-{role}"),
                "input": {"heading": "Pick a store"}
            }))
            .unwrap();
            serde_json::to_value(call.check(&store, now_ms()).unwrap()).unwrap()
        };
        assert_eq!(
            check("giga"),
            serde_json::json!({"outcome": "allowed", "leadSessionId": null})
        );
        let refused = check("mono");
        assert_eq!(refused["error"]["code"], "not_a_giga", "{refused}");
        let rows: Vec<(String, String)> = store
            .lock_conn()
            .unwrap()
            .prepare("SELECT status, gate_result FROM mono_messages WHERE request_id LIKE ?1")
            .unwrap()
            .query_map([format!("{run}-%")], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, [("rejected".to_string(), "not_a_giga".to_string())]);
    }
}
