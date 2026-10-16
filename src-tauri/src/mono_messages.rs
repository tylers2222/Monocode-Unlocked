//! Messages between Monos in `monocode.db`, one row per message and part.
//!
//! The CHECKs make every state the domain forbids unwritable, so a writer that
//! forgets a column fails at the write. [`parse_row`] is the one place a row
//! becomes a [`MonoMessage`]; [`insert`] writes only what that type can hold.
//! Session ids are plain text, not foreign keys: sender sessions can be
//! unpersisted, and a Mono reset deletes its chat.

use std::collections::HashMap;
use std::fmt;

use rusqlite::{
    params, Connection, OptionalExtension, Row, ToSql, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::control::ControlHost;
use crate::mono_gate::{
    self, Caller, CallerKind, Closure, Decision, Facts, HandleState, Handled, Input, OpenHandle,
    Recipient, Referenced, Refusal, ReplyFacts, Route, SendFacts, Target, TargetSession,
};
use crate::mono_roster::validate_mono_id;
use crate::session_store::{validate_id, SessionStore};

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    rebuild_for_intents(conn)?;
    conn.execute_batch(&table_sql("mono_messages"))?;
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
         );"
    )
}

const INDEXES: &str = "
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

/// Design 16.2: the PR 3 table (`action_type`, `steers`) becomes the intents
/// table. Keyed on the `steers` column, so it runs once. Foreign keys are off
/// for the copy (decision 236): `DROP TABLE` under them fails once a row
/// names another, and the pragma is a no-op inside a transaction. A failure
/// rolls back to the old table, and the next open tries again.
fn rebuild_for_intents(conn: &Connection) -> rusqlite::Result<()> {
    let old: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('mono_messages') WHERE name = 'steers')",
        [],
        |row| row.get(0),
    )?;
    if !old {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let rebuilt = copy_into_intents_table(conn);
    let restored = conn.execute_batch("PRAGMA foreign_keys = ON");
    rebuilt.and(restored)
}

fn copy_into_intents_table(conn: &Connection) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch("DROP TABLE IF EXISTS mono_messages_new")?;
    tx.execute_batch(&table_sql("mono_messages_new"))?;
    tx.execute_batch(
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
         FROM mono_messages m;
         DROP TABLE mono_messages;
         ALTER TABLE mono_messages_new RENAME TO mono_messages;",
    )?;
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
    Steer(MessageId),
}

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
            Intent::New => None,
            Intent::Reply(id) | Intent::Steer(id) => Some(id),
        }
    }

    /// The two columns of an accepted row; `None` when they disagree.
    fn of_columns(kind: IntentKind, reply_to: Option<MessageId>) -> Option<Self> {
        match (kind, reply_to) {
            (IntentKind::New, None) => Some(Intent::New),
            (IntentKind::Reply, Some(id)) => Some(Intent::Reply(id)),
            (IntentKind::Steer, Some(id)) => Some(Intent::Steer(id)),
            _ => None,
        }
    }
}

/// Flat as `intent` and `replyTo`, the shape a rejected row has too.
impl Serialize for Intent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("intent", &self.kind())?;
        map.serialize_entry("replyTo", &self.reply_to())?;
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
text_enum!(RejectCode {
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
    RateLimited = "rate_limited",
    HopLimit = "hop_limit",
    NoOpenHandle = "no_open_handle",
    UnknownHandle = "unknown_handle",
    HandleClosed = "handle_closed",
    AmbiguousReply = "ambiguous_reply",
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
    /// is a foreign key.
    #[serde(rename_all = "camelCase")]
    Rejected {
        gate_result: RejectCode,
        wanting_reply: bool,
        intent: IntentKind,
        reply_to: Option<MessageId>,
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

    /// The `intent` and `reply_to` columns.
    fn intent_columns(&self) -> (IntentKind, Option<&MessageId>) {
        match self {
            Status::Rejected {
                intent, reply_to, ..
            } => (*intent, reply_to.as_ref()),
            Status::Open { intent, .. }
            | Status::Delivered { intent, .. }
            | Status::Answered { intent, .. }
            | Status::Unanswered { intent, .. }
            | Status::Dropped { intent, .. } => (intent.kind(), intent.reply_to()),
        }
    }
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

pub fn insert(conn: &Connection, message: &MonoMessage) -> rusqlite::Result<()> {
    let columns = StatusColumns::of(&message.status);
    let (intent, reply_to) = message.status.intent_columns();
    let (to_mono_id, to_session_id) = match &message.to {
        Some(Party::Mono { mono_id }) => (Some(mono_id.as_str()), None),
        Some(Party::Session { session_id }) => (None, Some(session_id.as_str())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO mono_messages (
           id, request_id, part, payload_hash, tool, from_mono_id, from_session_id,
           to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
           intent, reply_to, hop, status, gate_result, handled_by_session,
           delivery, turn_id, slot_window, handle, delivered_at, closed_at, notified_at,
           created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?27)",
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
            intent.as_str(),
            reply_to.map(MessageId::as_str),
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
        ],
    )?;
    Ok(())
}

const SELECT: &str = "SELECT id, request_id, part, payload_hash, tool, from_mono_id,
       from_session_id, to_mono_id, to_session_id, addressed, body, body_chars,
       wanting_reply, intent, reply_to, hop, status, gate_result,
       handled_by_session, delivery, turn_id, slot_window, handle, delivered_at,
       closed_at, notified_at, created_at
  FROM mono_messages";

pub fn get(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<MonoMessage>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [id.as_str()], parse_row)
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
    let intent = || required(Intent::of_columns(intent_kind, reply_to.clone()), "intent");
    let status = match (required(text("status")?, "status")?.as_str(), wanting_reply) {
        ("rejected", _) => Status::Rejected {
            gate_result: required(
                text("gate_result")?.as_deref().and_then(RejectCode::parse),
                "gate_result",
            )?,
            wanting_reply,
            intent: intent_kind,
            reply_to: reply_to.clone(),
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
    Ok(MonoMessage {
        id: required(parsed(row, "id", MessageId::parse)?, "id")?,
        request_id: required(parsed(row, "request_id", RequestId::parse)?, "request_id")?,
        part: required(Part::new(row.get("part")?), "part")?,
        payload_hash: required(
            parsed(row, "payload_hash", PayloadHash::parse)?,
            "payload_hash",
        )?,
        tool: required(text("tool")?.as_deref().and_then(Tool::parse), "tool")?,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallerArg {
    kind: String,
    mono_id: String,
    session_id: String,
    turn_id: String,
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
    fn parse(self) -> Result<Caller, String> {
        Ok(Caller {
            kind: match self.kind.as_str() {
                "mono_chat" => CallerKind::MonoChat,
                "habit_run" => CallerKind::HabitRun,
                _ => return Err("Invalid caller kind".into()),
            },
            mono_id: MonoId::parse(&self.mono_id).ok_or("Invalid caller Mono id")?,
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
        messages: Vec<MonoMessage>,
        /// The messages this reply answered, with their handles.
        answered: Vec<Answered>,
        duplicate: bool,
        routes: Vec<RouteGroup>,
    },
    Failed {
        error: ToolError,
    },
}

fn refused(code: Refusal, hint: String) -> Submitted {
    Submitted::Failed {
        error: ToolError {
            code: code.as_str(),
            hint,
            message_id: None,
            open_handles: None,
            duplicate: false,
        },
    }
}

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
        messages,
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
fn new_message_id() -> MessageId {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    // Bytes 9 to 15 and the low half of byte 0 are random in a v4 uuid.
    let bits = bytes[9..]
        .iter()
        .fold(u64::from(bytes[0] & 0x0f), |bits, byte| {
            bits << 8 | u64::from(*byte)
        });
    let tail: String = (0..12)
        .map(|index| char::from(ALPHABET[((bits >> (5 * index)) & 31) as usize]))
        .collect();
    MessageId::parse(&format!("m_{tail}")).expect("base32 id")
}

/// The caller's `monos` row: `None` when it is gone, else its current chat.
fn mono_chat(conn: &Connection, mono_id: &MonoId) -> rusqlite::Result<Option<Option<SessionId>>> {
    let data: Option<String> = conn
        .query_row(
            "SELECT data_json FROM monos WHERE id = ?1",
            [mono_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(data.map(|data| chat_of(&data)))
}

/// `data_json.sessionId`, a Mono's current chat.
fn chat_of(data: &str) -> Option<SessionId> {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|data| data.get("sessionId")?.as_str().and_then(SessionId::parse))
}

/// The Mono whose current chat is `chat`.
fn mono_of_chat(conn: &Connection, chat: &SessionId) -> rusqlite::Result<Option<MonoId>> {
    let monos = conn
        .prepare("SELECT id, data_json FROM monos")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(monos
        .into_iter()
        .find(|(_, data)| chat_of(data).as_ref() == Some(chat))
        .and_then(|(id, _)| MonoId::parse(&id)))
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

fn recipient(conn: &Connection, send_to: &str, live: &Live) -> rusqlite::Result<Recipient> {
    if let Some(mono_id) = MonoId::parse(send_to) {
        if let Some(chat) = mono_chat(conn, &mono_id)? {
            let chat = chat
                .map(|session| target(conn, session, live))
                .transpose()?;
            return Ok(Recipient::Mono { mono_id, chat });
        }
    }
    if let Some(session) = SessionId::parse(send_to) {
        let own_project = live
            .sessions
            .iter()
            .any(|entry| entry.session_id == send_to && entry.own_project);
        let target = target(conn, session, live)?;
        if target.exists {
            return Ok(Recipient::Session {
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
            Some(mono_id) => conn
                .query_row(
                    "SELECT name FROM monos WHERE id = ?1",
                    [mono_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_default(),
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
    let hash = input.payload_hash();
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    // Design 7 (PR 6 review r1 m1): the control host let this call through
    // before the clear forgot its session, and it reaches here after the clear
    // committed. A row written now would never be dropped.
    if session_ended(&tx, &caller.session_id)? {
        return Ok(Submitted::Failed {
            error: ToolError {
                code: "unknown_token",
                hint: crate::control::UNKNOWN_TOKEN.to_string(),
                message_id: None,
                open_handles: None,
                duplicate: false,
            },
        });
    }
    // Design 5.4 (2): a call bound to the turn shows that its provider runs
    // it, so it took the text that opened it.
    confirm_openers(&tx, &caller.session_id, &caller.turn_id, now)?;
    let caller_row = mono_chat(&tx, &caller.mono_id)?;
    let prior = get_by_request(&tx, &caller.session_id, request_id)?;
    let mut referenced_id = None;
    let decision = match &input {
        Input::Send(send) => {
            let recipient = recipient(&tx, &send.send_to, live)?;
            let reply_to = match &send.reply_to {
                Some(id) => resolve_ref(&tx, id)?
                    .map(|message| referenced(&tx, caller, &recipient, message, live))
                    .transpose()?,
                None => None,
            };
            referenced_id = reply_to.as_ref().map(|found| found.message.id.clone());
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
                    },
                },
            )
        }
        Input::Reply(reply) => {
            let then = match prior.first() {
                Some(first) if matches!(first.status, Status::Rejected { .. }) => {
                    handled_at(&tx, caller, first.created_at, live)?
                }
                _ => Vec::new(),
            };
            mono_gate::decide_reply(
                reply,
                &hash,
                &Facts {
                    caller: caller_row,
                    prior,
                    about: ReplyFacts {
                        handled: handled_in_turn(&tx, caller, live)?,
                        then,
                    },
                },
            )
        }
    };
    let submitted = record(
        &tx,
        caller,
        request_id,
        &input,
        hash,
        decision,
        referenced_id,
        now,
    )?;
    tx.commit()?;
    Ok(submitted)
}

/// Design 16.6: the accepted row `id` names. A steer's `reply_to` is a
/// message id only (PR I2 review r1).
fn resolve_ref(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<MonoMessage>> {
    Ok(get(conn, id)?.filter(|message| !matches!(message.status, Status::Rejected { .. })))
}

/// `message` with the session target_session gives for it, as live sees it.
fn referenced(
    conn: &Connection,
    caller: &Caller,
    recipient: &Recipient,
    message: MonoMessage,
    live: &Live,
) -> rusqlite::Result<Referenced> {
    let session = match mono_gate::target_session(caller, recipient, &message) {
        TargetSession::Receiver {
            session: Some(session),
            ..
        } => Some(target(conn, session, live)?),
        _ => None,
    };
    Ok(Referenced { message, session })
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
#[allow(clippy::too_many_arguments)]
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
    // A rejected row keeps the intent the call asked for, so a refused steer
    // stays a steer (design 16.2).
    let (message, addressed, wanting_reply, asked) = match input {
        Input::Send(send) => (
            &send.message,
            send.send_to.as_str(),
            send.wanting_reply,
            send.intent,
        ),
        Input::Reply(reply) => (&reply.message, "", reply.wanting_reply, IntentKind::Reply),
    };
    let row = |part: Part, to: Option<Party>, hop: u32, status| MonoMessage {
        id: new_message_id(),
        request_id: request_id.clone(),
        part,
        payload_hash: hash.clone(),
        tool: input.tool(),
        from_mono_id: Some(caller.mono_id.clone()),
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
            open_handles,
        } => {
            let message = row(
                Part::FIRST,
                to,
                0,
                Status::Rejected {
                    gate_result: code,
                    wanting_reply,
                    intent: asked,
                    reply_to,
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

/// Design 5.5 rule 3: before a hand-off removes a queue item or sends its
/// text, row `id` takes a slot of turn `turn_id`, set by `window`. Same row,
/// same turn: the slot it has. Returns the row with its slot, or `None` when
/// the row is no longer `open` or `delivered`, was already accepted, or holds
/// another turn's slot; the caller then removes the item and delivers nothing
/// (5.1). A steer row takes a slot only as a steer into the turn that runs
/// the work it names; any other hand-off drops it, and its sender is owed a
/// notice (design 16.6).
pub fn assign_slot(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    window: &str,
    delivery: Delivery,
    now: i64,
) -> rusqlite::Result<(Option<MonoMessage>, Notices)> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(message) = get(&tx, id)? else {
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
        let target_turn: Option<String> = tx.query_row(
            "SELECT turn_id FROM mono_messages WHERE id = ?1",
            [target.as_str()],
            |row| row.get(0),
        )?;
        if delivery != Delivery::Steer || target_turn.as_deref() != Some(turn_id) {
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
/// queue item nor a slot holds, oldest first.
pub fn unqueued(conn: &Connection, session: &SessionId) -> rusqlite::Result<Vec<MonoMessage>> {
    conn.prepare(&format!(
        "{SELECT} WHERE handled_by_session = ?1 AND status IN ('open', 'delivered')
           AND delivered_at IS NULL AND turn_id IS NULL
         ORDER BY created_at, id"
    ))?
    .query_map([session.as_str()], parse_row)?
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
}

impl Notices {
    fn name(&mut self, session: SessionId) {
        if !self.sessions.contains(&session) {
            self.sessions.push(session);
        }
    }

    pub fn merge(&mut self, other: Notices) {
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
                if let Some(chat) = mono_chat(conn, &mono_id)?.flatten() {
                    self.name(chat);
                }
            }
        }
        Ok(())
    }

    /// To every window; nothing when no session is named.
    pub fn emit(&self, app: &AppHandle) {
        if !self.sessions.is_empty() {
            let _ = app.emit("mono-message-notices", self);
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
/// that is over, so it is dropped too.
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
                AND reply_to IN (SELECT id FROM mono_messages
                                  WHERE handled_by_session = ?2 AND turn_id = ?3)
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
    if changed + waiting.len() > 0 {
        notices.name(session.clone());
    }
    notices.name_senders(&tx, &waiting)?;
    notices.name_senders(&tx, &unanswered)?;
    tx.commit()?;
    Ok(notices)
}

/// Design 2 (review r5): window `label` runs no turn any more, so the slots
/// it set that no provider took are free again. Names their sessions.
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
    Ok(notices)
}

/// Design 5.3, when `SessionStore` opens: no turn outlives the app, so every
/// slot's turn has ended. A receiver that `sessions` no longer has (or whose
/// worktree was removed) is never loaded again to take its messages, so they
/// are dropped and their senders are owed a notice. No window listens yet,
/// and every window reconciles when it boots, so this names no session.
pub fn startup(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    end_turn_rows(&tx, "turn_id IS NOT NULL", params![now])?;
    // A steer still waiting for a hand-off names work no turn runs now.
    tx.execute(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
          WHERE intent = 'steer' AND status IN ('open', 'delivered') AND delivered_at IS NULL",
        [now],
    )?;
    tx.execute(
        "UPDATE mono_messages SET status = 'dropped', closed_at = ?1, updated_at = ?1
          WHERE (status = 'open' OR (status = 'delivered' AND delivered_at IS NULL))
            AND NOT EXISTS (SELECT 1 FROM sessions
                             WHERE sessions.id = mono_messages.handled_by_session
                               AND worktree_removed = 0)",
        [now],
    )?;
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
        .prepare(
            "UPDATE mono_messages
                SET status = 'dropped', handled_by_session = NULL, closed_at = ?2, updated_at = ?2
              WHERE handled_by_session = ?1
                AND (status = 'open' OR (status = 'delivered' AND delivered_at IS NULL))
              RETURNING id",
        )?
        .query_map(params![session.as_str(), now], |row| {
            row.get::<_, String>(0)
        })?
        .map(|id| id.and_then(|id| required(MessageId::parse(&id), "id")))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    notices.name_senders(tx, &dropped)?;
    Ok(notices)
}

/// Design 7: an archived session was unarchived, so it can receive again.
pub(crate) fn session_reopened(conn: &Connection, session_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM mono_session_ends WHERE session_id = ?1",
        [session_id],
    )?;
    Ok(())
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
    let slot = get(&tx, id)?.and_then(|message| match message.status {
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
        tx.execute(
            "UPDATE mono_messages SET status = 'dropped', closed_at = ?2, updated_at = ?2
              WHERE id = ?1",
            params![id.as_str(), now],
        )?;
        notices.name_senders(&tx, std::slice::from_ref(id))?;
    }
    tx.commit()?;
    Ok((answer, notices))
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

/// The route of a new accept, sent to the window that owns its session.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RouteEvent<'a> {
    messages: &'a [MonoMessage],
    route: &'a Routing,
    /// The window that made the call; the owner tells it the delivery.
    caller_window: &'a str,
}

#[allow(clippy::too_many_arguments)]
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
    let tool = Tool::parse(&tool).ok_or("Unknown messaging tool")?;
    let Some(request_id) = RequestId::parse(&request_id) else {
        return Ok(refused(
            Refusal::InvalidInput,
            "A request id has 1 to 128 letters, digits, - and _.".into(),
        ));
    };
    let running = control.running_sessions();
    let mut submitted = {
        let conn = store.lock_conn()?;
        submit(
            &conn,
            &caller,
            &request_id,
            mono_gate::parse(tool, &input),
            &Live {
                sessions: &live,
                running: &running,
                steerable_harnesses: &steerable_harnesses,
            },
            now_ms(),
        )
        .map_err(|error| error.to_string())?
    };
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
            let rows: Vec<MonoMessage> = messages
                .iter()
                .filter(|message| group.message_ids.contains(&message.id))
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
) -> Result<Option<MonoMessage>, String> {
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
) -> Result<Vec<MonoMessage>, String> {
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
        const MAPPED: [&str; 5] = ["action_type", "steers", "intent", "reply_to", "hop"];
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

    // Design 16.2 map. Defects: a wrong CASE line, a lost row, or a column
    // value lost or moved by the copy.
    #[test]
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
                intent: Intent::Steer(target.clone()),
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
            "status = 'rejected' OR (intent = 'new') = (reply_to IS NULL)",
        );
        assert_check_refuses(
            "delivered",
            &[("intent", text("steer"))],
            "status = 'rejected' OR (intent = 'new') = (reply_to IS NULL)",
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
            kind: CallerKind::MonoChat,
            mono_id: mono(mono_id),
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
            100,
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
                messages.iter().map(|message| message.id.clone()).collect()
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
        let [reply] = messages.as_slice() else {
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
            .map(|message| message.id)
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
                .expect("a slot");
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
            sorted(ids(unqueued(&conn, &session("a-chat")).unwrap())),
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
            sorted(ids(unqueued(&conn, &session("a-chat")).unwrap())),
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
               VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat"}', 2, 1, 1);
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
            .map(|message| {
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
        let group = |id: &str, via, message: &MonoMessage| RouteGroup {
            route: Routing {
                session_id: session(id),
                via,
            },
            message_ids: vec![message.id.clone()],
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
            kind: CallerKind::HabitRun,
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
            Some(Intent::Steer(work))
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
            kind: CallerKind::HabitRun,
            mono_id: mono("mono-a"),
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
    fn stored_row(
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
        assert_eq!(message.status.intent(), Some(&Intent::Steer(work.clone())));
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
        assert_eq!(slot.map(|message| message.id), Some(steer.clone()));
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
            .all(|message| message.id != steer));
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
        for i in (0..49).map(|i| (i * 17 + 5) % 49) {
            let (to, target) = match i % 7 {
                6 => ("mono-a", &targets[1].1),
                class => (targets[class].0.as_str(), &targets[class].1),
            };
            let outcome = call_in_turns(
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
                Some(&Intent::Steer(target.clone()))
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
}
