//! Messages between Monos in `monocode.db`, one row per message and part.
//!
//! The CHECKs make every state the domain forbids unwritable, so a writer that
//! forgets a column fails at the write. [`parse_row`] is the one place a row
//! becomes a [`MonoMessage`]; [`insert`] writes only what that type can hold.
//! Session ids are plain text, not foreign keys: sender sessions can be
//! unpersisted, and a Mono reset deletes its chat.

use std::collections::HashSet;
use std::fmt;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use crate::control::ControlHost;
use crate::mono_gate::{
    self, Caller, CallerKind, Decision, Facts, Input, OpenHandle, Recipient, Refusal, Route,
    Target, Waiting,
};
use crate::mono_roster::validate_mono_id;
use crate::session_store::{validate_id, SessionStore};

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_messages (
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
           ON mono_messages (reply_to) WHERE reply_to IS NOT NULL;
         -- A session that was cleared or deleted. The gate trusts this over
         -- the live session list from the web app.
         CREATE TABLE IF NOT EXISTS mono_session_ends (
           session_id TEXT PRIMARY KEY,
           ended_at   INTEGER NOT NULL
         );",
    )
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
            fn parse(text: &str) -> Option<Self> {
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
text_enum!(ActionType {
    Queue = "queue",
    Steer = "steer",
});
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
    AmbiguousSteer = "ambiguous_steer",
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
    #[serde(rename_all = "camelCase")]
    Rejected {
        gate_result: RejectCode,
        wanting_reply: bool,
    },
    #[serde(rename_all = "camelCase")]
    Open {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<WaitingSlot>,
    },
    #[serde(rename_all = "camelCase")]
    Delivered {
        wanting_reply: Wanting<false>,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<PlainSlot>,
    },
    #[serde(rename_all = "camelCase")]
    Answered {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        routed: Routed,
        slot: Option<WaitingSlot>,
        closed_at: i64,
    },
    #[serde(rename_all = "camelCase")]
    Unanswered {
        wanting_reply: Wanting<true>,
        #[serde(flatten)]
        routed: Routed,
        slot: DeliveredSlot,
        closed_at: i64,
        notified_at: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    Dropped {
        wanting_reply: bool,
        closed_at: i64,
        notified_at: Option<i64>,
    },
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
    pub action_type: Option<ActionType>,
    pub reply_to: Option<MessageId>,
    pub steers: Option<MessageId>,
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
    let (to_mono_id, to_session_id) = match &message.to {
        Some(Party::Mono { mono_id }) => (Some(mono_id.as_str()), None),
        Some(Party::Session { session_id }) => (None, Some(session_id.as_str())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO mono_messages (
           id, request_id, part, payload_hash, tool, from_mono_id, from_session_id,
           to_mono_id, to_session_id, addressed, body, body_chars, wanting_reply,
           action_type, reply_to, steers, hop, status, gate_result, handled_by_session,
           delivery, turn_id, slot_window, handle, delivered_at, closed_at, notified_at,
           created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?28)",
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
            message.action_type.map(ActionType::as_str),
            message.reply_to.as_ref().map(MessageId::as_str),
            message.steers.as_ref().map(MessageId::as_str),
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
       wanting_reply, action_type, reply_to, steers, hop, status, gate_result,
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
    let status = match (required(text("status")?, "status")?.as_str(), wanting_reply) {
        ("rejected", _) => Status::Rejected {
            gate_result: required(
                text("gate_result")?.as_deref().and_then(RejectCode::parse),
                "gate_result",
            )?,
            wanting_reply,
        },
        ("open", true) => Status::Open {
            wanting_reply: Wanting,
            routed: routed()?,
            slot: waiting_slot()?,
        },
        ("delivered", false) => Status::Delivered {
            wanting_reply: Wanting,
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
            routed: routed()?,
            slot: waiting_slot()?,
            closed_at: closed_at()?,
        },
        ("unanswered", true) => Status::Unanswered {
            wanting_reply: Wanting,
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
        action_type: text("action_type")?
            .map(|action| ActionType::parse(&action).ok_or_else(|| invalid("action_type")))
            .transpose()?,
        reply_to: parsed(row, "reply_to", MessageId::parse)?,
        steers: parsed(row, "steers", MessageId::parse)?,
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
}

/// What the web app and `ControlHost` know about sessions, read before the
/// store transaction opens (design 5.3 lock order).
pub struct Live<'a> {
    pub sessions: &'a [LiveSession],
    /// Sessions with an authorized turn (`ControlHost.active`).
    pub running: &'a HashSet<String>,
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
    /// routed (design 5.5 rule 1).
    #[serde(rename_all = "camelCase")]
    Accepted {
        messages: Vec<MonoMessage>,
        /// The messages this reply answered, with their handles.
        answered: Vec<Answered>,
        duplicate: bool,
        /// The route of a new accept; a duplicate has none.
        #[serde(skip_serializing_if = "Option::is_none")]
        route: Option<Routing>,
        /// Set when another window owns the route's session and runs it
        /// (design 6); the caller window then routes nothing.
        #[serde(skip_serializing_if = "Option::is_none")]
        owner_window: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answered {
    pub handle: Option<Handle>,
    pub message_id: MessageId,
}

fn accept_outcome(
    conn: &Connection,
    messages: Vec<MonoMessage>,
    route: Option<Routing>,
) -> rusqlite::Result<Submitted> {
    let mut answered = Vec::new();
    for message_id in messages
        .iter()
        .filter_map(|message| message.reply_to.clone())
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
        duplicate: route.is_none(),
        route,
        owner_window: None,
    })
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
    Ok(data.map(|data| {
        serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|data| data.get("sessionId")?.as_str().and_then(SessionId::parse))
    }))
}

fn target(conn: &Connection, session: SessionId, live: &Live) -> rusqlite::Result<Target> {
    let listed = live
        .sessions
        .iter()
        .find(|entry| entry.session_id == session.as_str());
    // The live list is a snapshot from before an await, so a clear that ran
    // since wins over it.
    let ended = conn
        .query_row(
            "SELECT 1 FROM mono_session_ends WHERE session_id = ?1",
            [session.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
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
        running: live.running.contains(session.as_str()),
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

/// The messages delivered to `session` that still wait for its reply: the
/// provider took them (`delivered_at`), in any turn until PR 5.
fn waiting(conn: &Connection, session: &SessionId, live: &Live) -> rusqlite::Result<Vec<Waiting>> {
    let messages = conn
        .prepare(&format!(
            "{SELECT} WHERE handled_by_session = ?1 AND status = 'open'
               AND turn_id IS NOT NULL AND delivered_at IS NOT NULL
             ORDER BY created_at, id"
        ))?
        .query_map([session.as_str()], parse_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    messages
        .into_iter()
        .filter_map(|message| match &message.status {
            Status::Open {
                slot: Some(slot), ..
            } => Some((slot.handle, message)),
            _ => None,
        })
        .map(|(handle, message)| {
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
            let sender = target(conn, message.from_session_id.clone(), live)?;
            Ok(Waiting {
                message,
                handle,
                from,
                sender,
            })
        })
        .collect()
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
    let caller_row = mono_chat(&tx, &caller.mono_id)?;
    let prior = get_by_request(&tx, &caller.session_id, request_id)?;
    let decision = match &input {
        Input::Send(send) => mono_gate::decide_send(
            caller,
            send,
            &hash,
            &Facts {
                caller: caller_row,
                prior,
                about: recipient(&tx, &send.send_to, live)?,
            },
        ),
        Input::Reply(reply) => mono_gate::decide_reply(
            reply,
            &hash,
            &Facts {
                caller: caller_row,
                prior,
                about: waiting(&tx, &caller.session_id, live)?,
            },
        ),
    };
    let submitted = record(&tx, caller, request_id, &input, hash, decision, now)?;
    tx.commit()?;
    Ok(submitted)
}

/// An accepted row's `delivery` is `queue` until a hand-off writes `submit`
/// or `steer` (design 1.2 review r2); the route goes back in [`Routing`].
fn accepted(wanting_reply: bool, route: &Route) -> Status {
    let routed = Routed {
        handled_by_session: route.session.clone(),
        delivery: Delivery::Queue,
    };
    if wanting_reply {
        Status::Open {
            wanting_reply: Wanting,
            routed,
            slot: None,
        }
    } else {
        Status::Delivered {
            wanting_reply: Wanting,
            routed,
            slot: None,
        }
    }
}

fn record(
    tx: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: &Input,
    hash: PayloadHash,
    decision: Decision,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let (message, addressed, wanting_reply, action_type) = match input {
        Input::Send(send) => (
            &send.message,
            send.send_to.as_str(),
            send.wanting_reply,
            Some(send.action_type),
        ),
        Input::Reply(reply) => (&reply.message, "", reply.wanting_reply, None),
    };
    let row = |part: Part, to: Option<Party>, reply_to: Option<MessageId>, hop: u32, status| {
        MonoMessage {
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
            action_type,
            reply_to,
            steers: None,
            hop,
            created_at: now,
            status,
        }
    };
    match decision {
        Decision::Duplicate { prior } => Ok(match &prior[0].status {
            Status::Rejected { gate_result, .. } => Submitted::Failed {
                error: ToolError {
                    code: gate_result.as_str(),
                    hint: gate_result.hint().into(),
                    message_id: Some(prior[0].id.clone()),
                    open_handles: None,
                    duplicate: true,
                },
            },
            _ => accept_outcome(tx, prior, None)?,
        }),
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
                None,
                0,
                Status::Rejected {
                    gate_result: code,
                    wanting_reply,
                },
            );
            insert(tx, &message)?;
            let lists_handles = matches!(
                code,
                RejectCode::NoOpenHandle | RejectCode::AmbiguousReply | RejectCode::UnknownHandle
            );
            Ok(Submitted::Failed {
                error: ToolError {
                    code: code.as_str(),
                    hint,
                    message_id: Some(message.id),
                    open_handles: lists_handles.then_some(open_handles),
                    duplicate: false,
                },
            })
        }
        Decision::Accept {
            to,
            route,
            hop,
            reply_to,
        } => {
            let threads: Vec<Option<MessageId>> = if reply_to.is_empty() {
                vec![None]
            } else {
                reply_to.into_iter().map(Some).collect()
            };
            let mut messages = Vec::with_capacity(threads.len());
            for (part, thread) in (0..).zip(threads) {
                let message = row(
                    Part::new(part).expect("at most 10 parts"),
                    Some(to.clone()),
                    thread.clone(),
                    hop,
                    accepted(wanting_reply, &route),
                );
                insert(tx, &message)?;
                if let Some(answered) = thread {
                    tx.execute(
                        "UPDATE mono_messages SET status = 'answered', closed_at = ?2, updated_at = ?2
                          WHERE id = ?1 AND status = 'open'",
                        params![answered.as_str(), now],
                    )?;
                }
                messages.push(message);
            }
            accept_outcome(
                tx,
                messages,
                Some(Routing {
                    session_id: route.session,
                    via: route.via.delivery(),
                }),
            )
        }
    }
}

/// Design 5.5 rule 3: before a hand-off removes a queue item or sends its
/// text, row `id` takes a slot of turn `turn_id`, set by `window`. Same row,
/// same turn: the slot it has. False when the row is no longer `open` or
/// `delivered`, was already accepted, or holds another turn's slot; the
/// caller then removes the item and delivers nothing (5.1).
pub fn assign_slot(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    window: &str,
    delivery: Delivery,
    now: i64,
) -> rusqlite::Result<bool> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(message) = get(&tx, id)? else {
        return Ok(false);
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
        _ => return Ok(false),
    };
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
    tx.commit()?;
    Ok(assigned)
}

/// Design 5.5 rule 4: a hand-off of turn `turn_id` failed, so the row goes
/// back to waiting with no slot. A row the provider accepted keeps its slot.
pub fn release(conn: &Connection, id: &MessageId, turn_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_messages SET turn_id = NULL, slot_window = NULL, handle = NULL, updated_at = ?3
          WHERE id = ?1 AND turn_id = ?2 AND delivered_at IS NULL",
        params![id.as_str(), turn_id, now],
    )?;
    Ok(())
}

/// The provider took the text the hand-off of turn `turn_id` gave it.
pub fn mark_delivery(
    conn: &Connection,
    id: &MessageId,
    turn_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_messages SET delivered_at = ?3, updated_at = ?3
          WHERE id = ?1 AND turn_id = ?2 AND delivered_at IS NULL",
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn message_id(id: &str) -> Result<MessageId, String> {
    MessageId::parse(id).ok_or_else(|| "Invalid message id".into())
}

fn turn(turn_id: &str) -> Result<&str, String> {
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
    // Design 6: the window that owns the session runs the route, so no other
    // window loads a second copy of it to deliver.
    if let Submitted::Accepted {
        messages,
        route: Some(route),
        owner_window,
        ..
    } = &mut submitted
    {
        let owner = control.route_owner(route.session_id.as_str(), window.label());
        if owner != window.label()
            && app
                .emit_to(
                    owner.as_str(),
                    "mono-message-route",
                    RouteEvent {
                        messages,
                        route,
                        caller_window: window.label(),
                    },
                )
                .is_ok()
        {
            *owner_window = Some(owner);
        }
    }
    Ok(submitted)
}

#[tauri::command(async)]
pub fn mono_message_assign_slot(
    window: WebviewWindow,
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
    kind: String,
) -> Result<bool, String> {
    let delivery = match kind.as_str() {
        "submit" => Delivery::Submit,
        "steer" => Delivery::Steer,
        _ => return Err("A hand-off is submit or steer".into()),
    };
    let conn = store.lock_conn()?;
    assign_slot(
        &conn,
        &message_id(&id)?,
        turn(&turn_id)?,
        window.label(),
        delivery,
        now_ms(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_message_release(
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
) -> Result<(), String> {
    let conn = store.lock_conn()?;
    release(&conn, &message_id(&id)?, turn(&turn_id)?, now_ms()).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn mono_message_mark_delivery(
    store: State<'_, SessionStore>,
    id: String,
    turn_id: String,
) -> Result<(), String> {
    let conn = store.lock_conn()?;
    mark_delivery(&conn, &message_id(&id)?, turn(&turn_id)?, now_ms())
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

#[cfg(test)]
mod tests {
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
            action_type: Some(ActionType::Queue),
            reply_to: None,
            steers: None,
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
                        gate_result: RejectCode::TooLarge,
                        wanting_reply: true,
                    },
                )
            },
            message(
                'b',
                Status::Open {
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: None,
                },
            ),
            message(
                'c',
                Status::Open {
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: Some(waiting(1, Some(20))),
                },
            ),
            message(
                'd',
                Status::Delivered {
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
                    wanting_reply: Wanting,
                    routed: routed(),
                    slot: Some(waiting(2, Some(20))),
                    closed_at: 30,
                },
            ),
            message(
                'f',
                Status::Unanswered {
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
            action_type: None,
            reply_to: Some(asked.id.clone()),
            hop: 1,
            ..message(
                letter,
                Status::Delivered {
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

    #[test]
    fn ensure_tables_twice_is_a_no_op() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let kept = message(
            'a',
            Status::Dropped {
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
    fn check_refuses_an_accepted_reply_that_answers_nothing() {
        assert_check_refuses(
            "delivered",
            &[("tool", text("reply"))],
            "tool <> 'reply' OR status = 'rejected' OR reply_to IS NOT NULL",
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

    #[test]
    fn check_refuses_a_reply_that_steers_or_queues() {
        assert_check_refuses(
            "delivered",
            &[
                ("tool", text("reply")),
                ("reply_to", text("m_aaaaaaaaaaaa")),
                ("steers", text("m_aaaaaaaaaaaa")),
                ("hop", Sql::Integer(1)),
            ],
            "tool <> 'reply' OR (steers IS NULL AND action_type IS NULL)",
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
            &[("tool", text("reply")), ("part", Sql::Integer(2))],
            "part = 0 OR (tool = 'reply' AND status <> 'rejected')",
        );
    }

    #[test]
    fn check_refuses_an_accepted_reply_at_hop_0() {
        assert_check_refuses(
            "delivered",
            &[
                ("tool", text("reply")),
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

    fn messaging_store() -> SessionStore {
        let store = SessionStore::open_in_memory().unwrap();
        store
            .lock_conn()
            .unwrap()
            .execute_batch(
                r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
                   VALUES ('giga', 'Boss', 'giga', '{"sessionId":"giga-chat"}', 0, 1, 1),
                          ('mono-a', 'A', 'mono', '{"sessionId":"a-chat"}', 1, 1, 1);
                   INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
                   VALUES ('giga-chat', '/', 'codex', 'm', 'default', 'G', 1, 1),
                          ('a-chat', '/', 'codex', 'm', 'default', 'A', 1, 1),
                          ('project', '/p', 'codex', 'm', 'default', 'P', 1, 1);"#,
            )
            .unwrap();
        store
    }

    fn caller_in(mono_id: &str, session_id: &str) -> Caller {
        Caller {
            kind: CallerKind::MonoChat,
            mono_id: mono(mono_id),
            session_id: session(session_id),
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

    /// A call while the sessions in `running` run a turn. Every test session
    /// runs on codex, which can steer.
    fn call_with(
        conn: &Connection,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: serde_json::Value,
        live: &[LiveSession],
        running: &[&str],
    ) -> Submitted {
        let running: HashSet<String> = running.iter().map(|id| id.to_string()).collect();
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
        let input =
            serde_json::json!({"message": "Ship it", "send_to": "mono-a", "wanting_reply": false});
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
                route: None,
                owner_window: None,
            }
        );
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn a_request_id_reused_with_other_input_is_refused_without_a_row() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let giga = caller_in("giga", "giga-chat");
        let send = |message: &str| serde_json::json!({"message": message, "send_to": "mono-a", "wanting_reply": false});
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
        let send =
            |to: &str| serde_json::json!({"message": "Hi", "send_to": to, "wanting_reply": false});
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
        let input = serde_json::json!({"message": "Hi", "send_to": "nope", "wanting_reply": false});
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
            serde_json::json!({"message": "Is CI green?", "send_to": "mono-a", "wanting_reply": true}),
            &[],
        ))
        .remove(0);
        assert!(assign_slot(&conn, &ask, "turn-a", "main", Delivery::Submit, 101).unwrap());
        mark_delivery(&conn, &ask, "turn-a", 102).unwrap();
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
            (&reply.reply_to, reply.hop, &reply.to),
            (
                &Some(ask.clone()),
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
        accepted_ids(&call(
            conn,
            &caller_in("giga", "giga-chat"),
            request_id,
            Tool::SendMessage,
            serde_json::json!({"message": "Hi", "send_to": "mono-a", "wanting_reply": wanting_reply}),
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
        assert!(assign_slot(&conn, &id, "turn-1", "main", Delivery::Steer, 101).unwrap());
        assert!(assign_slot(&conn, &id, "turn-1", "main", Delivery::Steer, 102).unwrap());
        assert!(!assign_slot(&conn, &id, "turn-2", "main", Delivery::Submit, 103).unwrap());
        assert_eq!(slot_turn(&conn, &id).as_deref(), Some("turn-1"));
        release(&conn, &id, "turn-2", 104).unwrap();
        assert_eq!(slot_turn(&conn, &id).as_deref(), Some("turn-1"));
        release(&conn, &id, "turn-1", 105).unwrap();
        assert_eq!(slot_turn(&conn, &id), None);
        assert!(assign_slot(&conn, &id, "turn-2", "main", Delivery::Submit, 106).unwrap());
    }

    #[test]
    fn a_row_the_provider_took_keeps_its_slot_and_is_never_handed_off_again() {
        let store = messaging_store();
        let conn = store.lock_conn().unwrap();
        let id = sent_to_a(&conn, "r1", false);
        assert!(assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 101).unwrap());
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
        assert!(!assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 104).unwrap());
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
        assert!(assign_slot(&conn, &held, "turn-1", "main", Delivery::Submit, 101).unwrap());
        let listed: Vec<MessageId> = unqueued(&conn, &session("a-chat"))
            .unwrap()
            .into_iter()
            .map(|message| message.id)
            .collect();
        assert_eq!(listed, vec![waiting]);
        assert!(unqueued(&conn, &session("giga-chat")).unwrap().is_empty());
    }

    fn ask_delivered(conn: &Connection, request_id: &str, turn_id: &str) -> MessageId {
        let id = sent_to_a(conn, request_id, true);
        assert!(assign_slot(conn, &id, turn_id, "main", Delivery::Submit, 101).unwrap());
        mark_delivery(conn, &id, turn_id, 102).unwrap();
        id
    }

    fn stored_delivery(conn: &Connection, id: &MessageId) -> Option<String> {
        conn.query_row(
            "SELECT delivery FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn hi(to: &str, wanting_reply: bool, action_type: &str) -> serde_json::Value {
        serde_json::json!({"message": "Hi", "send_to": to, "wanting_reply": wanting_reply, "action_type": action_type})
    }

    fn yes() -> serde_json::Value {
        serde_json::json!({"message": "Yes"})
    }

    fn route_of(submitted: &Submitted) -> Option<Routing> {
        match submitted {
            Submitted::Accepted { route, .. } => route.clone(),
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
        assert!(assign_slot(&conn, &ask, "turn-1", "main", Delivery::Steer, 101).unwrap());
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
            hi("project", false, "steer"),
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
        let steer = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            hi("mono-a", false, "steer"),
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
            hi("mono-a", false, "steer"),
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
        let outcome = call_with(
            &conn,
            &caller_in("giga", "giga-chat"),
            "r1",
            Tool::SendMessage,
            hi("mono-a", false, "steer"),
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
            hi("mono-a", false, "steer"),
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
        assert!(!assign_slot(&conn, &id, "turn-1", "main", Delivery::Submit, 101).unwrap());
        assert_eq!(slot_turn(&conn, &id), None);
    }
}
