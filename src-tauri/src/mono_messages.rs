//! Messages between Monos in `monocode.db`, one row per message and part.
//!
//! The CHECKs make every state the domain forbids unwritable, so a writer that
//! forgets a column fails at the write. [`parse_row`] is the one place a row
//! becomes a [`MonoMessage`]; [`insert`] writes only what that type can hold.
//! Session ids are plain text, not foreign keys: sender sessions can be
//! unpersisted, and a Mono reset deletes its chat.

// PR 4a's `submit` is the first caller outside the tests.
#![cfg_attr(not(test), allow(dead_code))]

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Serialize, Serializer};

use crate::mono_roster::validate_mono_id;
use crate::session_store::validate_id;

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
           CHECK (request_id NOT GLOB '*[^A-Za-z0-9_-]*')
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

    pub fn chars(&self) -> u32 {
        self.chars
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
        assert_eq!(long.chars(), 8001);
        let short = Body::new("Ship the fix");
        assert_eq!((short.text(), short.chars()), ("Ship the fix", 12));
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
                    ("gate_result", text("too_large")),
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
            &[("payload_hash", text(&"z".repeat(64)))],
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
}
