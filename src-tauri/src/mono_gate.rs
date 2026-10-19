//! The messaging gate (design 6): one pure function per tool from a call and
//! the facts `mono_messages::submit` read in its transaction to a
//! [`Decision`]. The first matching row of design 16.5 or 6.4 wins.
//!
//! PR I2 has rows S1-S7, I1, I2, S9, S10, S13-S17, S20, W1-W3 (whole-turn)
//! and R1-R8, R10, R11, the hop limit (S18, R9) and the rate limit (S19).
//! The level rows L1-L7 (design 13) come with session callers, then the peer
//! rule (S8), upward steers (S12) and rule (b).

use std::borrow::Cow;

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::mono_messages::{
    Delivery, Handle, Intent, IntentKind, MessageId, MonoId, MonoMessage, Party, PayloadHash,
    RejectCode, SessionId, Status, SteerTarget, Tool,
};
use crate::mono_roster::MonoRole;

/// How a Mono calls: from its chat or from one of its habit runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerKind {
    MonoChat,
    HabitRun,
}

/// How a session caller got the app CLI (design 13.4): a submitted
/// `/operator` turn, or only the `startedBy` stamp of `sessions.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Operator,
    Started,
}

/// Who speaks in a call (design 2 step 5, 13.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Speaker {
    /// A Mono's chat or one of its habit runs.
    Mono { kind: CallerKind, mono_id: MonoId },
    /// Design 13.4: a lead or worker session with app access. It has no
    /// Mono. `lead_session_id` is the web app's copy (the Change E level
    /// link, not the orchestration link); the gate's level comes from the
    /// stored `sessions.lead_session_id`, and this copy counts only when
    /// `sessions` has no row for the caller (design 13.1).
    Session {
        access: Access,
        cwd: String,
        lead_session_id: Option<SessionId>,
    },
}

/// A call as the web app resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub speaker: Speaker,
    pub session_id: SessionId,
    /// The turn that authorized the call (design 2 step 4). A reply answers
    /// only the messages of this turn.
    pub turn_id: String,
}

impl Caller {
    /// The caller's Mono; `None` for a session caller.
    pub fn mono_id(&self) -> Option<&MonoId> {
        match &self.speaker {
            Speaker::Mono { mono_id, .. } => Some(mono_id),
            Speaker::Session { .. } => None,
        }
    }

    /// How a Mono calls; `None` for a session caller.
    pub fn kind(&self) -> Option<CallerKind> {
        match &self.speaker {
            Speaker::Mono { kind, .. } => Some(*kind),
            Speaker::Session { .. } => None,
        }
    }
}

/// Design 13.1: a session's level, derived on each call from
/// `sessions.lead_session_id`. This is the Change E level link, not the
/// orchestration link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLevel {
    Lead,
    Worker { lead: SessionId },
}

/// Design 13.1: every level a caller can have. Giga and Mono come from
/// `monos.role` (a habit run has its Mono's level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Giga,
    Mono,
    Lead,
    Worker { lead: SessionId },
}

/// What a send names (design 16.12, whole-turn). One value: "reply_to and
/// scope both" has no form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTarget {
    /// `reply_to`: a message id only (PR I2 review r1). A handle names a
    /// message the caller's session received, which S10 refuses.
    Message(MessageId),
    /// `scope: "turn"`.
    Turn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendInput {
    pub message: String,
    pub send_to: String,
    pub wanting_reply: bool,
    /// As asked: rows I1, W1 and I2 refuse what a send cannot carry.
    pub intent: IntentKind,
    /// `None` is "neither": W1 writes a `steer_needs_target` row for a steer.
    pub target: Option<SendTarget>,
}

impl SendInput {
    pub fn reply_to(&self) -> Option<&MessageId> {
        match &self.target {
            Some(SendTarget::Message(id)) => Some(id),
            _ => None,
        }
    }

    pub fn whole_turn(&self) -> bool {
        self.target == Some(SendTarget::Turn)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyInput {
    pub message: String,
    /// The handles it answers; without them, the one open handle.
    pub to: Option<Vec<Handle>>,
    pub wanting_reply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Send(SendInput),
    Reply(ReplyInput),
}

impl Input {
    pub fn tool(&self) -> Tool {
        match self {
            Input::Send(_) => Tool::SendMessage,
            Input::Reply(_) => Tool::Reply,
        }
    }

    /// sha256 of the canonical `[tool, parsed input]`, defaults filled in, so
    /// a retry that leaves out a default is the same call.
    pub fn payload_hash(&self) -> PayloadHash {
        let canonical = match self {
            Input::Send(send) => {
                let mut fields = json!({
                    "message": send.message,
                    "send_to": send.send_to,
                    "wanting_reply": send.wanting_reply,
                    "intent": send.intent.as_str(),
                    "reply_to": send.reply_to().map(MessageId::as_str),
                });
                // Only a whole-turn call hashes `scope`, so every other call
                // keeps the hash it had before `scope` existed.
                if send.whole_turn() {
                    fields["scope"] = json!("turn");
                }
                json!(["send_message", fields])
            }
            Input::Reply(reply) => {
                let mut fields = json!({
                    "message": reply.message,
                    "wanting_reply": reply.wanting_reply,
                });
                // Only a call with `to` hashes it, so a call without keeps
                // the hash it had before `to` existed.
                if let Some(to) = &reply.to {
                    fields["to"] = json!(to);
                }
                json!(["reply", fields])
            }
        };
        sha256(&canonical.to_string())
    }
}

/// sha256 of a canonical call text, as 64 lowercase hex digits.
fn sha256(text: &str) -> PayloadHash {
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    PayloadHash::parse(&digest).expect("sha256 is 64 lowercase hex digits")
}

/// S1 and R1: the input the model sent, or the `invalid_input` hint.
pub fn parse(tool: Tool, raw: &Value) -> Result<Input, String> {
    let empty = Map::new();
    let fields = raw.as_object().unwrap_or(&empty);
    let allowed: &[&str] = match tool {
        Tool::SendMessage => &[
            "message",
            "send_to",
            "wanting_reply",
            "intent",
            "reply_to",
            "scope",
        ],
        Tool::Reply => &["message", "wanting_reply", "to"],
        Tool::StartSession | Tool::AppAction => {
            return Err(format!("{} is not a messaging tool", tool.as_str()))
        }
    };
    let unknown: Vec<&str> = fields
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown {} fields: {}. Allowed: {}.",
            tool.as_str(),
            unknown.join(", "),
            allowed.join(", ")
        ));
    }
    // SQLite counts a text only up to U+0000, so the row of such a body,
    // accepted or rejected, fails its length CHECK.
    let message = match fields.get("message") {
        Some(Value::String(text)) if text.contains('\0') => {
            return Err("message must not contain the character U+0000".into())
        }
        Some(Value::String(text)) if !text.trim().is_empty() => text.clone(),
        _ => return Err("message must be a non-empty string".into()),
    };
    let wanting_reply = match (fields.get("wanting_reply"), tool) {
        (Some(Value::Bool(wanting)), _) => *wanting,
        (None, Tool::Reply) => false,
        _ => {
            return Err(
                "wanting_reply is required: true when you need an answer, false otherwise".into(),
            )
        }
    };
    if tool == Tool::Reply {
        let to = fields
            .get("to")
            .map(|to| handles(to).ok_or_else(|| TO_RULE.to_string()))
            .transpose()?;
        return Ok(Input::Reply(ReplyInput {
            message,
            to,
            wanting_reply,
        }));
    }
    let send_to = match fields.get("send_to") {
        Some(Value::String(id)) if !id.trim().is_empty() && id.len() <= 256 => id.clone(),
        _ => {
            return Err(
                "send_to must be a mono_id from get_monos or the id of a session in one of your projects"
                    .into(),
            )
        }
    };
    let intent = match fields.get("intent").and_then(Value::as_str) {
        Some(text) => IntentKind::parse(text).ok_or(INTENT_RULE)?,
        None => return Err(INTENT_RULE.into()),
    };
    let target = match (fields.get("reply_to"), fields.get("scope")) {
        (Some(_), Some(_)) => return Err(TARGET_RULE.into()),
        (Some(reply_to), None) => Some(SendTarget::Message(
            reply_to
                .as_str()
                .and_then(MessageId::parse)
                .ok_or(REPLY_TO_RULE)?,
        )),
        (None, Some(Value::String(scope))) if scope == "turn" => Some(SendTarget::Turn),
        (None, Some(_)) => return Err(SCOPE_RULE.into()),
        (None, None) => None,
    };
    Ok(Input::Send(SendInput {
        message,
        send_to,
        wanting_reply,
        intent,
        target,
    }))
}

const INTENT_RULE: &str = r#"intent is required and is one of "new", "steer", "reply" or "continue". Use "new" for new work, or "steer" with reply_to to change the work a message started."#;
/// Also the answer to a handle (PR I2 review r1): a steer names a message
/// the caller sent, and only its id can.
const REPLY_TO_RULE: &str = "reply_to must be one message_id (m_…). A steer names a message you sent: use the message_id your send_message returned.";
const SCOPE_RULE: &str = r#"scope must be "turn": steer everything the recipient is doing now."#;
const TARGET_RULE: &str = r#"Pass reply_to or scope "turn", not both. reply_to steers the work one message started; scope "turn" steers everything the recipient is doing now."#;

/// What `get_message` takes (design 16.4): a message id, or a handle of the
/// caller's turn. A steer's `reply_to` takes only an id (PR I2 review r1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRef {
    Id(MessageId),
    Handle(Handle),
}

impl MessageRef {
    fn parse(text: &str) -> Option<Self> {
        Handle::parse(text)
            .map(MessageRef::Handle)
            .or_else(|| MessageId::parse(text).map(MessageRef::Id))
    }
}

const HANDLE_RULE: &str = r##"handle must be one message_id or one handle such as "#1"."##;

/// `get_message` (design 16.4): the one field `handle`, a message id or a
/// handle of the caller's turn, or the `invalid_input` hint.
pub fn parse_get(raw: &Value) -> Result<MessageRef, String> {
    let empty = Map::new();
    let fields = raw.as_object().unwrap_or(&empty);
    let unknown: Vec<&str> = fields
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "handle")
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown get_message fields: {}. Allowed: handle.",
            unknown.join(", ")
        ));
    }
    fields
        .get("handle")
        .and_then(Value::as_str)
        .and_then(MessageRef::parse)
        .ok_or_else(|| HANDLE_RULE.into())
}

const TO_RULE: &str =
    r##"to must be a handle such as "#1", or a list of 1 to 9 different handles"##;

/// `to` as design 3.3 allows it: one `"#n"`, or 1 to 9 different ones. A
/// string becomes a one-item list.
fn handles(to: &Value) -> Option<Vec<Handle>> {
    let items = match to {
        Value::String(_) => std::slice::from_ref(to),
        Value::Array(items) if (1..=9).contains(&items.len()) => items.as_slice(),
        _ => return None,
    };
    let mut handles: Vec<Handle> = Vec::with_capacity(items.len());
    for item in items {
        let handle = item.as_str().and_then(Handle::parse)?;
        if handles.contains(&handle) {
            return None;
        }
        handles.push(handle);
    }
    Some(handles)
}

/// A session as the gate sees it. It exists when the web app's live list or
/// `sessions` has it and `mono_session_ends` does not (design 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub session: SessionId,
    pub exists: bool,
    /// The turn in `ControlHost.active`, while it runs one.
    pub turn: Option<String>,
    pub steerable: bool,
}

impl Target {
    pub fn running(&self) -> bool {
        self.turn.is_some()
    }
}

/// What `send_to` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// No Mono and no existing session has that id.
    Unknown,
    /// `chat` is its current chat, if it has one.
    Mono {
        mono_id: MonoId,
        /// `monos.role` (design 16.12): W2 reads it.
        role: MonoRole,
        chat: Option<Target>,
        /// `path_key` of each of its `projects` (design 6.1): S8 and L3.
        projects: Vec<String>,
        /// Its `messageAllowlist` (design 6.1): S8.
        allowlist: Vec<MonoId>,
    },
    /// An existing session. `own_project` is false for Mono chats, habit runs
    /// and sessions outside the caller's projects. `level` is its level
    /// (design 13.1): the stored row wins over the live list.
    Session {
        target: Target,
        own_project: bool,
        level: SessionLevel,
    },
}

impl Recipient {
    fn party(&self) -> Option<Party> {
        match self {
            Recipient::Unknown => None,
            Recipient::Mono { mono_id, .. } => Some(Party::Mono {
                mono_id: mono_id.clone(),
            }),
            Recipient::Session { target, .. } => Some(Party::Session {
                session_id: target.session.clone(),
            }),
        }
    }
}

/// Design 16.12.2.2 (whole-turn): who may steer a recipient's whole turn. A
/// Giga steers a Mono's, and a Mono a lead's in one of its projects. A lead
/// or a worker steers no whole turn, and no caller a worker's.
fn may_steer_turn(caller: &Level, recipient: &Recipient) -> bool {
    matches!(
        (caller, recipient),
        (
            Level::Giga,
            Recipient::Mono {
                role: MonoRole::Mono,
                ..
            }
        ) | (
            Level::Mono,
            Recipient::Session {
                level: SessionLevel::Lead,
                own_project: true,
                ..
            }
        )
    )
}

/// The work a steer's target started (design 16.6): the turn that handles
/// it, or none yet when it waits in a queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPin {
    Turn(String),
    /// M started no turn of its own, so the steer joins the turn its session
    /// runs now, and the accepted row keeps it (`SteerTarget::Pinned`): rule
    /// (b)'s late answer, or a start row (design 16.2).
    Live(String),
    NotStarted,
}

/// Where a steer that names M goes (design 16.6): the receiver side, or
/// rule (b)'s `Giga` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSession {
    /// M is not a message from the caller to the recipient.
    NotBetween,
    /// The session handling M, `None` once a drop took it away.
    Receiver {
        session: Option<SessionId>,
        pin: TurnPin,
    },
    /// Rule (b): a Mono's late answer to a Giga goes to the Giga's current
    /// chat, `None` when M came from another session of the Giga (a habit
    /// run never takes a steer). Pin: the turn that chat runs now, which the
    /// accepted answer keeps (review r1 m1).
    Giga {
        chat: Option<SessionId>,
        pin: TurnPin,
    },
}

/// Rule (b) (design 6.1): how long a Giga's question to a Mono stays open to
/// a late answer, 24 h.
const ANSWER_WINDOW_MS: i64 = 86_400_000;

/// Design 16.6: where a steer that names M goes. The receiver side when M
/// is the caller's message to the recipient (from a session caller: a row
/// with no Mono that its session wrote); the `Giga` variant when M
/// qualifies rule (b).
pub fn target_session(
    caller: &Caller,
    level: &Level,
    recipient: &Recipient,
    m: &MonoMessage,
    now: i64,
) -> TargetSession {
    if let (
        Level::Mono,
        Recipient::Mono {
            mono_id: giga,
            role: MonoRole::Giga,
            chat,
            ..
        },
        Some(me),
    ) = (level, recipient, caller.mono_id())
    {
        // Only an unanswered M: an open one is answered with reply, which
        // closes its handle (intents review r1).
        let qualifies = m.from_mono_id.as_ref() == Some(giga)
            && matches!(&m.to, Some(Party::Mono { mono_id }) if mono_id == me)
            && matches!(m.status, Status::Unanswered { .. })
            && now - m.created_at <= ANSWER_WINDOW_MS;
        if qualifies {
            let chat = chat
                .as_ref()
                .filter(|chat| chat.session == m.from_session_id);
            return TargetSession::Giga {
                chat: chat.map(|chat| chat.session.clone()),
                pin: chat
                    .and_then(|chat| chat.turn.clone())
                    .map_or(TurnPin::NotStarted, TurnPin::Live),
            };
        }
    }
    let from_caller = match caller.mono_id() {
        Some(mono_id) => m.from_mono_id.as_ref() == Some(mono_id),
        None => m.from_mono_id.is_none() && m.from_session_id == caller.session_id,
    };
    if !from_caller || m.to.is_none() || m.to != recipient.party() {
        return TargetSession::NotBetween;
    }
    let turn = |turn_id: &str| TurnPin::Turn(turn_id.to_string());
    let (session, pin) = match &m.status {
        Status::Open { routed, slot, .. } | Status::Answered { routed, slot, .. } => (
            Some(&routed.handled_by_session),
            slot.as_ref().map(|slot| turn(&slot.turn_id)),
        ),
        Status::Delivered { routed, slot, .. } => (
            Some(&routed.handled_by_session),
            match (slot, recipient) {
                (Some(slot), _) => Some(turn(&slot.turn_id)),
                // Design 16.2: a start row's work is its whole session, so a
                // steer through it is pinned to the turn that session runs.
                (None, Recipient::Session { target, .. }) if m.tool == Tool::StartSession => {
                    target.turn.clone().map(TurnPin::Live)
                }
                (None, _) => None,
            },
        ),
        Status::Unanswered { routed, slot, .. } => {
            (Some(&routed.handled_by_session), Some(turn(&slot.turn_id)))
        }
        Status::Dropped { .. } | Status::Rejected { .. } => (None, None),
    };
    TargetSession::Receiver {
        session: session.cloned(),
        pin: pin.unwrap_or(TurnPin::NotStarted),
    }
}

/// The accepted row `reply_to` named, and the session target_session gives
/// for it (the receiver's session, or rule (b)'s Giga chat) as the live
/// list and `ControlHost` see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referenced {
    pub message: MonoMessage,
    pub session: Option<Target>,
}

/// What the send rows (design 16.5) read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendFacts {
    pub recipient: Recipient,
    /// `None` without `reply_to`, or when it named no accepted row (S9).
    pub reply_to: Option<Referenced>,
    /// S19 (design 6.1): the caller's accepted sends to this recipient in the
    /// last 10 minutes.
    pub pair: PairCount,
}

/// Design 6.1 "pair count": accepted `send_message` rows with the caller's
/// sender key and this recipient, `created_at > now - RATE_WINDOW_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PairCount {
    pub count: u32,
    /// `created_at` of the oldest counted row, for the retry hint.
    pub oldest: Option<i64>,
}

/// How a reply's handle stands in the caller's turn (design 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleState {
    /// The provider took the text and nothing closed it yet: the open set.
    Open,
    /// Handed off, but the provider has not taken the text, so the agent
    /// never saw the handle.
    Unseen,
    Closed(Closure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closure {
    Answered,
    Unanswered,
    Dropped,
}

impl Closure {
    pub fn as_str(self) -> &'static str {
        match self {
            Closure::Answered => "answered",
            Closure::Unanswered => "unanswered",
            Closure::Dropped => "dropped",
        }
    }
}

/// A message with a handle in the caller's turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handled {
    pub message: MonoMessage,
    pub handle: Handle,
    pub state: HandleState,
    /// The sender Mono's name, for hints.
    pub from: String,
    /// The session the reply goes to: the message's sender session.
    pub sender: Target,
}

/// What the reply rows (design 6.4) read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyFacts {
    /// The caller's handles in its turn.
    pub handled: Vec<Handled>,
    /// For a duplicate of a rejected reply: the caller's handles as they
    /// stood when the first call ran.
    pub then: Vec<Handled>,
}

/// The caller's `monos` row (design 16.12): its current chat, its role and
/// (design 6.1) the `path_key` of each project and its allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerMono {
    pub chat: Option<SessionId>,
    pub role: MonoRole,
    pub projects: Vec<String>,
    pub allowlist: Vec<MonoId>,
}

/// What the gate knows of the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerFacts {
    /// A Mono caller's `monos` row.
    Mono(CallerMono),
    /// Design 13.1: a session caller's level (the stored `lead_session_id`
    /// wins over the web app's copy) and the `path_key` of its `cwd`, its one
    /// project.
    Session { level: SessionLevel, cwd: String },
}

impl CallerFacts {
    pub fn level(&self) -> Level {
        match self {
            CallerFacts::Mono(CallerMono {
                role: MonoRole::Giga,
                ..
            }) => Level::Giga,
            CallerFacts::Mono(CallerMono {
                role: MonoRole::Mono,
                ..
            }) => Level::Mono,
            CallerFacts::Session {
                level: SessionLevel::Lead,
                ..
            } => Level::Lead,
            CallerFacts::Session {
                level: SessionLevel::Worker { lead },
                ..
            } => Level::Worker { lead: lead.clone() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts<About> {
    /// What the gate knows of the caller; `None` when a Mono caller's row is
    /// gone (`not_a_mono`).
    pub caller: Option<CallerFacts>,
    /// Rows this caller session already wrote with this request id.
    pub prior: Vec<MonoMessage>,
    pub about: About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    InvalidInput,
    RequestIdReused,
    NotAMono,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::InvalidInput => "invalid_input",
            Refusal::RequestIdReused => "request_id_reused",
            Refusal::NotAMono => "not_a_mono",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Queue,
    Steer,
}

impl Via {
    pub fn delivery(self) -> Delivery {
        match self {
            Via::Queue => Delivery::Queue,
            Via::Steer => Delivery::Steer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub session: SessionId,
    pub via: Via,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenHandle {
    pub handle: Handle,
    pub message_id: MessageId,
    pub from: String,
}

/// The window of the pair count (design 6.1), 10 minutes.
pub const RATE_WINDOW_MS: i64 = 600_000;
/// S19: the accepted sends one pair may have in the window.
const RATE_LIMIT: u32 = 6;
/// S18 and R9: the deepest hop a call may write.
const MAX_HOP: u32 = 4;

/// One row an accepted call writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub to: Party,
    pub route: Route,
    /// What the row is; a `reply` makes its target `answered`.
    pub intent: Intent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The same call again: its stored outcome, and no new row. A call that
    /// was rejected gets `rejection` again (design 3.4).
    Duplicate {
        prior: Vec<MonoMessage>,
        rejection: Option<Rejection>,
    },
    /// No row.
    Refuse { code: Refusal, hint: String },
    /// One `rejected` row. `hop` is the hop the call would have had for S18
    /// and R9 (design 6, E25), and 0 for every other code.
    Reject {
        code: RejectCode,
        hint: String,
        to: Option<Party>,
        hop: u32,
        open_handles: Vec<OpenHandle>,
    },
    /// One row per entry, in part order, each to its own route. A reply to
    /// several handles answers each sender in its own session.
    Accept { hop: u32, rows: Vec<Outgoing> },
}

/// A rejected reply: its code, hint and the open handles it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub code: RejectCode,
    pub hint: String,
    pub open_handles: Vec<OpenHandle>,
}

impl From<Rejection> for Decision {
    fn from(rejection: Rejection) -> Self {
        Decision::Reject {
            code: rejection.code,
            hint: rejection.hint,
            to: None,
            hop: 0,
            open_handles: rejection.open_handles,
        }
    }
}

impl RejectCode {
    /// The hint of a stored rejection, when its call is made again.
    pub fn hint(self) -> &'static str {
        match self {
            RejectCode::TooLarge => "Messages are limited to 8000 characters. Save the full text to a file or artifact and send its path.",
            RejectCode::UnknownTarget => "No Mono or project session with that id. Call get_monos for mono_id values.",
            RejectCode::SelfSend => "You cannot message yourself.",
            RejectCode::TargetNoConversation => "That Mono has no chat yet. Ask the user to open it once. Use a new request id when you retry.",
            RejectCode::DirectionForbidden => "That session is not in one of your projects.",
            RejectCode::UpwardSteerForbidden => "You can steer a Giga only about a message it sent you in the last 24 h. Answer an open handle with reply, or send with intent \"new\"; it is queued.",
            RejectCode::ReplyUnsupportedTarget => "Project sessions cannot reply. Send with wanting_reply false; their completion report comes back to you.",
            RejectCode::HabitCannotWait => "A habit run ends before an answer can arrive. Send with wanting_reply false, or ask from your chat.",
            RejectCode::UnknownMessage => "No message you can refer to has that id. Use the message_id your send_message returned.",
            RejectCode::SteerTargetMismatch => "reply_to is not a message between you and send_to.",
            RejectCode::IntentMismatch => "intent \"new\" takes no reply_to. Remove reply_to, or use intent \"steer\" to change the work that message started.",
            RejectCode::IntentNotAllowed => "Agents cannot send continue. Use reply or steer.",
            RejectCode::TargetGone => "The session handling that message no longer exists (cleared or deleted).",
            RejectCode::CannotSteer => "That session's provider cannot steer a running turn. Send it with intent \"new\"; it waits for the turn to end.",
            RejectCode::RateLimited => "Too many messages to that recipient in 10 minutes. Retry later with a new request id.",
            RejectCode::HopLimit => "This reply chain is 4 replies deep. Stop here or start a new message.",
            RejectCode::NoOpenHandle => "No message in this turn is waiting for your reply. Use send_message.",
            RejectCode::UnknownHandle => "No such handle in this turn.",
            RejectCode::HandleClosed => "That handle is already closed.",
            RejectCode::AmbiguousReply => "Several handles are open. Pass to.",
            RejectCode::SteerNeedsTarget => "name a message or use scope turn",
            RejectCode::NothingRunning => "send it as a new message",
            RejectCode::SteerScopeNotAllowed => "You cannot steer this recipient's whole turn. A Giga may steer a Mono's whole turn, and a Mono one of its leads. Steer one message with reply_to, or send with intent \"new\".",
            RejectCode::WorkerCannotSpawn => "Only lead sessions can start sessions. Ask your lead: messages.send with send_to set to your lead's session id.",
            RejectCode::WorkerScope => "A worker can message only its lead. Message your lead: messages.send with send_to set to your lead's session id.",
            RejectCode::SteerViaLead => "That session is a worker of a lead session. Message the lead; only its lead steers its workers.",
            RejectCode::SessionCannotWait => "A lead or worker session cannot wait for an answer. Send with wanting_reply false. The recipient answers with a new message if it needs to.",
            RejectCode::LeadScope => "A lead acts only on its own workers. Start workers with sessions.start, steer or stop only sessions you started, and report to your Mono with messages.send.",
            RejectCode::NotAGiga => "Only a Giga can file, update or withdraw approval cards. Escalate to your Giga: a Mono messages its Giga with messages.send, and a lead or worker reports to its lead or Mono.",
        }
    }

    /// The codes whose error lists the open handles (design 3.4).
    pub fn lists_handles(self) -> bool {
        matches!(
            self,
            RejectCode::NoOpenHandle | RejectCode::AmbiguousReply | RejectCode::UnknownHandle
        )
    }
}

const NOT_A_MONO: &str = "Only a Mono's chat or its habit runs can use Mono messaging.";
pub(crate) const REQUEST_ID_REUSED: &str =
    "This request id was already used with different input. Use a new request id.";

fn reject(code: RejectCode, to: Option<Party>) -> Decision {
    reject_at(code, to, 0)
}

fn reject_at(code: RejectCode, to: Option<Party>, hop: u32) -> Decision {
    Decision::Reject {
        code,
        hint: code.hint().into(),
        to,
        hop,
        open_handles: Vec::new(),
    }
}

/// The rows every tool shares: the caller's facts, then S2/S3 (R2/R3).
/// `again` builds the answer of a call that was rejected. `Ok` is what the
/// gate knows of the caller when no row here decides the call. Only a Mono
/// caller comes without facts (its `monos` row is gone), so a session caller
/// never gets `not_a_mono`.
fn prelude<'f, A>(
    facts: &'f Facts<A>,
    hash: &PayloadHash,
    again: impl FnOnce(&CallerFacts, RejectCode) -> Rejection,
) -> Result<&'f CallerFacts, Decision> {
    let Some(caller) = &facts.caller else {
        return Err(Decision::Refuse {
            code: Refusal::NotAMono,
            hint: NOT_A_MONO.into(),
        });
    };
    let Some(first) = facts.prior.first() else {
        return Ok(caller);
    };
    Err(if &first.payload_hash == hash {
        Decision::Duplicate {
            prior: facts.prior.clone(),
            rejection: match first.status {
                Status::Rejected { gate_result, .. } => Some(again(caller, gate_result)),
                _ => None,
            },
        }
    } else {
        Decision::Refuse {
            code: Refusal::RequestIdReused,
            hint: REQUEST_ID_REUSED.into(),
        }
    })
}

fn too_large(message: &str) -> bool {
    message.chars().count() > crate::mono_messages::Body::MAX_CHARS
}

/// The hint of a send rejection (design 3.4, 16.5, 16.12): the intent codes
/// and S15 name what this call asked for, the level codes the lead to
/// message, and S19 the time to wait. A replay builds it again from its own
/// facts (`prelude`).
fn send_hint(
    code: RejectCode,
    row: &CallerFacts,
    input: &SendInput,
    about: &SendFacts,
    now: i64,
) -> String {
    match (code, input.intent, &input.target) {
        (RejectCode::IntentMismatch, IntentKind::New, Some(SendTarget::Turn)) => {
            r#"intent "new" takes no scope. Remove scope, or use intent "steer"."#.into()
        }
        (RejectCode::IntentMismatch, _, Some(SendTarget::Turn)) => {
            r#"scope "turn" is for intent "steer" only."#.into()
        }
        (RejectCode::IntentMismatch, intent, None) => format!(
            r#"intent "{}" needs reply_to: one message_id (m_…)."#,
            intent.as_str()
        ),
        (RejectCode::IntentNotAllowed, IntentKind::Reply, _) => r##"send_message cannot reply. Use reply (to: "#n") to answer a message that waits for you."##.into(),
        _ => match (code, row, &about.recipient) {
            (
                RejectCode::WorkerScope | RejectCode::TargetGone,
                CallerFacts::Session {
                    level: SessionLevel::Worker { lead },
                    ..
                },
                _,
            )
            | (
                RejectCode::SteerViaLead,
                _,
                Recipient::Session {
                    level: SessionLevel::Worker { lead },
                    ..
                },
            ) => with_lead(code, lead),
            (RejectCode::TargetGone, ..) => r#"The work that reply_to started is not running now: it finished, it has not started, or its session is idle, cleared or deleted. Send it with intent "new"."#.into(),
            (RejectCode::DirectionForbidden, CallerFacts::Mono(_), Recipient::Mono { .. }) => {
                "You and that Mono share no project and neither allowlists the other.".into()
            }
            (
                RejectCode::DirectionForbidden,
                CallerFacts::Session { .. },
                Recipient::Mono { .. },
            ) => "That Mono has none of your project folders.".into(),
            (RejectCode::UpwardSteerForbidden, CallerFacts::Session { .. }, _) => {
                r#"Messages to your lead or to a Mono are queued. Send with intent "new"."#.into()
            }
            (RejectCode::RateLimited, ..) => rate_hint(about.pair, now),
            _ => code.hint().into(),
        },
    }
}

/// Design 3.4: a level hint with the lead's id filled in.
fn with_lead(code: RejectCode, lead: &SessionId) -> String {
    let lead = lead.as_str();
    match code {
        RejectCode::WorkerCannotSpawn => format!(
            r#"Only lead sessions can start sessions. Ask your lead ({lead}): messages.send with send_to "{lead}"."#
        ),
        RejectCode::WorkerScope => format!(
            r#"A worker can message only its lead. Message your lead ({lead}): messages.send with send_to "{lead}"."#
        ),
        RejectCode::SteerViaLead => format!(
            "That session is a worker of lead {lead}. Message the lead ({lead}); only its lead steers its workers."
        ),
        RejectCode::TargetGone => format!(
            "Your lead ({lead}) is archived or no longer exists. It cannot get messages until the user restores it. Use a new request id when you retry."
        ),
        RejectCode::NotAGiga => format!(
            r#"Only a Giga can file, update or withdraw approval cards. Escalate to your Giga: message your lead ({lead}) with messages.send with send_to "{lead}"."#
        ),
        _ => code.hint().into(),
    }
}

/// Design 3.4 `rate_limited`: `<n>` is the whole seconds, rounded up, until
/// the oldest counted send leaves the window.
fn rate_hint(pair: PairCount, now: i64) -> String {
    let Some(oldest) = pair.oldest else {
        return RejectCode::RateLimited.hint().into();
    };
    let seconds = (oldest + RATE_WINDOW_MS - now)
        .unsigned_abs()
        .div_ceil(1000);
    format!(
        "{RATE_LIMIT} messages to that recipient in 10 minutes. Retry after {seconds} s with a new request id."
    )
}

/// What an intent-checked send asks for.
enum Asked {
    New,
    Steer,
    WholeTurn,
}

/// Design 16.5 and 16.12.2.1, the rows of PR 4a, PR I2, S19 and the level
/// rows L1-L7 in order.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "baseline 2026-10"
)]
pub fn decide_send(
    caller: &Caller,
    input: &SendInput,
    hash: &PayloadHash,
    facts: &Facts<SendFacts>,
    now: i64,
) -> Decision {
    let about = &facts.about;
    let again = |row: &CallerFacts, code: RejectCode| Rejection {
        code,
        hint: send_hint(code, row, input, about, now),
        open_handles: Vec::new(),
    };
    let row = match prelude(facts, hash, again) {
        Ok(row) => row,
        Err(decision) => return decision,
    };
    let recipient = &about.recipient;
    let to = recipient.party();
    let reject_at = |code: RejectCode, to: Option<Party>, hop: u32| Decision::Reject {
        code,
        hint: send_hint(code, row, input, about, now),
        to,
        hop,
        open_handles: Vec::new(),
    };
    let reject = |code: RejectCode, to: Option<Party>| reject_at(code, to, 0);
    if too_large(&input.message) {
        return reject(RejectCode::TooLarge, to);
    }
    // I1, W1, then I2: the call alone, whatever it names.
    let asked = match (input.intent, &input.target) {
        (IntentKind::New, None) => Asked::New,
        (IntentKind::Steer, Some(SendTarget::Message(_))) => Asked::Steer,
        (IntentKind::Steer, Some(SendTarget::Turn)) => Asked::WholeTurn,
        (IntentKind::New, Some(_))
        | (IntentKind::Reply | IntentKind::Continue, None | Some(SendTarget::Turn)) => {
            return reject(RejectCode::IntentMismatch, to)
        }
        (IntentKind::Steer, None) => return reject(RejectCode::SteerNeedsTarget, to),
        (IntentKind::Reply | IntentKind::Continue, Some(SendTarget::Message(_))) => {
            return reject(RejectCode::IntentNotAllowed, to)
        }
    };
    // L1, L2 before S5: a worker gets "message your lead" for every other
    // target, a made-up one too, and `target_gone` for an ended lead.
    if let CallerFacts::Session {
        level: SessionLevel::Worker { lead },
        ..
    } = row
    {
        if input.send_to != lead.as_str() {
            return reject(RejectCode::WorkerScope, to);
        }
        if !matches!(recipient, Recipient::Session { target, .. } if target.exists) {
            return reject(RejectCode::TargetGone, to);
        }
    }
    let own_chat = match row {
        CallerFacts::Mono(row) => row.chat.as_ref(),
        CallerFacts::Session { .. } => None,
    };
    match recipient {
        Recipient::Unknown => return reject(RejectCode::UnknownTarget, None),
        Recipient::Mono { mono_id, .. } if caller.mono_id() == Some(mono_id) => {
            return reject(RejectCode::SelfSend, to)
        }
        Recipient::Session { target, .. }
            if target.session == caller.session_id || Some(&target.session) == own_chat =>
        {
            return reject(RejectCode::SelfSend, to)
        }
        Recipient::Session {
            own_project: false, ..
        } => return reject(RejectCode::DirectionForbidden, to),
        Recipient::Session { .. } | Recipient::Mono { .. } => {}
    }
    // L3, L4, L7 after S7, so a session outside the caller's projects keeps
    // its lead hidden.
    let led_by_caller =
        |lead: &SessionId| matches!(row, CallerFacts::Session { .. }) && *lead == caller.session_id;
    match (row, recipient) {
        (
            CallerFacts::Session {
                level: SessionLevel::Lead,
                cwd,
            },
            Recipient::Mono { projects, .. },
        ) if !projects.contains(cwd) => return reject(RejectCode::DirectionForbidden, to),
        (
            _,
            Recipient::Session {
                level: SessionLevel::Worker { lead },
                ..
            },
        ) if !led_by_caller(lead) => return reject(RejectCode::SteerViaLead, to),
        (
            CallerFacts::Session {
                level: SessionLevel::Lead,
                ..
            },
            Recipient::Session { level, .. },
        ) if !matches!(level, SessionLevel::Worker { lead } if led_by_caller(lead)) => {
            return reject(RejectCode::LeadScope, to)
        }
        _ => {}
    }
    // W2: the sender learns it may not send this before anything about the
    // recipient's state.
    let level = row.level();
    if matches!(asked, Asked::WholeTurn) && !may_steer_turn(&level, recipient) {
        return reject(RejectCode::SteerScopeNotAllowed, to);
    }
    if let CallerFacts::Session { level, .. } = row {
        // L5: no notice can reach a session caller (design 13.4).
        if input.wanting_reply {
            return reject(RejectCode::SessionCannotWait, to);
        }
        // L6: a session holds no handle, so it never answers upward by steer.
        let upward = matches!(level, SessionLevel::Worker { .. })
            || matches!(recipient, Recipient::Mono { .. });
        if upward && !matches!(asked, Asked::New) {
            return reject(RejectCode::UpwardSteerForbidden, to);
        }
    }
    // S8, the peer rule: two Monos of one role talk when they share a
    // project or one allowlists the other.
    if let (
        CallerFacts::Mono(me),
        Recipient::Mono {
            mono_id,
            role,
            projects,
            allowlist,
            ..
        },
    ) = (row, recipient)
    {
        let shares = me.projects.iter().any(|project| projects.contains(project));
        let allowlisted = me.allowlist.contains(mono_id)
            || caller.mono_id().is_some_and(|me| allowlist.contains(me));
        if me.role == *role && !shares && !allowlisted {
            return reject(RejectCode::DirectionForbidden, to);
        }
    }
    let steered = if let Asked::Steer = asked {
        // S9, S12, then S10: an upward steer names a message on the Giga's
        // side, so S10 would hide the S12 hint that names reply.
        let Some(referenced) = &about.reply_to else {
            return reject(RejectCode::UnknownMessage, to);
        };
        let upward = level == Level::Mono
            && matches!(
                recipient,
                Recipient::Mono {
                    role: MonoRole::Giga,
                    ..
                }
            );
        let (session, pin) =
            match target_session(caller, &level, recipient, &referenced.message, now) {
                TargetSession::Giga { chat, pin } => (chat, pin),
                _ if upward => return reject(RejectCode::UpwardSteerForbidden, to),
                TargetSession::NotBetween => return reject(RejectCode::SteerTargetMismatch, to),
                TargetSession::Receiver { session, pin } => (session, pin),
            };
        Some((referenced, session, pin))
    } else {
        None
    };
    // S13, S14.
    match recipient {
        Recipient::Session { .. } if input.wanting_reply => {
            return reject(RejectCode::ReplyUnsupportedTarget, to)
        }
        Recipient::Mono { .. }
            if caller.kind() == Some(CallerKind::HabitRun) && input.wanting_reply =>
        {
            return reject(RejectCode::HabitCannotWait, to)
        }
        _ => {}
    }
    let (hop, route, intent) = if let Some((referenced, session, pin)) = steered {
        // S15: the work M started must run now, in the turn that runs it.
        let running = referenced.session.as_ref().filter(|target| {
            session.as_ref() == Some(&target.session)
                && target.exists
                && match &pin {
                    TurnPin::Turn(turn) | TurnPin::Live(turn) => target.turn.as_ref() == Some(turn),
                    TurnPin::NotStarted => false,
                }
        });
        let Some(target) = running else {
            return reject(RejectCode::TargetGone, to);
        };
        if !target.steerable {
            return reject(RejectCode::CannotSteer, to);
        }
        let message = referenced.message.id.clone();
        // A steer is one hop deeper than the message it changes.
        (
            referenced.message.hop + 1,
            Route {
                session: target.session.clone(),
                via: Via::Steer,
            },
            Intent::Steer(match pin {
                TurnPin::Live(turn_id) => SteerTarget::Pinned { message, turn_id },
                _ => SteerTarget::Message(message),
            }),
        )
    } else if let Asked::WholeTurn = asked {
        // W3: the recipient's own session must run a turn. S17 then.
        let own = match recipient {
            Recipient::Mono {
                chat: Some(chat), ..
            } => Some(chat),
            Recipient::Session { target, .. } => Some(target),
            Recipient::Mono { chat: None, .. } | Recipient::Unknown => None,
        };
        let running = own
            .filter(|target| target.exists)
            .and_then(|target| target.turn.as_ref().map(|turn| (target, turn)));
        let Some((chat, turn)) = running else {
            return reject(RejectCode::NothingRunning, to);
        };
        if !chat.steerable {
            return reject(RejectCode::CannotSteer, to);
        }
        // One row, one route: nothing here reads the recipient's sessions.
        (
            0,
            Route {
                session: chat.session.clone(),
                via: Via::Steer,
            },
            Intent::Steer(SteerTarget::Turn {
                turn_id: turn.clone(),
            }),
        )
    } else {
        // S16: new work goes to the recipient's chat or the project session,
        // and always waits for the turn end.
        let session = match recipient {
            Recipient::Session { target, .. } => &target.session,
            Recipient::Mono {
                chat: Some(chat), ..
            } if chat.exists => &chat.session,
            _ => return reject(RejectCode::TargetNoConversation, to),
        };
        (
            0,
            Route {
                session: session.clone(),
                via: Via::Queue,
            },
            Intent::New,
        )
    };
    // S18, S19: every accepted path counts in both.
    if hop > MAX_HOP {
        return reject_at(RejectCode::HopLimit, to, hop);
    }
    if about.pair.count >= RATE_LIMIT {
        return reject(RejectCode::RateLimited, to);
    }
    Decision::Accept {
        hop,
        rows: vec![Outgoing {
            to: to.expect("a resolved recipient is a party"),
            route,
            intent,
        }],
    }
}

/// The first 60 code points of a message, on one line, for hints.
fn preview(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        short.push('…');
    }
    short
}

/// Design 6.4.
pub fn decide_reply(input: &ReplyInput, hash: &PayloadHash, facts: &Facts<ReplyFacts>) -> Decision {
    let to = input.to.as_deref();
    if let Err(decision) = prelude(facts, hash, |_, code| {
        reply_again(code, to, &facts.about.then)
    }) {
        return decision;
    }
    if too_large(&input.message) {
        return reject(RejectCode::TooLarge, None);
    }
    let named = match answers(to, &facts.about.handled) {
        Ok(named) => named,
        Err(rejection) => return rejection.into(),
    };
    let hop = named
        .iter()
        .map(|handled| handled.message.hop)
        .max()
        .unwrap_or(0)
        + 1;
    // R9, between R8 and R10.
    if hop > MAX_HOP {
        return reject_at(RejectCode::HopLimit, None, hop);
    }
    if let Some(rejection) = target_gone(&named) {
        return rejection.into();
    }
    let mut rows = Vec::with_capacity(named.len());
    for handled in &named {
        let from = handled
            .message
            .from_mono_id
            .clone()
            .expect("target_gone refused a row with no sender Mono");
        let sender = &handled.sender;
        rows.push(Outgoing {
            to: Party::Mono { mono_id: from },
            route: Route {
                session: sender.session.clone(),
                via: if sender.running() && sender.steerable {
                    Via::Steer
                } else {
                    Via::Queue
                },
            },
            intent: Intent::Reply(handled.message.id.clone()),
        });
    }
    Decision::Accept { hop, rows }
}

/// R5-R8: the messages a reply answers, or why it answers none. Each handle
/// of `to` is checked in order.
fn answers<'a>(
    to: Option<&[Handle]>,
    handled: &'a [Handled],
) -> Result<Vec<&'a Handled>, Rejection> {
    let open = open_set(handled);
    let Some(to) = to else {
        return match open.as_slice() {
            [] => Err(listing(RejectCode::NoOpenHandle, None, &open)),
            [one] => Ok(vec![*one]),
            several => Err(ambiguous_reply(several)),
        };
    };
    to.iter()
        .map(|handle| {
            let row = handled.iter().find(|row| row.handle == *handle);
            match row.map(|row| (row, row.state)) {
                Some((row, HandleState::Open)) => Ok(row),
                Some((_, HandleState::Closed(closure))) => Err(handle_closed(*handle, closure)),
                Some((_, HandleState::Unseen)) | None => Err(unknown_handle(*handle, &open)),
            }
        })
        .collect()
}

fn open_set(handled: &[Handled]) -> Vec<&Handled> {
    handled
        .iter()
        .filter(|row| row.state == HandleState::Open)
        .collect()
}

/// A rejection that lists `open`, with `hint` or else the code's own.
fn listing(code: RejectCode, hint: Option<String>, open: &[&Handled]) -> Rejection {
    Rejection {
        code,
        hint: hint.unwrap_or_else(|| code.hint().into()),
        open_handles: open
            .iter()
            .map(|row| OpenHandle {
                handle: row.handle,
                message_id: row.message.id.clone(),
                from: row.from.clone(),
            })
            .collect(),
    }
}

fn ambiguous_reply(open: &[&Handled]) -> Rejection {
    let listed: Vec<String> = open
        .iter()
        .map(|row| {
            format!(
                "{} (from {}: {})",
                row.handle,
                row.from,
                preview(row.message.body.text())
            )
        })
        .collect();
    listing(
        RejectCode::AmbiguousReply,
        Some(format!(
            "Several handles are open: {}. Pass to.",
            listed.join(", ")
        )),
        open,
    )
}

fn unknown_handle(handle: Handle, open: &[&Handled]) -> Rejection {
    let names: Vec<String> = open.iter().map(|row| row.handle.to_string()).collect();
    let names = if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    };
    listing(
        RejectCode::UnknownHandle,
        Some(format!(
            "No handle {handle} in this turn. Open handles: {names}."
        )),
        open,
    )
}

/// R10 (PR 5 review): the named handles whose sender is gone, by name, so
/// the agent can leave them out of `to`. A deleted sender Mono has no chat
/// to take the answer either.
fn target_gone(named: &[&Handled]) -> Option<Rejection> {
    let gone: Vec<String> = named
        .iter()
        .filter(|row| !row.sender.exists || row.message.from_mono_id.is_none())
        .map(|row| row.handle.to_string())
        .collect();
    if gone.is_empty() {
        return None;
    }
    let others = if gone.len() < named.len() {
        " Answer the other handles with to."
    } else {
        ""
    };
    Some(listing(
        RejectCode::TargetGone,
        Some(format!(
            "The sender of {} no longer exists (cleared or deleted).{others}",
            gone.join(", ")
        )),
        &[],
    ))
}

fn handle_closed(handle: Handle, closure: Closure) -> Rejection {
    listing(
        RejectCode::HandleClosed,
        Some(format!("{handle} is already {}.", closure.as_str())),
        &[],
    )
}

/// Design 3.4 (PR 4a review m3): a duplicate of a rejected reply answers its
/// rejection again. `then` holds the caller's handles as they stood when the
/// first call ran, so the hint and the open handles are the ones it got.
fn reply_again(code: RejectCode, to: Option<&[Handle]>, then: &[Handled]) -> Rejection {
    let open = open_set(then);
    // That call got past every handle before the first one that was not open.
    let failed = to.and_then(|to| {
        to.iter()
            .copied()
            .find(|handle| !open.iter().any(|row| row.handle == *handle))
    });
    let closure = |handle: Handle| {
        then.iter().find_map(|row| match row.state {
            HandleState::Closed(closure) if row.handle == handle => Some(closure),
            _ => None,
        })
    };
    match (code, failed) {
        (RejectCode::AmbiguousReply, _) if !open.is_empty() => ambiguous_reply(&open),
        (RejectCode::UnknownHandle, Some(handle)) => unknown_handle(handle, &open),
        (RejectCode::HandleClosed, Some(handle)) => match closure(handle) {
            Some(closure) => handle_closed(handle, closure),
            None => listing(code, None, &[]),
        },
        (RejectCode::TargetGone, _) => {
            let named: Vec<&Handled> = match to {
                Some(to) => open
                    .iter()
                    .copied()
                    .filter(|row| to.contains(&row.handle))
                    .collect(),
                None => open.clone(),
            };
            target_gone(&named).unwrap_or_else(|| listing(code, None, &[]))
        }
        _ => listing(code, None, &open),
    }
}

/// The `payload_hash` of an A row (design 6.6): sha256 hex of the compact
/// JSON text of `[action, input]`.
/// Object keys are sorted (serde_json without `preserve_order`), so the key
/// order of the call never changes the hash.
pub fn action_hash(action: &str, input: &Value) -> PayloadHash {
    sha256(&serde_json::to_string(&(action, input)).expect("a JSON value serializes"))
}

/// What the A rows read (design 6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFacts {
    /// A session caller's level (stored row wins); `None` for a Mono chat or
    /// habit run.
    pub caller_level: Option<SessionLevel>,
    /// The caller Mono's `monos.role`; `None` for a session caller or a Mono
    /// with no `monos` row.
    pub caller_role: Option<MonoRole>,
    /// The level of the session the action's target names, when it names an
    /// existing session.
    pub target_level: Option<SessionLevel>,
    /// The row this caller session already wrote with this request id (part
    /// 0, any tool).
    pub prior: Option<MonoMessage>,
    /// A1: the session this request id started was archived or deleted
    /// since: the session an accepted start row names, or, with no row, the
    /// session the start would launch.
    pub prior_start_ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDecision {
    /// A6, A7, or A1 with an accepted start row: the action runs.
    /// `lead_session_id` is the lead id `sessions.start` writes on the new
    /// session.
    Allow { lead_session_id: Option<SessionId> },
    /// A1 or A2 refusing the request id: no row.
    Refuse { code: Refusal, hint: String },
    /// A1 with a rejected row: its answer again, no new row.
    Replay {
        prior: MessageId,
        code: RejectCode,
        hint: String,
    },
    /// A3, A4, A5, A8: one rejected row with `tool` (`StartSession` or
    /// `AppAction`).
    Reject {
        code: RejectCode,
        hint: String,
        tool: Tool,
    },
}

pub(crate) const START_ENDED: &str =
    "The session this request id started was archived or deleted. Use a new request id.";

/// Design 6.6, rows A1 and A2, then the Giga row for approvals.*, then A3,
/// A4, A5, A8, A6, A7 in this order, for one app action other than
/// `messages.*` whose `[action, input]` hashes to `hash`.
pub fn decide_action(
    caller: &Caller,
    action: &str,
    hash: &PayloadHash,
    facts: &ActionFacts,
) -> ActionDecision {
    let refuse = |hint: &str| ActionDecision::Refuse {
        code: Refusal::RequestIdReused,
        hint: hint.into(),
    };
    if let Some(prior) = &facts.prior {
        if prior.payload_hash != *hash {
            return refuse(REQUEST_ID_REUSED);
        }
        return match prior.status {
            Status::Rejected { gate_result, .. } => ActionDecision::Replay {
                prior: prior.id.clone(),
                code: gate_result,
                hint: action_hint(gate_result, facts),
            },
            // `host.start` repeats a launch only while its session lives.
            _ if facts.prior_start_ended => refuse(START_ENDED),
            // Only a start writes an accepted row. Its first outcome stands
            // (A1), so A3 to A7 do not run again: a session caller is the
            // new session's lead, as A6 made it the first time.
            _ => ActionDecision::Allow {
                lead_session_id: caller
                    .mono_id()
                    .is_none()
                    .then(|| caller.session_id.clone()),
            },
        };
    }
    if facts.prior_start_ended {
        return refuse(START_ENDED);
    }
    if action.starts_with("approvals.") && facts.caller_role != Some(MonoRole::Giga) {
        return ActionDecision::Reject {
            code: RejectCode::NotAGiga,
            hint: action_hint(RejectCode::NotAGiga, facts),
            tool: Tool::AppAction,
        };
    }
    let reject = |code: RejectCode, tool: Tool, lead: &SessionId| ActionDecision::Reject {
        code,
        hint: with_lead(code, lead),
        tool,
    };
    let starts = action == "sessions.start";
    if let Some(SessionLevel::Worker { lead }) = &facts.caller_level {
        return if starts {
            reject(RejectCode::WorkerCannotSpawn, Tool::StartSession, lead)
        } else {
            reject(RejectCode::WorkerScope, Tool::AppAction, lead)
        };
    }
    if let Some(SessionLevel::Worker { lead }) = &facts.target_level {
        if matches!(action, "sessions.send" | "sessions.draft") && *lead != caller.session_id {
            return reject(RejectCode::SteerViaLead, Tool::AppAction, lead);
        }
    }
    let lead = facts.caller_level == Some(SessionLevel::Lead);
    let started = matches!(
        caller.speaker,
        Speaker::Session {
            access: Access::Started,
            ..
        }
    );
    if lead && started {
        let own_worker = matches!(
            &facts.target_level,
            Some(SessionLevel::Worker { lead }) if *lead == caller.session_id
        );
        if !started_lead_may(action, own_worker) {
            return ActionDecision::Reject {
                code: RejectCode::LeadScope,
                hint: RejectCode::LeadScope.hint().into(),
                tool: Tool::AppAction,
            };
        }
    }
    ActionDecision::Allow {
        lead_session_id: (starts && lead).then(|| caller.session_id.clone()),
    }
}

/// A8 (design 6.6): the addendum's powers of a lead whose access is only the
/// `startedBy` stamp, plus the reads it needs to pick a model and a worktree.
fn started_lead_may(action: &str, own_worker: bool) -> bool {
    match action {
        "models.list" | "sessions.list" | "sessions.start" | "worktrees.list"
        | "worktrees.create" => true,
        "sessions.read" | "sessions.send" | "sessions.draft" | "sessions.stop"
        | "sessions.archive" | "sessions.delete" => own_worker,
        _ => false,
    }
}

/// The hint of an A row from its facts, for the Giga row and again for a
/// replay: a lead id only ever goes to NULL (design 13.2), so a replay names
/// the same lead while the link stands.
fn action_hint(code: RejectCode, facts: &ActionFacts) -> String {
    match (code, &facts.caller_level, &facts.target_level) {
        (
            RejectCode::WorkerCannotSpawn | RejectCode::WorkerScope | RejectCode::NotAGiga,
            Some(SessionLevel::Worker { lead }),
            _,
        )
        | (RejectCode::SteerViaLead, _, Some(SessionLevel::Worker { lead })) => {
            with_lead(code, lead)
        }
        _ => code.hint().into(),
    }
}

/// TS `pathKey` (`src/shared/lib/paths.ts`), the key of design 6.1 "shares
/// project"; `src/shared/lib/pathKey.fixtures.json` holds the keys both give.
/// A Windows path (a drive letter and a slash or backslash, or `//` or `\\`)
/// gets `/` for each backslash, as every path that does not start with `/`
/// does on Windows. Trailing `/` go, and an empty result is `/`. A drive or
/// UNC path is lowercased; any other keeps its case. `~`, `..` and inner `//`
/// stay as written.
pub fn path_key(path: &str) -> String {
    let drive = |path: &str, after: fn(Option<&u8>) -> bool| {
        let bytes = path.as_bytes();
        bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && after(bytes.get(2))
    };
    let windows = drive(path, |next| matches!(next, Some(b'/' | b'\\')))
        || path.starts_with("\\\\")
        || path.starts_with("//");
    let slashed = if windows || (cfg!(windows) && !path.starts_with('/')) {
        Cow::Owned(path.replace('\\', "/"))
    } else {
        Cow::Borrowed(path)
    };
    let trimmed = match slashed.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    };
    if drive(trimmed, |next| matches!(next, None | Some(b'/'))) || trimmed.starts_with("//") {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mono_messages::{
        Body, DeliveredSlot, Part, PlainSlot, RequestId, Routed, Status, WaitingSlot, Wanting,
    };

    fn mono(id: &str) -> MonoId {
        MonoId::parse(id).unwrap()
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn caller(kind: CallerKind) -> Caller {
        Caller {
            speaker: Speaker::Mono {
                kind,
                mono_id: mono("giga"),
            },
            session_id: session("giga-chat"),
            turn_id: "turn-1".into(),
        }
    }

    fn new(message: &str, send_to: &str, wanting_reply: bool) -> SendInput {
        SendInput {
            message: message.into(),
            send_to: send_to.into(),
            wanting_reply,
            intent: IntentKind::New,
            target: None,
        }
    }

    fn asking(intent: IntentKind, reply_to: Option<&str>) -> SendInput {
        SendInput {
            intent,
            target: reply_to.map(|named| SendTarget::Message(MessageId::parse(named).unwrap())),
            ..new("Hi", "mono-a", false)
        }
    }

    fn steer(reply_to: &str) -> SendInput {
        asking(IntentKind::Steer, Some(reply_to))
    }

    /// A send of `intent` with `scope: "turn"` to `mono-a`.
    fn whole(intent: IntentKind) -> SendInput {
        SendInput {
            intent,
            target: Some(SendTarget::Turn),
            ..new("Stop", "mono-a", false)
        }
    }

    /// A session that runs `turn-1` while `running`.
    fn live(id: &str, running: bool, steerable: bool) -> Target {
        Target {
            session: session(id),
            exists: true,
            turn: running.then(|| "turn-1".to_string()),
            steerable,
        }
    }

    fn mono_a(chat: Option<Target>) -> Recipient {
        Recipient::Mono {
            mono_id: mono("mono-a"),
            role: MonoRole::Mono,
            chat,
            projects: Vec::new(),
            allowlist: Vec::new(),
        }
    }

    /// The caller's `monos` row: the Giga with its chat `giga-chat`.
    fn giga_row() -> CallerMono {
        CallerMono {
            chat: Some(session("giga-chat")),
            role: MonoRole::Giga,
            projects: Vec::new(),
            allowlist: Vec::new(),
        }
    }

    fn facts<A>(about: A) -> Facts<A> {
        Facts {
            caller: Some(CallerFacts::Mono(giga_row())),
            prior: Vec::new(),
            about,
        }
    }

    const NOW: i64 = 1_000_000_000;
    const DAY_MS: i64 = 86_400_000;
    const SCOPE: &str = "You cannot steer this recipient's whole turn. A Giga may steer a Mono's whole turn, and a Mono one of its leads. Steer one message with reply_to, or send with intent \"new\".";
    const UPWARD: &str = "You can steer a Giga only about a message it sent you in the last 24 h. Answer an open handle with reply, or send with intent \"new\"; it is queued.";
    const TARGET_GONE: &str = r#"The work that reply_to started is not running now: it finished, it has not started, or its session is idle, cleared or deleted. Send it with intent "new"."#;

    fn sends(recipient: Recipient) -> Facts<SendFacts> {
        facts(SendFacts {
            recipient,
            reply_to: None,
            pair: PairCount::default(),
        })
    }

    /// A send whose `reply_to` resolved to `message`, handled in `session`.
    fn steering(
        recipient: Recipient,
        message: MonoMessage,
        session: Option<Target>,
    ) -> Facts<SendFacts> {
        facts(SendFacts {
            recipient,
            reply_to: Some(Referenced { message, session }),
            pair: PairCount::default(),
        })
    }

    fn hash() -> PayloadHash {
        Input::Send(new("Ship it", "mono-a", false)).payload_hash()
    }

    fn row(id: char, from: Option<&str>, hop: u32, status: Status) -> MonoMessage {
        MonoMessage {
            id: MessageId::parse(&format!("m_{}", id.to_string().repeat(12))).unwrap(),
            request_id: RequestId::parse("req-1").unwrap(),
            part: Part::FIRST,
            payload_hash: hash(),
            tool: Tool::SendMessage,
            from_mono_id: from.map(mono),
            from_session_id: session("giga-chat"),
            to: Some(Party::Mono {
                mono_id: mono("mono-a"),
            }),
            addressed: "mono-a".into(),
            body: Body::new("Review PR 12 before the release goes out to everyone"),
            hop,
            created_at: 1,
            status,
        }
    }

    fn open(handle: i64) -> Status {
        Status::Open {
            wanting_reply: Wanting,
            intent: Intent::New,
            routed: Routed {
                handled_by_session: session("a-chat"),
                delivery: Delivery::Submit,
            },
            slot: Some(WaitingSlot {
                turn_id: "turn-1".into(),
                handle: Handle::new(handle).unwrap(),
                delivered_at: Some(2),
                window: "main".into(),
            }),
        }
    }

    fn handled(id: char, handle: i64, sender: Target) -> Handled {
        Handled {
            message: row(id, Some("giga"), 1, open(handle)),
            handle: Handle::new(handle).unwrap(),
            state: HandleState::Open,
            from: "Boss".into(),
            sender,
        }
    }

    fn replies(handled: Vec<Handled>) -> Facts<ReplyFacts> {
        facts(ReplyFacts {
            handled,
            then: Vec::new(),
        })
    }

    fn rejected(code: RejectCode, to: Option<Party>) -> Decision {
        Decision::Reject {
            code,
            hint: code.hint().into(),
            to,
            hop: 0,
            open_handles: Vec::new(),
        }
    }

    fn party_a() -> Option<Party> {
        Some(Party::Mono {
            mono_id: mono("mono-a"),
        })
    }

    fn reply(message: &str) -> ReplyInput {
        ReplyInput {
            message: message.into(),
            to: None,
            wanting_reply: false,
        }
    }

    #[test]
    fn s1_a_send_without_wanting_reply_or_with_unknown_fields_is_invalid_input() {
        let parse_send = |raw: Value| parse(Tool::SendMessage, &raw);
        let missing = parse_send(json!({"message": "Hi", "send_to": "mono-a"})).unwrap_err();
        assert!(missing.contains("wanting_reply"), "{missing}");
        let unknown = parse_send(
            json!({"message": "Hi", "send_to": "mono-a", "wanting_reply": false, "intent": "new", "from": "x"}),
        )
        .unwrap_err();
        assert!(unknown.contains("from"), "{unknown}");
        assert!(parse_send(
            json!({"message": "  ", "send_to": "mono-a", "wanting_reply": false, "intent": "new"})
        )
        .is_err());
        assert!(parse_send(
            json!({"message": "Hi", "send_to": "", "wanting_reply": false, "intent": "new"})
        )
        .is_err());
        assert_eq!(
            parse_send(
                json!({"message": "Hi", "send_to": "mono-a", "wanting_reply": true, "intent": "new"})
            ),
            Ok(Input::Send(new("Hi", "mono-a", true)))
        );
    }

    // PR I2. Defect: the old field is ignored and the call silently changes
    // meaning, an intent is guessed, or a second target is taken.
    #[test]
    fn s1_action_type_a_missing_intent_and_a_list_of_targets_are_invalid_input() {
        let parse_send = |raw: Value| parse(Tool::SendMessage, &raw);
        let action = parse_send(json!({
            "message": "Hi", "send_to": "mono-a", "wanting_reply": false,
            "intent": "new", "action_type": "queue",
        }))
        .unwrap_err();
        assert_eq!(
            action,
            "Unknown send_message fields: action_type. Allowed: message, send_to, wanting_reply, intent, reply_to, scope."
        );
        for intent in [json!(null), json!("queue"), json!("New"), json!(1)] {
            let error = parse_send(json!({
                "message": "Hi", "send_to": "mono-a", "wanting_reply": false, "intent": intent,
            }))
            .unwrap_err();
            assert_eq!(error, INTENT_RULE, "{intent}");
        }
        assert_eq!(
            parse_send(json!({"message": "Hi", "send_to": "mono-a", "wanting_reply": false})),
            Err(INTENT_RULE.to_string())
        );
        for reply_to in [
            json!(["m_aaaaaaaaaaaa"]),
            json!("m_AAAAAAAAAAAA"),
            json!("#1"),
            json!("#0"),
            json!(""),
            json!(7),
        ] {
            let error = parse_send(json!({
                "message": "Hi", "send_to": "mono-a", "wanting_reply": false,
                "intent": "steer", "reply_to": reply_to,
            }))
            .unwrap_err();
            assert_eq!(error, REPLY_TO_RULE, "{reply_to}");
        }
        // PR I2 review r1 M3: a handle is refused, and the hint says what to
        // pass instead.
        assert!(
            REPLY_TO_RULE.contains("use the message_id your send_message returned")
                && !REPLY_TO_RULE.contains("handle"),
            "{REPLY_TO_RULE}"
        );
        assert_eq!(
            parse_send(json!({
                "message": "Hi", "send_to": "mono-a", "wanting_reply": false,
                "intent": "steer", "reply_to": "m_aaaaaaaaaaaa",
            })),
            Ok(Input::Send(steer("m_aaaaaaaaaaaa")))
        );
    }

    #[test]
    fn r1_to_is_one_handle_or_up_to_nine_different_handles() {
        let parse_reply = |raw: Value| parse(Tool::Reply, &raw);
        let to = |numbers: &[i64]| ReplyInput {
            to: Some(numbers.iter().map(|n| Handle::new(*n).unwrap()).collect()),
            ..reply("Done")
        };
        assert_eq!(
            parse_reply(json!({"message": "Done", "to": "#1"})),
            Ok(Input::Reply(to(&[1])))
        );
        assert_eq!(
            parse_reply(json!({"message": "Done", "to": ["#2", "#10"]})),
            Ok(Input::Reply(to(&[2, 10])))
        );
        let nine: Vec<String> = (91..=99).map(|n| format!("#{n}")).collect();
        assert!(parse_reply(json!({"message": "Done", "to": nine})).is_ok());
        let ten: Vec<String> = (1..=10).map(|n| format!("#{n}")).collect();
        for bad in [
            json!(null),
            json!([]),
            json!(1),
            json!("1"),
            json!("#0"),
            json!("#01"),
            json!("#100"),
            json!(" #1"),
            json!(["#1", "#1"]),
            json!(["#1", 2]),
            json!(ten),
        ] {
            let error = parse_reply(json!({"message": "Done", "to": bad})).unwrap_err();
            assert!(error.starts_with("to must be"), "{bad}: {error}");
        }
        assert!(parse_reply(json!({"wanting_reply": true})).is_err());
        assert_eq!(
            parse_reply(json!({"message": "Done"})),
            Ok(Input::Reply(reply("Done")))
        );
    }

    #[test]
    fn a_call_from_a_mono_whose_row_is_gone_is_refused_not_a_mono() {
        let mut gone = sends(mono_a(None));
        gone.caller = None;
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "mono-a", false),
                &hash(),
                &gone,
                NOW,
            ),
            Decision::Refuse {
                code: Refusal::NotAMono,
                hint: NOT_A_MONO.into()
            }
        );
    }

    #[test]
    fn s2_the_same_call_again_is_a_duplicate_of_its_rows() {
        let prior = vec![row('a', Some("giga"), 0, open(1))];
        let mut again = sends(mono_a(Some(live("a-chat", false, true))));
        again.prior = prior.clone();
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Ship it", "mono-a", false),
                &hash(),
                &again,
                NOW,
            ),
            Decision::Duplicate {
                prior,
                rejection: None
            }
        );
    }

    #[test]
    fn s3_the_same_request_id_with_other_input_is_refused() {
        let mut again = sends(mono_a(Some(live("a-chat", false, true))));
        again.prior = vec![row('a', Some("giga"), 0, open(1))];
        let other = new("Ship it now", "mono-a", false);
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &other,
                &Input::Send(other.clone()).payload_hash(),
                &again,
                NOW,
            ),
            Decision::Refuse {
                code: Refusal::RequestIdReused,
                hint: REQUEST_ID_REUSED.into()
            }
        );
    }

    // PR 4a review m7 (G31, G32), PR I2 (intents review r1 M2). Defect: a
    // request id reused for another recipient, intent or target is answered
    // as a duplicate of the first message, so the second one is never sent.
    #[test]
    fn s3_a_request_id_reused_for_another_recipient_intent_or_target_is_refused() {
        let first = new("Hi", "mono-a", false);
        let mut again = sends(mono_a(Some(live("a-chat", false, true))));
        again.prior = vec![MonoMessage {
            payload_hash: Input::Send(first.clone()).payload_hash(),
            ..row('a', Some("giga"), 0, open(1))
        }];
        let first_steer = steer("m_bbbbbbbbbbbb");
        let mut steered = again.clone();
        steered.prior[0].payload_hash = Input::Send(first_steer.clone()).payload_hash();
        for (facts, other) in [
            (&again, new("Hi", "mono-b", false)),
            (&again, asking(IntentKind::Steer, None)),
            (&steered, steer("m_cccccccccccc")),
            (&steered, asking(IntentKind::New, Some("m_bbbbbbbbbbbb"))),
        ] {
            assert_eq!(
                decide_send(
                    &caller(CallerKind::MonoChat),
                    &other,
                    &Input::Send(other.clone()).payload_hash(),
                    facts,
                    NOW,
                ),
                Decision::Refuse {
                    code: Refusal::RequestIdReused,
                    hint: REQUEST_ID_REUSED.into()
                },
                "{other:?}"
            );
        }
        assert!(matches!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &first_steer,
                &Input::Send(first_steer.clone()).payload_hash(),
                &steered,
                NOW,
            ),
            Decision::Duplicate { .. }
        ));
    }

    #[test]
    fn s4_more_than_8000_characters_is_too_large() {
        let at = |chars: usize| {
            decide_send(
                &caller(CallerKind::MonoChat),
                &new(&"é".repeat(chars), "mono-a", false),
                &hash(),
                &sends(mono_a(Some(live("a-chat", false, true)))),
                NOW,
            )
        };
        assert_eq!(at(8001), rejected(RejectCode::TooLarge, party_a()));
        assert!(matches!(at(8000), Decision::Accept { .. }));
        // Outside the BMP: one code point, two UTF-16 units, four bytes.
        let emoji = decide_send(
            &caller(CallerKind::MonoChat),
            &new(&"😀".repeat(8000), "mono-a", false),
            &hash(),
            &sends(mono_a(Some(live("a-chat", false, true)))),
            NOW,
        );
        assert!(matches!(emoji, Decision::Accept { .. }), "{emoji:?}");
    }

    /// The rejection a send gets, with the hint of design 16.5.
    fn send_rejected(code: RejectCode, to: Option<Party>, hint: &str) -> Decision {
        Decision::Reject {
            code,
            hint: hint.into(),
            to,
            hop: 0,
            open_handles: Vec::new(),
        }
    }

    /// The S18 or R9 rejection of a call that would have had hop 5 (E25).
    fn hop_limited(to: Option<Party>) -> Decision {
        Decision::Reject {
            code: RejectCode::HopLimit,
            hint: "This reply chain is 4 replies deep. Stop here or start a new message.".into(),
            to,
            hop: 5,
            open_handles: Vec::new(),
        }
    }

    // PR I2, rows I1 and I2. Defect: a mislabelled call is accepted, or an
    // agent's continue or reply passes and leaves the question open.
    #[test]
    fn i1_i2_an_intent_that_disagrees_with_reply_to_or_that_agents_cannot_send_is_refused() {
        let decide = |input: SendInput| {
            decide_send(
                &caller(CallerKind::MonoChat),
                &input,
                &hash(),
                &sends(mono_a(Some(live("a-chat", true, true)))),
                NOW,
            )
        };
        let new_with_target = r#"intent "new" takes no reply_to. Remove reply_to, or use intent "steer" to change the work that message started."#;
        let needs =
            |intent: &str| format!(r#"intent "{intent}" needs reply_to: one message_id (m_…)."#);
        let only_steer = r#"scope "turn" is for intent "steer" only."#;
        for (input, code, hint) in [
            (
                asking(IntentKind::New, Some("m_aaaaaaaaaaaa")),
                RejectCode::IntentMismatch,
                new_with_target.to_string(),
            ),
            // Whole-turn: new, reply and continue take no scope.
            (
                whole(IntentKind::New),
                RejectCode::IntentMismatch,
                r#"intent "new" takes no scope. Remove scope, or use intent "steer"."#.to_string(),
            ),
            (
                whole(IntentKind::Reply),
                RejectCode::IntentMismatch,
                only_steer.to_string(),
            ),
            (
                whole(IntentKind::Continue),
                RejectCode::IntentMismatch,
                only_steer.to_string(),
            ),
            // I1 before I2: continue without reply_to gets the shape code.
            (
                asking(IntentKind::Continue, None),
                RejectCode::IntentMismatch,
                needs("continue"),
            ),
            (
                asking(IntentKind::Reply, None),
                RejectCode::IntentMismatch,
                needs("reply"),
            ),
            (
                asking(IntentKind::Continue, Some("m_aaaaaaaaaaaa")),
                RejectCode::IntentNotAllowed,
                "Agents cannot send continue. Use reply or steer.".to_string(),
            ),
            (
                asking(IntentKind::Reply, Some("m_aaaaaaaaaaaa")),
                RejectCode::IntentNotAllowed,
                r##"send_message cannot reply. Use reply (to: "#n") to answer a message that waits for you."##.to_string(),
            ),
        ] {
            assert_eq!(
                decide(input.clone()),
                send_rejected(code, party_a(), &hint),
                "{input:?}"
            );
        }
    }

    // PR I2, whole-turn. Defect: a row out of place gives the wrong code, so
    // a model fixes the wrong thing first.
    #[test]
    fn w1_runs_after_s4_and_i1_and_before_s5() {
        let decide = |input: SendInput, recipient: Recipient| {
            decide_send(
                &caller(CallerKind::MonoChat),
                &input,
                &hash(),
                &sends(recipient),
                NOW,
            )
        };
        let a_running = || mono_a(Some(live("a-chat", true, true)));
        let long = SendInput {
            message: "x".repeat(8001),
            ..asking(IntentKind::Steer, None)
        };
        assert_eq!(
            decide(long, a_running()),
            rejected(RejectCode::TooLarge, party_a())
        );
        assert!(matches!(
            decide(whole(IntentKind::New), a_running()),
            Decision::Reject {
                code: RejectCode::IntentMismatch,
                ..
            }
        ));
        let nowhere = SendInput {
            send_to: "nope".into(),
            ..asking(IntentKind::Steer, None)
        };
        assert_eq!(
            decide(nowhere, Recipient::Unknown),
            rejected(RejectCode::SteerNeedsTarget, None)
        );
    }

    // Whole-turn, S1. Defect: a call with two targets is accepted and one
    // target is ignored in silence, or a scope other than "turn" is read
    // as one.
    #[test]
    fn s1_reply_to_and_scope_together_or_a_wrong_scope_are_invalid_input() {
        let parse_send = |extra: Value| {
            let mut raw = json!({
                "message": "Stop", "send_to": "mono-a", "wanting_reply": false, "intent": "steer",
            });
            raw.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse(Tool::SendMessage, &raw)
        };
        assert_eq!(
            parse_send(json!({"reply_to": "m_aaaaaaaaaaaa", "scope": "turn"})),
            Err(TARGET_RULE.to_string())
        );
        for scope in [json!("session"), json!(true), json!(null), json!(["turn"])] {
            assert_eq!(
                parse_send(json!({ "scope": scope })),
                Err(SCOPE_RULE.to_string()),
                "{scope}"
            );
        }
        assert_eq!(
            parse_send(json!({"scope": "turn"})),
            Ok(Input::Send(whole(IntentKind::Steer)))
        );
    }

    /// A whole-turn steer of `caller_role` to `recipient`.
    fn steer_turn(
        caller_kind: CallerKind,
        caller_role: MonoRole,
        recipient: Recipient,
    ) -> Decision {
        let mut facts = sends(recipient);
        facts.caller = Some(CallerFacts::Mono(CallerMono {
            role: caller_role,
            ..giga_row()
        }));
        decide_send(
            &caller(caller_kind),
            &whole(IntentKind::Steer),
            &hash(),
            &facts,
            NOW,
        )
    }

    fn mono_with_role(id: &str, role: MonoRole, chat: Target) -> Recipient {
        Recipient::Mono {
            mono_id: mono(id),
            role,
            chat: Some(chat),
            projects: Vec::new(),
            allowlist: Vec::new(),
        }
    }

    fn project(running: bool) -> Recipient {
        Recipient::Session {
            target: live("p", running, true),
            own_project: true,
            level: SessionLevel::Lead,
        }
    }

    // Whole-turn, W1. Defect: the missing target falls back to a whole-turn
    // steer, which the spec forbids.
    #[test]
    fn w1_a_steer_with_no_target_is_steer_needs_target_even_when_whole_turn_would_pass() {
        let a_running = mono_a(Some(live("a-chat", true, true)));
        assert!(matches!(
            steer_turn(CallerKind::MonoChat, MonoRole::Giga, a_running.clone()),
            Decision::Accept { .. }
        ));
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &asking(IntentKind::Steer, None),
                &hash(),
                &sends(a_running),
                NOW,
            ),
            send_rejected(
                RejectCode::SteerNeedsTarget,
                party_a(),
                "name a message or use scope turn"
            )
        );
    }

    // Whole-turn, W2 (design 16.12.2.2). Defect: a sender the spec refuses
    // steers a whole turn, or a Mono cannot steer its own lead's.
    #[test]
    fn w2_a_giga_may_steer_a_monos_whole_turn_and_a_mono_a_leads() {
        let running = |id: &str| live(id, true, true);
        let a = || mono_a(Some(running("a-chat")));
        for kind in [CallerKind::MonoChat, CallerKind::HabitRun] {
            for (role, recipient) in [(MonoRole::Giga, a()), (MonoRole::Mono, project(true))] {
                assert!(
                    matches!(steer_turn(kind, role, recipient), Decision::Accept { .. }),
                    "{kind:?} {role:?}"
                );
            }
        }
        for (case, role, recipient) in [
            (
                "Mono to Giga",
                MonoRole::Mono,
                mono_with_role("giga-2", MonoRole::Giga, running("g2-chat")),
            ),
            (
                "Giga to Giga",
                MonoRole::Giga,
                mono_with_role("giga-2", MonoRole::Giga, running("g2-chat")),
            ),
            ("Mono to Mono", MonoRole::Mono, a()),
            ("Giga to project session", MonoRole::Giga, project(true)),
        ] {
            let to = recipient.party();
            assert_eq!(
                steer_turn(CallerKind::MonoChat, role, recipient),
                send_rejected(RejectCode::SteerScopeNotAllowed, to, SCOPE),
                "{case}"
            );
        }
    }

    // Whole-turn. Defect: the scope code hides the real problem with the
    // recipient.
    #[test]
    fn w2_runs_after_s5_s6_s7() {
        let me = mono_with_role("giga", MonoRole::Giga, live("giga-chat", true, true));
        let outside = Recipient::Session {
            target: live("other", true, true),
            own_project: false,
            level: SessionLevel::Lead,
        };
        for (recipient, code) in [
            (Recipient::Unknown, RejectCode::UnknownTarget),
            (me, RejectCode::SelfSend),
            (outside, RejectCode::DirectionForbidden),
        ] {
            assert!(
                matches!(
                    steer_turn(CallerKind::MonoChat, MonoRole::Giga, recipient),
                    Decision::Reject { code: got, .. } if got == code
                ),
                "{code:?}"
            );
        }
    }

    // Whole-turn, W3. Defect: an idle recipient takes the steer as new work,
    // or the answer is target_no_conversation.
    #[test]
    fn w3_an_idle_or_absent_recipient_is_nothing_running() {
        let cleared = Target {
            exists: false,
            ..live("a-chat", true, true)
        };
        for (case, chat) in [
            ("idle", Some(live("a-chat", false, true))),
            ("no chat", None),
            ("cleared", Some(cleared)),
        ] {
            assert_eq!(
                steer_turn(CallerKind::MonoChat, MonoRole::Giga, mono_a(chat)),
                send_rejected(
                    RejectCode::NothingRunning,
                    party_a(),
                    "send it as a new message"
                ),
                "{case}"
            );
        }
    }

    // Whole-turn. Defect: the sender learns about the recipient's state
    // before it learns that it may not send this at all.
    #[test]
    fn w2_runs_before_w3() {
        let idle_giga = mono_with_role("giga-2", MonoRole::Giga, live("g2-chat", false, true));
        assert!(matches!(
            steer_turn(CallerKind::MonoChat, MonoRole::Mono, idle_giga),
            Decision::Reject {
                code: RejectCode::SteerScopeNotAllowed,
                ..
            }
        ));
    }

    // Whole-turn, S17. Defect: a whole-turn steer queues into a provider
    // that can never take it.
    #[test]
    fn s17_a_whole_turn_steer_into_a_turn_that_cannot_steer_is_cannot_steer() {
        assert!(matches!(
            steer_turn(
                CallerKind::MonoChat,
                MonoRole::Giga,
                mono_a(Some(live("a-chat", true, false)))
            ),
            Decision::Reject {
                code: RejectCode::CannotSteer,
                ..
            }
        ));
    }

    // Whole-turn, S20. Defect: the steer is routed to another session,
    // waits for the turn end, or is pinned to no turn.
    #[test]
    fn s20_a_whole_turn_steer_goes_only_into_the_recipients_running_turn() {
        let chat = Target {
            turn: Some("turn-7".into()),
            ..live("a-chat", true, true)
        };
        assert_eq!(
            steer_turn(CallerKind::MonoChat, MonoRole::Giga, mono_a(Some(chat))),
            Decision::Accept {
                hop: 0,
                rows: vec![Outgoing {
                    to: party_a().unwrap(),
                    route: Route {
                        session: session("a-chat"),
                        via: Via::Steer
                    },
                    intent: Intent::Steer(SteerTarget::Turn {
                        turn_id: "turn-7".into()
                    }),
                }],
            }
        );
    }

    // Whole-turn. Defect: a retry with another target replays the first
    // answer, or every round-1 send hashes anew.
    #[test]
    fn s3_adding_scope_to_a_retry_is_another_call() {
        let bare = asking(IntentKind::Steer, None);
        let mut again = sends(mono_a(Some(live("a-chat", true, true))));
        again.prior = vec![MonoMessage {
            payload_hash: Input::Send(bare.clone()).payload_hash(),
            ..row('a', Some("giga"), 0, open(1))
        }];
        // The same call but for scope, so only scope can tell them apart.
        let with_scope = SendInput {
            target: Some(SendTarget::Turn),
            ..bare.clone()
        };
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &with_scope,
                &Input::Send(with_scope.clone()).payload_hash(),
                &again,
                NOW,
            ),
            Decision::Refuse {
                code: Refusal::RequestIdReused,
                hint: REQUEST_ID_REUSED.into()
            }
        );
        // The PR I2 round-1 encoding: no scope key at all.
        let round_1 = json!(["send_message", {
            "message": "Hi", "send_to": "mono-a", "wanting_reply": false,
            "intent": "steer", "reply_to": "m_aaaaaaaaaaaa",
        }]);
        assert_eq!(
            Input::Send(steer("m_aaaaaaaaaaaa")).payload_hash().as_str(),
            format!("{:x}", Sha256::digest(round_1.to_string().as_bytes()))
        );
    }

    #[test]
    fn s5_no_mono_and_no_session_is_unknown_target() {
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "nope", false),
                &hash(),
                &sends(Recipient::Unknown),
                NOW,
            ),
            rejected(RejectCode::UnknownTarget, None)
        );
    }

    #[test]
    fn s6_sending_to_yourself_or_your_own_chat_is_self_send() {
        let me = Recipient::Mono {
            mono_id: mono("giga"),
            role: MonoRole::Giga,
            chat: Some(live("giga-chat", true, true)),
            projects: Vec::new(),
            allowlist: Vec::new(),
        };
        let own_chat = Recipient::Session {
            target: live("giga-chat", true, true),
            own_project: true,
            level: SessionLevel::Lead,
        };
        for (recipient, to) in [
            (
                me,
                Party::Mono {
                    mono_id: mono("giga"),
                },
            ),
            (
                own_chat,
                Party::Session {
                    session_id: session("giga-chat"),
                },
            ),
        ] {
            assert_eq!(
                decide_send(
                    &caller(CallerKind::MonoChat),
                    &new("Hi", "giga", false),
                    &hash(),
                    &sends(recipient),
                    NOW,
                ),
                rejected(RejectCode::SelfSend, Some(to))
            );
        }
        // A habit run sends from its own session; its Mono's chat is still itself.
        let run = Caller {
            session_id: session("giga-run"),
            ..caller(CallerKind::HabitRun)
        };
        assert_eq!(
            decide_send(
                &run,
                &new("Hi", "giga-chat", false),
                &hash(),
                &sends(Recipient::Session {
                    target: live("giga-chat", false, true),
                    own_project: true,
                    level: SessionLevel::Lead,
                }),
                NOW,
            ),
            rejected(
                RejectCode::SelfSend,
                Some(Party::Session {
                    session_id: session("giga-chat")
                })
            )
        );
    }

    #[test]
    fn s7_a_session_outside_your_projects_is_direction_forbidden() {
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "other", false),
                &hash(),
                &sends(Recipient::Session {
                    target: live("other", false, true),
                    own_project: false,
                    level: SessionLevel::Lead,
                }),
                NOW,
            ),
            rejected(
                RejectCode::DirectionForbidden,
                Some(Party::Session {
                    session_id: session("other")
                })
            )
        );
    }

    /// A row giga sent to mono-a that a-chat handles, in `turn` once it got
    /// a slot.
    fn sent(id: char, hop: u32, turn: Option<&str>) -> MonoMessage {
        row(
            id,
            Some("giga"),
            hop,
            Status::Delivered {
                wanting_reply: Wanting,
                intent: if hop == 0 {
                    Intent::New
                } else {
                    Intent::Steer(SteerTarget::Message(
                        MessageId::parse("m_zzzzzzzzzzzz").unwrap(),
                    ))
                },
                routed: Routed {
                    handled_by_session: session("a-chat"),
                    delivery: Delivery::Submit,
                },
                slot: turn.map(|turn| PlainSlot {
                    turn_id: turn.into(),
                    delivered_at: Some(2),
                    window: "main".into(),
                }),
            },
        )
    }

    // PR I2, S9. Defect: a steer whose reply_to named nothing it may refer
    // to goes somewhere anyway, or (PR I2 review r1 M3) its hint teaches a
    // handle, a form a steer can never use.
    #[test]
    fn s9_a_steer_whose_reply_to_resolved_to_no_accepted_row_is_unknown_message() {
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &sends(mono_a(Some(live("a-chat", true, true)))),
                NOW,
            ),
            rejected(RejectCode::UnknownMessage, party_a())
        );
        let hint = RejectCode::UnknownMessage.hint();
        assert!(
            hint.contains("message_id") && !hint.contains("handle") && !hint.contains('#'),
            "{hint}"
        );
    }

    // PR I2, S10. Defect: a steer names a message between two other Monos,
    // or G steers A about A's own reply to G and the steer lands in
    // whatever turn A runs (the sender side of r0).
    #[test]
    fn s10_a_steer_about_a_message_the_caller_did_not_send_the_recipient_is_mismatched() {
        let a_chat = Some(live("a-chat", true, true));
        let between_others = MonoMessage {
            from_mono_id: Some(mono("mono-b")),
            ..sent('a', 0, Some("turn-1"))
        };
        let a_answered_g = MonoMessage {
            from_mono_id: Some(mono("mono-a")),
            from_session_id: session("a-chat"),
            to: Some(Party::Mono {
                mono_id: mono("giga"),
            }),
            ..sent('b', 1, Some("turn-1"))
        };
        let to_a_session = MonoMessage {
            to: Some(Party::Session {
                session_id: session("a-chat"),
            }),
            ..sent('c', 0, Some("turn-1"))
        };
        for message in [between_others, a_answered_g, to_a_session] {
            assert_eq!(
                decide_send(
                    &caller(CallerKind::MonoChat),
                    &steer(message.id.as_str()),
                    &hash(),
                    &steering(mono_a(a_chat.clone()), message.clone(), a_chat.clone()),
                    NOW,
                ),
                rejected(RejectCode::SteerTargetMismatch, party_a()),
                "{message:?}"
            );
        }
    }

    #[test]
    fn s13_a_project_session_cannot_be_asked_for_a_reply() {
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "p", true),
                &hash(),
                &sends(Recipient::Session {
                    target: live("p", false, true),
                    own_project: true,
                    level: SessionLevel::Lead,
                }),
                NOW,
            ),
            rejected(
                RejectCode::ReplyUnsupportedTarget,
                Some(Party::Session {
                    session_id: session("p")
                })
            )
        );
    }

    #[test]
    fn s14_a_habit_run_cannot_wait_for_a_reply() {
        let decide = |kind| {
            decide_send(
                &caller(kind),
                &new("Hi", "mono-a", true),
                &hash(),
                &sends(mono_a(Some(live("a-chat", false, true)))),
                NOW,
            )
        };
        assert_eq!(
            decide(CallerKind::HabitRun),
            rejected(RejectCode::HabitCannotWait, party_a())
        );
        assert!(matches!(
            decide(CallerKind::MonoChat),
            Decision::Accept { .. }
        ));
    }

    // PR I2, S15. Defect: a steer reaches an idle or cleared session, joins
    // a newer turn than the one that runs its target (unrelated work), or
    // reaches the model before the message it changes.
    #[test]
    fn s15_a_steer_whose_work_is_not_running_is_target_gone() {
        let gone_hint = r#"The work that reply_to started is not running now: it finished, it has not started, or its session is idle, cleared or deleted. Send it with intent "new"."#;
        let ended = Target {
            exists: false,
            ..live("a-chat", true, true)
        };
        let newer = Target {
            turn: Some("turn-2".into()),
            ..live("a-chat", true, true)
        };
        let dropped = row(
            'a',
            Some("giga"),
            0,
            Status::Dropped {
                wanting_reply: false,
                intent: Intent::New,
                closed_at: 3,
                notified_at: None,
            },
        );
        for (case, message, session) in [
            (
                "idle",
                sent('a', 0, Some("turn-1")),
                live("a-chat", false, true),
            ),
            ("cleared", sent('a', 0, Some("turn-1")), ended),
            ("newer turn", sent('a', 0, Some("turn-1")), newer),
            ("queued", sent('a', 0, None), live("a-chat", true, true)),
            ("dropped", dropped, live("a-chat", true, true)),
        ] {
            assert_eq!(
                decide_send(
                    &caller(CallerKind::MonoChat),
                    &steer(message.id.as_str()),
                    &hash(),
                    &steering(
                        mono_a(Some(live("a-chat", true, true))),
                        message,
                        Some(session)
                    ),
                    NOW,
                ),
                send_rejected(RejectCode::TargetGone, party_a(), gone_hint),
                "{case}"
            );
        }
    }

    #[test]
    fn s16_a_mono_without_a_chat_has_no_conversation() {
        let gone = Target {
            exists: false,
            ..live("a-chat", false, true)
        };
        for chat in [None, Some(gone)] {
            assert_eq!(
                decide_send(
                    &caller(CallerKind::MonoChat),
                    &new("Hi", "mono-a", false),
                    &hash(),
                    &sends(mono_a(chat)),
                    NOW,
                ),
                rejected(RejectCode::TargetNoConversation, party_a())
            );
        }
    }

    // PR I2, S17. Defect: a steer into a provider that cannot steer is
    // accepted and never arrives, or new work is refused for it.
    #[test]
    fn s17_a_steer_into_a_running_turn_whose_provider_cannot_steer_is_refused() {
        let stiff = live("a-chat", true, false);
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &steering(
                    mono_a(Some(stiff.clone())),
                    sent('a', 0, Some("turn-1")),
                    Some(stiff.clone())
                ),
                NOW,
            ),
            send_rejected(
                RejectCode::CannotSteer,
                party_a(),
                r#"That session's provider cannot steer a running turn. Send it with intent "new"; it waits for the turn to end."#
            )
        );
        assert!(matches!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "mono-a", false),
                &hash(),
                &sends(mono_a(Some(stiff))),
                NOW,
            ),
            Decision::Accept { .. }
        ));
    }

    // S18 (a steer's hop is M.hop + 1), after S15 and S17. Defect: a chain
    // of steers or late answers grows past hop 4, the limit cuts one hop
    // early, or a hop-4 steer that S15 or S17 refuses is told to stop the
    // chain instead.
    #[test]
    fn s18_a_steer_past_hop_4_is_hop_limit() {
        let steer_at = |hop: u32, handling: Target| {
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &steering(
                    mono_a(Some(live("a-chat", true, true))),
                    sent('a', hop, Some("turn-1")),
                    Some(handling),
                ),
                NOW,
            )
        };
        let running = live("a-chat", true, true);
        assert!(
            matches!(
                steer_at(3, running.clone()),
                Decision::Accept { hop: 4, .. }
            ),
            "{:?}",
            steer_at(3, running.clone())
        );
        assert_eq!(steer_at(4, running), hop_limited(party_a()));
        assert_eq!(
            steer_at(4, live("a-chat", false, true)),
            send_rejected(RejectCode::TargetGone, party_a(), TARGET_GONE)
        );
        assert_eq!(
            steer_at(4, live("a-chat", true, false)),
            send_rejected(
                RejectCode::CannotSteer,
                party_a(),
                r#"That session's provider cannot steer a running turn. Send it with intent "new"; it waits for the turn to end."#
            )
        );
        assert_eq!(
            answer_late(
                question("giga-chat", 4, NOW - 1_000, unanswered()),
                live("giga-chat", true, true),
            ),
            hop_limited(Some(Party::Mono {
                mono_id: mono("giga")
            }))
        );
    }

    // S19 (design 6.1 pair count). Defect: one path lets a pair flood past 6
    // sends in 10 minutes, or the hint gives no time to retry after.
    #[test]
    fn s19_a_seventh_send_to_one_recipient_in_10_minutes_is_rate_limited() {
        let a_chat = live("a-chat", true, true);
        let giga_sends = |input: SendInput, reply_to: Option<Referenced>, pair: PairCount| {
            let facts = facts(SendFacts {
                recipient: mono_a(Some(a_chat.clone())),
                reply_to,
                pair,
            });
            decide_send(&caller(CallerKind::MonoChat), &input, &hash(), &facts, NOW)
        };
        let every_path = |count: u32| {
            let pair = PairCount {
                count,
                oldest: Some(NOW - RATE_WINDOW_MS + 42_500),
            };
            let target = Referenced {
                message: sent('a', 0, Some("turn-1")),
                session: Some(a_chat.clone()),
            };
            [
                ("new", giga_sends(new("Hi", "mono-a", false), None, pair)),
                (
                    "steer",
                    giga_sends(steer("m_aaaaaaaaaaaa"), Some(target), pair),
                ),
                (
                    "whole turn",
                    giga_sends(whole(IntentKind::Steer), None, pair),
                ),
            ]
        };
        for (case, decision) in every_path(5) {
            assert!(
                matches!(decision, Decision::Accept { .. }),
                "{case}: {decision:?}"
            );
        }
        let hint =
            "6 messages to that recipient in 10 minutes. Retry after 43 s with a new request id.";
        for (case, decision) in every_path(6) {
            assert_eq!(
                decision,
                send_rejected(RejectCode::RateLimited, party_a(), hint),
                "{case}"
            );
        }
        let one_ms_left = PairCount {
            count: 6,
            oldest: Some(NOW - RATE_WINDOW_MS + 1),
        };
        assert_eq!(
            giga_sends(new("Hi", "mono-a", false), None, one_ms_left),
            send_rejected(
                RejectCode::RateLimited,
                party_a(),
                "6 messages to that recipient in 10 minutes. Retry after 1 s with a new request id."
            )
        );
    }

    // Design 16.5 row order, PR 7 review r1 n5. Defect: S19 runs before
    // S18, so a steer past hop 4 at the rate limit answers rate_limited and
    // its row loses the hop.
    #[test]
    fn s18_runs_before_s19() {
        let a_chat = live("a-chat", true, true);
        let facts = facts(SendFacts {
            recipient: mono_a(Some(a_chat.clone())),
            reply_to: Some(Referenced {
                message: sent('a', 4, Some("turn-1")),
                session: Some(a_chat),
            }),
            pair: PairCount {
                count: 6,
                oldest: Some(NOW - 1_000),
            },
        });
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &facts,
                NOW
            ),
            hop_limited(party_a())
        );
    }

    // PR I2, S20. Defect: new work still steers a running turn, a steer
    // goes anywhere but the session handling its target, or its hop does
    // not grow, so the hop limit cannot bound a chain of steers.
    #[test]
    #[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
    fn s20_new_work_is_queued_and_a_steer_goes_into_the_turn_that_runs_its_target() {
        let a_chat = live("a-chat", true, true);
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "mono-a", true),
                &hash(),
                &sends(mono_a(Some(a_chat.clone()))),
                NOW,
            ),
            Decision::Accept {
                hop: 0,
                rows: vec![Outgoing {
                    to: party_a().unwrap(),
                    route: Route {
                        session: session("a-chat"),
                        via: Via::Queue
                    },
                    intent: Intent::New,
                }],
            }
        );
        // The chat was reset since: the steer follows the target into the
        // session that handles it, not the Mono's current chat.
        let old_chat = live("a-old", true, true);
        let target = MonoMessage {
            status: match sent('a', 2, Some("turn-1")).status {
                Status::Delivered {
                    wanting_reply,
                    intent,
                    slot,
                    ..
                } => Status::Delivered {
                    wanting_reply,
                    intent,
                    slot,
                    routed: Routed {
                        handled_by_session: session("a-old"),
                        delivery: Delivery::Submit,
                    },
                },
                other => other,
            },
            ..sent('a', 2, Some("turn-1"))
        };
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &steering(mono_a(Some(a_chat)), target, Some(old_chat)),
                NOW,
            ),
            Decision::Accept {
                hop: 3,
                rows: vec![Outgoing {
                    to: party_a().unwrap(),
                    route: Route {
                        session: session("a-old"),
                        via: Via::Steer
                    },
                    intent: Intent::Steer(SteerTarget::Message(
                        MessageId::parse("m_aaaaaaaaaaaa").unwrap()
                    )),
                }],
            }
        );
        let project = Target {
            turn: Some("turn-5".into()),
            ..live("p", true, true)
        };
        let to_project = MonoMessage {
            to: Some(Party::Session {
                session_id: session("p"),
            }),
            status: Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: Routed {
                    handled_by_session: session("p"),
                    delivery: Delivery::Submit,
                },
                slot: Some(PlainSlot {
                    turn_id: "turn-5".into(),
                    delivered_at: Some(2),
                    window: "main".into(),
                }),
            },
            ..sent('p', 0, None)
        };
        assert_eq!(
            decide_send(
                &caller(CallerKind::HabitRun),
                &SendInput {
                    send_to: "p".into(),
                    ..steer("m_pppppppppppp")
                },
                &hash(),
                &steering(
                    Recipient::Session {
                        target: project.clone(),
                        own_project: true,
                        level: SessionLevel::Lead,
                    },
                    to_project,
                    Some(project),
                ),
                NOW,
            ),
            Decision::Accept {
                hop: 1,
                rows: vec![Outgoing {
                    to: Party::Session {
                        session_id: session("p")
                    },
                    route: Route {
                        session: session("p"),
                        via: Via::Steer
                    },
                    intent: Intent::Steer(SteerTarget::Message(
                        MessageId::parse("m_pppppppppppp").unwrap()
                    )),
                }],
            }
        );
    }

    #[test]
    fn r2_r3_a_reply_is_deduplicated_like_a_send() {
        let hash = Input::Reply(reply("Done")).payload_hash();
        let prior = vec![MonoMessage {
            payload_hash: hash.clone(),
            ..row('b', Some("mono-a"), 1, open(1))
        }];
        let mut again = replies(vec![handled('a', 1, live("giga-chat", true, true))]);
        again.prior = prior.clone();
        assert_eq!(
            decide_reply(&reply("Done"), &hash, &again),
            Decision::Duplicate {
                prior: prior.clone(),
                rejection: None
            }
        );
        assert_eq!(
            decide_reply(
                &reply("Done twice"),
                &Input::Reply(reply("Done twice")).payload_hash(),
                &again
            ),
            Decision::Refuse {
                code: Refusal::RequestIdReused,
                hint: REQUEST_ID_REUSED.into()
            }
        );
    }

    // Design 3.4 (PR 5). Defect: `to` is left out of the payload hash, so a
    // retry with the same request id that names other handles is answered
    // as a duplicate of the first reply. `"#1"` and `["#1"]` are one call.
    #[test]
    fn r3_a_request_id_reused_with_other_handles_is_refused() {
        let reply_to =
            |to: Value| match parse(Tool::Reply, &json!({"message": "Done", "to": to})).unwrap() {
                Input::Reply(reply) => reply,
                input => panic!("{input:?}"),
            };
        let mut again = replies(vec![handled('a', 1, live("giga-chat", true, true))]);
        again.prior = vec![MonoMessage {
            payload_hash: Input::Reply(reply_to(json!("#1"))).payload_hash(),
            ..row('b', Some("mono-a"), 1, open(1))
        }];
        let decide = |reply: ReplyInput| {
            decide_reply(&reply, &Input::Reply(reply.clone()).payload_hash(), &again)
        };
        assert!(matches!(
            decide(reply_to(json!(["#1"]))),
            Decision::Duplicate { .. }
        ));
        for other in [
            reply_to(json!("#2")),
            reply_to(json!(["#1", "#2"])),
            reply("Done"),
        ] {
            assert!(
                matches!(
                    decide(other.clone()),
                    Decision::Refuse {
                        code: Refusal::RequestIdReused,
                        ..
                    }
                ),
                "{other:?}"
            );
        }
    }

    #[test]
    fn r4_a_reply_over_8000_characters_is_too_large() {
        assert_eq!(
            decide_reply(
                &reply(&"x".repeat(8001)),
                &hash(),
                &replies(vec![handled('a', 1, live("giga-chat", true, true))]),
            ),
            rejected(RejectCode::TooLarge, None)
        );
    }

    #[test]
    fn r5_a_reply_with_nothing_waiting_is_no_open_handle_with_an_empty_list() {
        assert_eq!(
            decide_reply(&reply("Done"), &hash(), &replies(Vec::new())),
            rejected(RejectCode::NoOpenHandle, None)
        );
    }

    #[test]
    fn r6_a_reply_with_two_waiting_is_ambiguous_and_lists_both() {
        let decision = decide_reply(
            &reply("Done"),
            &hash(),
            &replies(vec![
                handled('a', 1, live("giga-chat", true, true)),
                handled('b', 2, live("giga-chat", true, true)),
            ]),
        );
        let Decision::Reject {
            code: RejectCode::AmbiguousReply,
            hint,
            to: None,
            hop: 0,
            open_handles,
        } = decision
        else {
            panic!("not ambiguous_reply: {decision:?}");
        };
        let id = |letter: char| {
            MessageId::parse(&format!("m_{}", letter.to_string().repeat(12))).unwrap()
        };
        assert_eq!(
            open_handles,
            vec![
                OpenHandle {
                    handle: Handle::new(1).unwrap(),
                    message_id: id('a'),
                    from: "Boss".into()
                },
                OpenHandle {
                    handle: Handle::new(2).unwrap(),
                    message_id: id('b'),
                    from: "Boss".into()
                },
            ]
        );
        assert!(
            hint.starts_with("Several handles are open: #1 (from Boss: Review PR 12")
                && hint.contains(", #2 (from Boss: ")
                && hint.ends_with("). Pass to."),
            "{hint}"
        );
    }

    // R9 between R8 and R10. Defect: replies ping-pong past hop 4, a reply
    // at hop 5 to a gone sender is told about the sender first, or a reply to
    // several messages takes its hop from one that is not the deepest.
    #[test]
    fn r9_a_reply_past_hop_4_is_hop_limit() {
        let at_hop = |hop: u32, sender: Target| {
            let handled = Handled {
                message: row('a', Some("giga"), hop, open(1)),
                ..handled('a', 1, sender)
            };
            decide_reply(&reply("Done"), &hash(), &replies(vec![handled]))
        };
        assert_eq!(
            at_hop(3, live("giga-chat", true, true)),
            Decision::Accept {
                hop: 4,
                rows: vec![Outgoing {
                    to: Party::Mono {
                        mono_id: mono("giga")
                    },
                    route: Route {
                        session: session("giga-chat"),
                        via: Via::Steer
                    },
                    intent: Intent::Reply(MessageId::parse("m_aaaaaaaaaaaa").unwrap()),
                }],
            }
        );
        let limit = hop_limited(None);
        assert_eq!(at_hop(4, live("giga-chat", true, true)), limit);
        let gone = Target {
            exists: false,
            ..live("giga-chat", true, true)
        };
        assert_eq!(at_hop(4, gone), limit);
        let both = |shallow: u32, deep: u32| {
            let at = |id, handle, hop| Handled {
                message: row(id, Some("giga"), hop, open(handle)),
                ..handled(id, handle, live("giga-chat", true, true))
            };
            decide_reply(
                &ReplyInput {
                    to: Some(vec![Handle::new(1).unwrap(), Handle::new(2).unwrap()]),
                    ..reply("Done")
                },
                &hash(),
                &replies(vec![at('a', 1, shallow), at('b', 2, deep)]),
            )
        };
        assert!(
            matches!(both(1, 3), Decision::Accept { hop: 4, .. }),
            "{:?}",
            both(1, 3)
        );
        assert_eq!(both(1, 4), limit);
    }

    // R10 and PR 5 review m4. Defect: a reply to a gone sender is accepted,
    // or the refusal does not say which handle to leave out of `to`.
    #[test]
    fn r10_a_reply_whose_sender_session_or_mono_is_gone_is_target_gone() {
        let gone = Target {
            exists: false,
            ..live("giga-chat", true, true)
        };
        let gone_hint = |hint: &str| Decision::Reject {
            code: RejectCode::TargetGone,
            hint: hint.into(),
            to: None,
            hop: 0,
            open_handles: Vec::new(),
        };
        assert_eq!(
            decide_reply(
                &reply("Done"),
                &hash(),
                &replies(vec![handled('a', 1, gone.clone())])
            ),
            gone_hint("The sender of #1 no longer exists (cleared or deleted).")
        );
        let deleted_mono = Handled {
            message: row('a', None, 1, open(1)),
            ..handled('a', 1, live("giga-chat", true, true))
        };
        assert_eq!(
            decide_reply(&reply("Done"), &hash(), &replies(vec![deleted_mono])),
            gone_hint("The sender of #1 no longer exists (cleared or deleted).")
        );
        let both = ReplyInput {
            to: Some(vec![Handle::new(1).unwrap(), Handle::new(2).unwrap()]),
            ..reply("Done")
        };
        assert_eq!(
            decide_reply(
                &both,
                &hash(),
                &replies(vec![
                    handled('a', 1, live("giga-chat", true, true)),
                    handled('b', 2, gone),
                ])
            ),
            gone_hint(
                "The sender of #2 no longer exists (cleared or deleted). Answer the other handles with to."
            )
        );
    }

    #[test]
    fn r11_a_reply_answers_the_one_waiting_message_one_hop_deeper() {
        let decide = |sender| {
            decide_reply(
                &reply("Done"),
                &hash(),
                &replies(vec![handled('a', 1, sender)]),
            )
        };
        let accept = |via| Decision::Accept {
            hop: 2,
            rows: vec![Outgoing {
                to: Party::Mono {
                    mono_id: mono("giga"),
                },
                route: Route {
                    session: session("giga-chat"),
                    via,
                },
                intent: Intent::Reply(MessageId::parse("m_aaaaaaaaaaaa").unwrap()),
            }],
        };
        assert_eq!(decide(live("giga-chat", true, true)), accept(Via::Steer));
        assert_eq!(decide(live("giga-chat", false, true)), accept(Via::Queue));
        assert_eq!(decide(live("giga-chat", true, false)), accept(Via::Queue));
    }

    const LEAD_SCOPE: &str = "A lead acts only on its own workers. Start workers with sessions.start, steer or stop only sessions you started, and report to your Mono with messages.send.";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// A session caller in `/p/alpha` whose app access is only `startedBy`.
    fn session_caller(id: &str) -> Caller {
        Caller {
            speaker: Speaker::Session {
                access: Access::Started,
                cwd: "/p/alpha".into(),
                lead_session_id: None,
            },
            session_id: session(id),
            turn_id: "turn-1".into(),
        }
    }

    fn operator(id: &str) -> Caller {
        Caller {
            speaker: Speaker::Session {
                access: Access::Operator,
                cwd: "/p/alpha".into(),
                lead_session_id: None,
            },
            ..session_caller(id)
        }
    }

    fn worker_of(lead: &str) -> SessionLevel {
        SessionLevel::Worker {
            lead: session(lead),
        }
    }

    /// A lead in `/p/alpha`.
    fn lead_row() -> CallerFacts {
        CallerFacts::Session {
            level: SessionLevel::Lead,
            cwd: "/p/alpha".into(),
        }
    }

    /// `w1`, the worker of `l1`.
    fn worker_row() -> CallerFacts {
        CallerFacts::Session {
            level: worker_of("l1"),
            cwd: "/p/alpha".into(),
        }
    }

    /// The Mono `mono-a` (projects `/p/alpha`) from its chat `a-chat`.
    fn mono_a_caller() -> Caller {
        Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::MonoChat,
                mono_id: mono("mono-a"),
            },
            session_id: session("a-chat"),
            turn_id: "turn-1".into(),
        }
    }

    fn mono_a_row() -> CallerFacts {
        CallerFacts::Mono(CallerMono {
            chat: Some(session("a-chat")),
            role: MonoRole::Mono,
            projects: strings(&["/p/alpha"]),
            allowlist: Vec::new(),
        })
    }

    /// A steerable session in `/p/alpha` that runs `turn-1`.
    fn in_alpha(id: &str, level: SessionLevel) -> Recipient {
        Recipient::Session {
            target: live(id, true, true),
            own_project: true,
            level,
        }
    }

    /// A Mono of `role` in `projects`, whose chat `<id>-chat` runs `turn-1`.
    fn mono_in(id: &str, role: MonoRole, projects: &[&str]) -> Recipient {
        Recipient::Mono {
            mono_id: mono(id),
            role,
            chat: Some(live(&format!("{id}-chat"), true, true)),
            projects: strings(projects),
            allowlist: Vec::new(),
        }
    }

    fn about(recipient: Recipient) -> SendFacts {
        SendFacts {
            recipient,
            reply_to: None,
            pair: PairCount::default(),
        }
    }

    fn decide_as(who: &Caller, row: CallerFacts, input: &SendInput, about: SendFacts) -> Decision {
        let facts = Facts {
            caller: Some(row),
            prior: Vec::new(),
            about,
        };
        decide_send(who, input, &hash(), &facts, NOW)
    }

    /// `input` sent to `send_to`.
    fn at(send_to: &str, input: SendInput) -> SendInput {
        SendInput {
            send_to: send_to.into(),
            ..input
        }
    }

    /// New work accepted into the queue of `session_id`.
    fn queued(to: Option<Party>, session_id: &str) -> Decision {
        Decision::Accept {
            hop: 0,
            rows: vec![Outgoing {
                to: to.unwrap(),
                route: Route {
                    session: session(session_id),
                    via: Via::Queue,
                },
                intent: Intent::New,
            }],
        }
    }

    fn worker_scope(lead: &str) -> String {
        format!(
            r#"A worker can message only its lead. Message your lead ({lead}): messages.send with send_to "{lead}"."#
        )
    }

    fn via_lead(lead: &str) -> String {
        format!(
            "That session is a worker of lead {lead}. Message the lead ({lead}); only its lead steers its workers."
        )
    }

    // L1 before S5. Defect: a worker reaches a Mono or another session, or a
    // made-up target gets unknown_target, which tells the worker nothing it
    // can act on.
    #[test]
    fn l1_a_worker_may_message_only_its_lead_whatever_it_names() {
        for (send_to, recipient) in [
            ("nope", Recipient::Unknown),
            ("mono-a", mono_in("mono-a", MonoRole::Mono, &["/p/alpha"])),
            ("l2", in_alpha("l2", SessionLevel::Lead)),
        ] {
            let to = recipient.party();
            assert_eq!(
                decide_as(
                    &session_caller("w1"),
                    worker_row(),
                    &new("Done", send_to, false),
                    about(recipient)
                ),
                send_rejected(RejectCode::WorkerScope, to, &worker_scope("l1")),
                "{send_to}"
            );
        }
    }

    // L2 before S5 (design 13.2: an archived lead's session has ended).
    // Defect: a worker gets unknown_target for its own lead and no word that
    // the lead is gone.
    #[test]
    fn l2_a_worker_whose_lead_ended_gets_target_gone_naming_the_lead() {
        assert_eq!(
            decide_as(
                &session_caller("w1"),
                worker_row(),
                &new("Done", "l1", false),
                about(Recipient::Unknown)
            ),
            send_rejected(
                RejectCode::TargetGone,
                None,
                "Your lead (l1) is archived or no longer exists. It cannot get messages until the user restores it. Use a new request id when you retry."
            )
        );
    }

    // S7 before L4. Defect: a Mono learns the lead of a worker outside its
    // projects from the steer_via_lead hint.
    #[test]
    fn s7_runs_before_l4_so_a_worker_outside_the_callers_projects_keeps_its_lead_hidden() {
        let outside = Recipient::Session {
            target: live("w9", true, true),
            own_project: false,
            level: worker_of("l9"),
        };
        let to = outside.party();
        assert_eq!(
            decide_as(
                &mono_a_caller(),
                mono_a_row(),
                &new("Hi", "w9", false),
                about(outside)
            ),
            send_rejected(
                RejectCode::DirectionForbidden,
                to,
                "That session is not in one of your projects."
            )
        );
    }

    // L3. Defect: a lead messages a Mono that holds none of its folders, or
    // cannot report to a Mono or Giga that does.
    #[test]
    fn l3_a_lead_may_message_only_a_mono_that_holds_its_folder() {
        let report = |send_to: &str, recipient: Recipient| {
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &new("Done", send_to, false),
                about(recipient),
            )
        };
        let other = mono_in("mono-c", MonoRole::Mono, &["/p/gamma"]);
        assert_eq!(
            report("mono-c", other.clone()),
            send_rejected(
                RejectCode::DirectionForbidden,
                other.party(),
                "That Mono has none of your project folders."
            )
        );
        for (id, role) in [("mono-a", MonoRole::Mono), ("giga", MonoRole::Giga)] {
            let mine = mono_in(id, role, &["/p/gamma", "/p/alpha"]);
            assert_eq!(
                report(id, mine.clone()),
                queued(mine.party(), &format!("{id}-chat")),
                "{id}"
            );
        }
    }

    // L4, for every intent and sender (decisions 472, 503). Defect: a Mono, a
    // habit run, a Giga or another lead messages a worker around its lead, a
    // steer or a whole-turn steer slips past L4 (W2 answers
    // steer_scope_not_allowed), or the lead itself cannot.
    #[test]
    fn l4_only_its_lead_may_message_a_worker() {
        let w1 = in_alpha("w1", worker_of("l1"));
        let rejected = send_rejected(RejectCode::SteerViaLead, w1.party(), &via_lead("l1"));
        for (case, who, row) in [
            ("Mono", mono_a_caller(), mono_a_row()),
            (
                "Giga",
                caller(CallerKind::MonoChat),
                CallerFacts::Mono(giga_row()),
            ),
            ("another lead", session_caller("l2"), lead_row()),
        ] {
            assert_eq!(
                decide_as(&who, row, &new("Hi", "w1", false), about(w1.clone())),
                rejected,
                "{case}"
            );
        }
        assert_eq!(
            decide_as(
                &caller(CallerKind::MonoChat),
                CallerFacts::Mono(giga_row()),
                &at("w1", whole(IntentKind::Steer)),
                about(w1.clone())
            ),
            rejected,
            "Giga whole-turn steer"
        );
        let to_w1 = MonoMessage {
            from_session_id: session("a-chat"),
            to: Some(Party::Session {
                session_id: session("w1"),
            }),
            addressed: "w1".into(),
            status: Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: Routed {
                    handled_by_session: session("w1"),
                    delivery: Delivery::Queue,
                },
                slot: Some(PlainSlot {
                    turn_id: "turn-1".into(),
                    delivered_at: Some(2),
                    window: "main".into(),
                }),
            },
            ..row('s', Some("mono-a"), 0, open(1))
        };
        assert_eq!(
            decide_as(
                &mono_a_caller(),
                mono_a_row(),
                &at("w1", steer("m_ssssssssssss")),
                SendFacts {
                    reply_to: Some(Referenced {
                        message: to_w1,
                        session: Some(live("w1", true, true)),
                    }),
                    ..about(w1.clone())
                }
            ),
            rejected,
            "Mono steer with reply_to"
        );
        let habit_run = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("mono-a"),
            },
            session_id: session("a-run"),
            ..mono_a_caller()
        };
        assert_eq!(
            decide_as(
                &habit_run,
                mono_a_row(),
                &new("Hi", "w1", false),
                about(w1.clone())
            ),
            rejected,
            "habit run"
        );
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &new("Next", "w1", false),
                about(w1.clone())
            ),
            queued(w1.party(), "w1")
        );
    }

    // L7 after L4. Defect: a lead messages a sibling lead or the user's own
    // session in its folder, or another lead's worker gets lead_scope and the
    // lead never learns whose worker it is.
    #[test]
    fn l7_a_lead_may_message_no_session_but_its_own_workers() {
        let send = |send_to: &str, recipient: Recipient| {
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &new("Hi", send_to, false),
                about(recipient),
            )
        };
        for id in ["l2", "user"] {
            let lead = in_alpha(id, SessionLevel::Lead);
            assert_eq!(
                send(id, lead.clone()),
                send_rejected(RejectCode::LeadScope, lead.party(), LEAD_SCOPE),
                "{id}"
            );
        }
        let w2 = in_alpha("w2", worker_of("l2"));
        assert_eq!(
            send("w2", w2.clone()),
            send_rejected(RejectCode::SteerViaLead, w2.party(), &via_lead("l2"))
        );
    }

    // L5 before S9 and S13. Defect: a session caller waits for an answer
    // whose notice can never reach it, gets reply_unsupported_target, a code
    // about the recipient, for a limit of its own, or a steer that names an
    // unknown message gets unknown_message first.
    #[test]
    fn l5_a_session_caller_cannot_wait_for_an_answer() {
        let hint = "A lead or worker session cannot wait for an answer. Send with wanting_reply false. The recipient answers with a new message if it needs to.";
        for (case, who, caller_row, send_to, recipient) in [
            (
                "lead to its worker",
                "l1",
                lead_row(),
                "w1",
                in_alpha("w1", worker_of("l1")),
            ),
            (
                "lead to its Mono",
                "l1",
                lead_row(),
                "mono-a",
                mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]),
            ),
            (
                "worker to its lead",
                "w1",
                worker_row(),
                "l1",
                in_alpha("l1", SessionLevel::Lead),
            ),
        ] {
            let to = recipient.party();
            assert_eq!(
                decide_as(
                    &session_caller(who),
                    caller_row,
                    &new("Hi", send_to, true),
                    about(recipient)
                ),
                send_rejected(RejectCode::SessionCannotWait, to, hint),
                "{case}"
            );
        }
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &SendInput {
                    wanting_reply: true,
                    ..at("w1", steer("m_uuuuuuuuuuuu"))
                },
                about(in_alpha("w1", worker_of("l1")))
            ),
            send_rejected(
                RejectCode::SessionCannotWait,
                in_alpha("w1", worker_of("l1")).party(),
                hint
            ),
            "lead's steer with an unknown reply_to"
        );
    }

    // L6 before S9. Defect: a worker steers its lead's work or a lead its
    // Mono's, though a session holds no handle to answer with, or new work
    // upward is refused.
    #[test]
    fn l6_a_session_caller_sends_upward_only_new_work() {
        let hint = r#"Messages to your lead or to a Mono are queued. Send with intent "new"."#;
        for (who, caller_row, send_to, recipient, chat) in [
            (
                "w1",
                worker_row(),
                "l1",
                in_alpha("l1", SessionLevel::Lead),
                "l1",
            ),
            (
                "l1",
                lead_row(),
                "mono-a",
                mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]),
                "mono-a-chat",
            ),
        ] {
            let send = |input: SendInput| {
                decide_as(
                    &session_caller(who),
                    caller_row.clone(),
                    &at(send_to, input),
                    about(recipient.clone()),
                )
            };
            assert_eq!(
                send(steer("m_aaaaaaaaaaaa")),
                send_rejected(RejectCode::UpwardSteerForbidden, recipient.party(), hint),
                "{who}"
            );
            assert_eq!(
                send(new("Done", send_to, false)),
                queued(recipient.party(), chat),
                "{who}"
            );
        }
    }

    // W2 before L6 (design 16.12): no session caller steers a whole turn.
    // Defect: a worker that steers its own lead's whole turn gets
    // upward_steer_forbidden from L6 instead of steer_scope_not_allowed.
    #[test]
    fn w2_a_worker_steering_its_leads_whole_turn_is_steer_scope_not_allowed() {
        let l1 = in_alpha("l1", SessionLevel::Lead);
        assert_eq!(
            decide_as(
                &session_caller("w1"),
                worker_row(),
                &at("l1", whole(IntentKind::Steer)),
                about(l1.clone())
            ),
            send_rejected(
                RejectCode::SteerScopeNotAllowed,
                l1.party(),
                RejectCode::SteerScopeNotAllowed.hint()
            )
        );
    }

    // Design 16.12.9.2. Defect: a session caller steers a whole turn, a Mono
    // cannot steer its own lead's, or a worker's whole turn is steered around
    // its lead.
    #[test]
    fn w2_whole_turn_steers_between_levels() {
        let l1 = Recipient::Session {
            target: Target {
                turn: Some("turn-7".into()),
                ..live("l1", true, true)
            },
            own_project: true,
            level: SessionLevel::Lead,
        };
        let w1 = in_alpha("w1", worker_of("l1"));
        let a = mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]);
        let swt = |who: &Caller, caller_row: CallerFacts, send_to: &str, recipient: &Recipient| {
            decide_as(
                who,
                caller_row,
                &at(send_to, whole(IntentKind::Steer)),
                about(recipient.clone()),
            )
        };
        assert_eq!(
            swt(&mono_a_caller(), mono_a_row(), "l1", &l1),
            Decision::Accept {
                hop: 0,
                rows: vec![Outgoing {
                    to: Party::Session {
                        session_id: session("l1")
                    },
                    route: Route {
                        session: session("l1"),
                        via: Via::Steer
                    },
                    intent: Intent::Steer(SteerTarget::Turn {
                        turn_id: "turn-7".into()
                    }),
                }],
            }
        );
        for (case, who, caller_row, send_to, recipient, code, hint) in [
            (
                "A to W1",
                mono_a_caller(),
                mono_a_row(),
                "w1",
                &w1,
                RejectCode::SteerViaLead,
                via_lead("l1"),
            ),
            (
                "L to W1",
                session_caller("l1"),
                lead_row(),
                "w1",
                &w1,
                RejectCode::SteerScopeNotAllowed,
                SCOPE.to_string(),
            ),
            (
                "W1 to L",
                session_caller("w1"),
                worker_row(),
                "l1",
                &l1,
                RejectCode::SteerScopeNotAllowed,
                SCOPE.to_string(),
            ),
            (
                "W1 to A",
                session_caller("w1"),
                worker_row(),
                "mono-a",
                &a,
                RejectCode::WorkerScope,
                worker_scope("l1"),
            ),
            (
                "L to A",
                session_caller("l1"),
                lead_row(),
                "mono-a",
                &a,
                RejectCode::SteerScopeNotAllowed,
                SCOPE.to_string(),
            ),
        ] {
            assert_eq!(
                swt(&who, caller_row, send_to, recipient),
                send_rejected(code, recipient.party(), &hint),
                "{case}"
            );
        }
    }

    // Design 16.12.2.2. Defect: the rule lets a worker's whole turn be
    // steered and leans on L4 running first.
    #[test]
    fn may_steer_turn_is_false_for_a_worker_even_when_called_directly() {
        let w1 = in_alpha("w1", worker_of("l1"));
        for level in [
            Level::Giga,
            Level::Mono,
            Level::Lead,
            Level::Worker {
                lead: session("l1"),
            },
        ] {
            assert!(
                !may_steer_turn(&level, &w1),
                "{level:?} may steer a worker's whole turn"
            );
        }
    }

    /// mono-a's late answer to `m`, accepted into turn `turn` of the Giga's
    /// chat and pinned to it.
    fn answered_late(m: &MonoMessage, turn: &str) -> Decision {
        Decision::Accept {
            hop: m.hop + 1,
            rows: vec![Outgoing {
                to: Party::Mono {
                    mono_id: mono("giga"),
                },
                route: Route {
                    session: session("giga-chat"),
                    via: Via::Steer,
                },
                intent: Intent::Steer(SteerTarget::Pinned {
                    message: m.id.clone(),
                    turn_id: turn.into(),
                }),
            }],
        }
    }

    /// The Giga's question to mono-a from the Giga's session `from`, asked at
    /// `created_at`.
    fn question(from: &str, hop: u32, created_at: i64, status: Status) -> MonoMessage {
        MonoMessage {
            from_session_id: session(from),
            created_at,
            ..row('q', Some("giga"), hop, status)
        }
    }

    /// A question that a-chat held in `turn-1`, and that turn ended.
    fn unanswered() -> Status {
        Status::Unanswered {
            wanting_reply: Wanting,
            intent: Intent::New,
            routed: Routed {
                handled_by_session: session("a-chat"),
                delivery: Delivery::Submit,
            },
            slot: DeliveredSlot {
                turn_id: "turn-1".into(),
                handle: Handle::new(1).unwrap(),
                delivered_at: 2,
                window: "main".into(),
            },
            closed_at: 3,
            notified_at: None,
        }
    }

    /// mono-a steers the Giga about `m`, with the Giga's chat as `chat`.
    fn answer_late(m: MonoMessage, chat: Target) -> Decision {
        let input = at("giga", steer(m.id.as_str()));
        decide_as(
            &mono_a_caller(),
            mono_a_row(),
            &input,
            SendFacts {
                recipient: mono_with_role("giga", MonoRole::Giga, chat.clone()),
                reply_to: Some(Referenced {
                    message: m,
                    session: Some(chat),
                }),
                pair: PairCount::default(),
            },
        )
    }

    // S12 before S10 (intents review r1). Defect: a Mono steers a Giga about
    // anything but its late answer to the Giga's question, answers an open
    // handle by steer (the handle stays open), or is refused with
    // steer_target_mismatch, which never names reply.
    #[test]
    fn s12_a_monos_steer_to_a_giga_needs_the_gigas_unanswered_question_of_the_last_24_h() {
        let own = MonoMessage {
            from_session_id: session("a-chat"),
            to: Some(Party::Mono {
                mono_id: mono("giga"),
            }),
            status: Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: Routed {
                    handled_by_session: session("giga-chat"),
                    delivery: Delivery::Submit,
                },
                slot: Some(PlainSlot {
                    turn_id: "turn-1".into(),
                    delivered_at: Some(2),
                    window: "main".into(),
                }),
            },
            ..row('m', Some("mono-a"), 0, open(1))
        };
        let wants_no_reply = sent('q', 0, Some("turn-1")).status;
        for (case, m) in [
            (
                "an open question",
                question("giga-chat", 0, NOW - 1_000, open(1)),
            ),
            (
                "a question a day and 1 ms old",
                question("giga-chat", 0, NOW - DAY_MS - 1, unanswered()),
            ),
            (
                "a message that wants no reply",
                question("giga-chat", 0, NOW - 1_000, wants_no_reply),
            ),
            ("its own message to the Giga", own),
            (
                "the Giga's question to another Mono",
                MonoMessage {
                    to: Some(Party::Mono {
                        mono_id: mono("mono-b"),
                    }),
                    ..question("giga-chat", 0, NOW - 1_000, unanswered())
                },
            ),
            (
                "another Giga's question",
                MonoMessage {
                    from_mono_id: Some(mono("giga-2")),
                    ..question("giga-chat", 0, NOW - 1_000, unanswered())
                },
            ),
        ] {
            assert_eq!(
                answer_late(m, live("giga-chat", true, true)),
                send_rejected(
                    RejectCode::UpwardSteerForbidden,
                    Some(Party::Mono {
                        mono_id: mono("giga")
                    }),
                    UPWARD
                ),
                "{case}"
            );
        }
    }

    // S12 binds a Mono caller only (review r2 n2). Defect: a Giga's steer to
    // another Giga about a message not between them gets S12's
    // upward_steer_forbidden, whose hint names a late answer, instead of
    // S10's steer_target_mismatch.
    #[test]
    fn s12_leaves_a_gigas_steer_to_another_giga_to_s10() {
        let a_chat = Some(live("a-chat", true, true));
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &steer("m_aaaaaaaaaaaa"),
                &hash(),
                &steering(
                    Recipient::Mono {
                        mono_id: mono("giga-2"),
                        role: MonoRole::Giga,
                        chat: Some(live("g2-chat", true, true)),
                        projects: Vec::new(),
                        allowlist: vec![mono("giga")],
                    },
                    sent('a', 0, Some("turn-1")),
                    a_chat,
                ),
                NOW,
            ),
            rejected(
                RejectCode::SteerTargetMismatch,
                Some(Party::Mono {
                    mono_id: mono("giga-2")
                })
            )
        );
    }

    // Rule (b) (design 16.6, intents review r1). Defect: a Mono's late answer
    // never reaches the Giga, lands in its idle chat as new work, reaches a
    // finished habit run, or closes the question as answered (only reply
    // closes a message).
    #[test]
    fn rule_b_a_late_answer_steers_the_gigas_running_chat_and_leaves_the_question_open_to_reply() {
        let giga = Some(Party::Mono {
            mono_id: mono("giga"),
        });
        let running = Target {
            turn: Some("turn-4".into()),
            ..live("giga-chat", true, true)
        };
        let a_day_old = question("giga-chat", 2, NOW - DAY_MS, unanswered());
        assert_eq!(
            answer_late(a_day_old.clone(), running.clone()),
            answered_late(&a_day_old, "turn-4")
        );
        assert_eq!(
            answer_late(a_day_old, live("giga-chat", false, true)),
            send_rejected(RejectCode::TargetGone, giga.clone(), TARGET_GONE)
        );
        assert_eq!(
            answer_late(question("giga-run", 2, NOW - DAY_MS, unanswered()), running),
            send_rejected(RejectCode::TargetGone, giga, TARGET_GONE)
        );
    }

    // Intents review r1 dropped the sender side of target_session, so a row
    // whose Mono was deleted is no row of a session caller's. Defect: a lead
    // steers its worker about a message a deleted Mono sent it.
    #[test]
    fn a_session_callers_steer_naming_a_row_from_a_deleted_mono_is_steer_target_mismatch() {
        let orphan = MonoMessage {
            from_session_id: session("a-chat"),
            to: Some(Party::Session {
                session_id: session("w1"),
            }),
            status: Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: Routed {
                    handled_by_session: session("w1"),
                    delivery: Delivery::Submit,
                },
                slot: Some(PlainSlot {
                    turn_id: "turn-1".into(),
                    delivered_at: Some(2),
                    window: "main".into(),
                }),
            },
            ..row('o', None, 0, open(1))
        };
        let w1 = in_alpha("w1", worker_of("l1"));
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &at("w1", steer("m_oooooooooooo")),
                SendFacts {
                    recipient: w1.clone(),
                    reply_to: Some(Referenced {
                        message: orphan,
                        session: Some(live("w1", true, true)),
                    }),
                    pair: PairCount::default(),
                },
            ),
            send_rejected(
                RejectCode::SteerTargetMismatch,
                w1.party(),
                "reply_to is not a message between you and send_to."
            )
        );
    }

    // Design 13.4. Defect: the prelude refuses every caller without a Mono as
    // not_a_mono, so no lead or worker can send; a session holds no handle,
    // so its reply must find none.
    #[test]
    fn a_session_caller_passes_the_prelude_and_its_reply_finds_no_open_handle() {
        let l1 = in_alpha("l1", SessionLevel::Lead);
        assert_eq!(
            decide_as(
                &session_caller("w1"),
                worker_row(),
                &new("Done", "l1", false),
                about(l1.clone())
            ),
            queued(l1.party(), "l1")
        );
        let facts = Facts {
            caller: Some(lead_row()),
            prior: Vec::new(),
            about: ReplyFacts {
                handled: Vec::new(),
                then: Vec::new(),
            },
        };
        assert_eq!(
            decide_reply(&reply("Done"), &hash(), &facts),
            send_rejected(
                RejectCode::NoOpenHandle,
                None,
                "No message in this turn is waiting for your reply. Use send_message."
            )
        );
    }

    // Design 3.4: a duplicate of a rejected call answers its rejection again.
    // Defect: the replay answers the code's bare hint, so a retry loses the
    // lead id or the text for the caller's kind that the first call gave.
    #[test]
    fn a_replayed_send_rejection_gets_the_hint_the_first_call_got() {
        let gamma = || mono_in("mono-c", MonoRole::Mono, &["/p/gamma"]);
        for (case, who, caller_row, input, recipient) in [
            (
                "L1",
                session_caller("w1"),
                worker_row(),
                new("Hi", "nope", false),
                Recipient::Unknown,
            ),
            (
                "L2",
                session_caller("w1"),
                worker_row(),
                new("Hi", "l1", false),
                Recipient::Unknown,
            ),
            (
                "L3",
                session_caller("l1"),
                lead_row(),
                new("Hi", "mono-c", false),
                gamma(),
            ),
            (
                "L4",
                mono_a_caller(),
                mono_a_row(),
                new("Hi", "w1", false),
                in_alpha("w1", worker_of("l1")),
            ),
            (
                "L6",
                session_caller("l1"),
                lead_row(),
                at("mono-a", steer("m_aaaaaaaaaaaa")),
                mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]),
            ),
        ] {
            let Decision::Reject { code, hint, .. } =
                decide_as(&who, caller_row.clone(), &input, about(recipient.clone()))
            else {
                panic!("{case}: not rejected");
            };
            let prior = vec![MonoMessage {
                status: Status::Rejected {
                    gate_result: code,
                    wanting_reply: false,
                    intent: input.intent,
                    reply_to: None,
                    scope: None,
                },
                ..row('r', None, 0, open(1))
            }];
            let facts = Facts {
                caller: Some(caller_row),
                prior: prior.clone(),
                about: about(recipient),
            };
            assert_eq!(
                decide_send(&who, &input, &hash(), &facts, NOW),
                Decision::Duplicate {
                    prior,
                    rejection: Some(Rejection {
                        code,
                        hint,
                        open_handles: Vec::new(),
                    }),
                },
                "{case}"
            );
        }
    }

    // L3 (design 6.3), PR 7 review r1 m9 and n5. Defect: a lead reports to
    // a Giga whose projects do not hold its folder, or L3 compares folders
    // by prefix, so a lead in a subfolder reaches a Mono of the parent.
    #[test]
    fn l3_refuses_a_giga_outside_the_leads_folder_and_a_lead_in_a_subfolder() {
        let giga = mono_in("giga", MonoRole::Giga, &["/p/gamma"]);
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &new("Done", "giga", false),
                about(giga.clone())
            ),
            send_rejected(
                RejectCode::DirectionForbidden,
                giga.party(),
                "That Mono has none of your project folders."
            ),
            "giga outside the folder"
        );
        let in_sub = CallerFacts::Session {
            level: SessionLevel::Lead,
            cwd: "/p/alpha/sub".into(),
        };
        let a = mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]);
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                in_sub,
                &new("Done", "mono-a", false),
                about(a.clone())
            ),
            send_rejected(
                RejectCode::DirectionForbidden,
                a.party(),
                "That Mono has none of your project folders."
            ),
            "subfolder"
        );
    }

    const PEER_RULE: &str = "You and that Mono share no project and neither allowlists the other.";

    // S8, the peer rule (design 6.2). Defect: two Monos of one role with
    // nothing in common talk, or a shared project or an allowlist on either
    // side does not let them.
    #[test]
    fn s8_monos_of_one_role_talk_through_a_shared_project_or_an_allowlist() {
        let b_caller = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::MonoChat,
                mono_id: mono("mono-b"),
            },
            session_id: session("b-chat"),
            turn_id: "turn-1".into(),
        };
        let b = |projects: &[&str], allowlist: &[&str]| {
            CallerFacts::Mono(CallerMono {
                chat: Some(session("b-chat")),
                role: MonoRole::Mono,
                projects: strings(projects),
                allowlist: allowlist.iter().map(|id| mono(id)).collect(),
            })
        };
        let a = |allowlist: &[&str]| Recipient::Mono {
            mono_id: mono("mono-a"),
            role: MonoRole::Mono,
            chat: Some(live("a-chat", true, true)),
            projects: strings(&["/p/alpha"]),
            allowlist: allowlist.iter().map(|id| mono(id)).collect(),
        };
        let send = |caller_row: CallerFacts, recipient: Recipient| {
            decide_as(
                &b_caller,
                caller_row,
                &new("Hi", "mono-a", false),
                about(recipient),
            )
        };
        assert_eq!(
            send(b(&["/p/beta"], &[]), a(&[])),
            send_rejected(RejectCode::DirectionForbidden, party_a(), PEER_RULE)
        );
        for (case, caller_row, recipient) in [
            ("shared project", b(&["/p/beta", "/p/alpha"], &[]), a(&[])),
            (
                "mono-b allowlists mono-a",
                b(&["/p/beta"], &["mono-a"]),
                a(&[]),
            ),
            (
                "mono-a allowlists mono-b",
                b(&["/p/beta"], &[]),
                a(&["mono-b"]),
            ),
        ] {
            assert_eq!(
                send(caller_row, recipient),
                queued(party_a(), "a-chat"),
                "{case}"
            );
        }
        let giga_in = |projects: &[&str]| {
            CallerFacts::Mono(CallerMono {
                projects: strings(projects),
                ..giga_row()
            })
        };
        let to_g2 = |caller_row: CallerFacts, projects: &[&str]| {
            decide_as(
                &caller(CallerKind::MonoChat),
                caller_row,
                &new("Hi", "giga-2", false),
                about(mono_in("giga-2", MonoRole::Giga, projects)),
            )
        };
        let g2 = Some(Party::Mono {
            mono_id: mono("giga-2"),
        });
        assert_eq!(
            to_g2(giga_in(&["/m"]), &["/n"]),
            send_rejected(RejectCode::DirectionForbidden, g2.clone(), PEER_RULE)
        );
        assert_eq!(to_g2(giga_in(&["/m"]), &["/m"]), queued(g2, "giga-2-chat"));
    }

    // Design 6.2: the peer rule binds one role only. Defect: a Giga cannot
    // message a Mono it shares no project with, or a Mono cannot queue new
    // work for a Giga (rule (c)).
    #[test]
    fn s8_a_giga_messages_any_mono_and_any_mono_queues_work_for_a_giga() {
        let a = mono_in("mono-a", MonoRole::Mono, &["/p/alpha"]);
        assert_eq!(
            decide_as(
                &caller(CallerKind::MonoChat),
                CallerFacts::Mono(giga_row()),
                &new("Hi", "mono-a", false),
                about(a.clone())
            ),
            queued(a.party(), "mono-a-chat")
        );
        let giga = mono_in("giga", MonoRole::Giga, &["/m"]);
        assert_eq!(
            decide_as(
                &mono_a_caller(),
                mono_a_row(),
                &new("Done", "giga", false),
                about(giga.clone())
            ),
            queued(giga.party(), "giga-chat")
        );
    }

    // Design 6.1. Defect: Rust keys a project folder unlike TS `pathKey`, so
    // S8 and L3 disagree with the projects TS shows.
    #[test]
    fn path_key_matches_ts_path_key_on_the_shared_fixture() {
        let fixture: Vec<(String, String)> =
            serde_json::from_str(include_str!("../../src/shared/lib/pathKey.fixtures.json"))
                .unwrap();
        for (path, key) in fixture {
            assert_eq!(path_key(&path), key, "{path:?}");
        }
    }

    const ALLOWED: ActionDecision = ActionDecision::Allow {
        lead_session_id: None,
    };

    fn act(
        who: &Caller,
        action: &str,
        caller_level: Option<SessionLevel>,
        target_level: Option<SessionLevel>,
    ) -> ActionDecision {
        decide_action(
            who,
            action,
            &action_hash(action, &json!({})),
            &ActionFacts {
                caller_level,
                caller_role: None,
                target_level,
                prior: None,
                prior_start_ended: false,
            },
        )
    }

    fn act_rejected(code: RejectCode, hint: &str, tool: Tool) -> ActionDecision {
        ActionDecision::Reject {
            code,
            hint: hint.into(),
            tool,
        }
    }

    // A8 (design 6.6). Defect: an app-started lead gets every /operator
    // action (all notes, a sibling's delete, steering or stopping a sibling
    // lead, a user's session or another lead's worker), or loses a power the
    // addendum gives it (stop its worker, make a worktree); or an /operator
    // lead is narrowed too.
    #[test]
    fn a8_a_lead_started_by_the_app_acts_only_on_its_own_workers() {
        let started = session_caller("l1");
        let user_lead = operator("l1");
        let lead_scope = act_rejected(RejectCode::LeadScope, LEAD_SCOPE, Tool::AppAction);
        for (case, who, action, target, expected) in [
            (
                "notes.list",
                &started,
                "notes.list",
                None,
                lead_scope.clone(),
            ),
            (
                "delete a sibling",
                &started,
                "sessions.delete",
                Some(SessionLevel::Lead),
                lead_scope.clone(),
            ),
            (
                "send a sibling lead",
                &started,
                "sessions.send",
                Some(SessionLevel::Lead),
                lead_scope.clone(),
            ),
            (
                "draft into a user session",
                &started,
                "sessions.draft",
                None,
                lead_scope.clone(),
            ),
            (
                "stop a sibling lead",
                &started,
                "sessions.stop",
                Some(SessionLevel::Lead),
                lead_scope.clone(),
            ),
            (
                "stop another lead's worker",
                &started,
                "sessions.stop",
                Some(worker_of("l2")),
                lead_scope.clone(),
            ),
            (
                "stop its worker",
                &started,
                "sessions.stop",
                Some(worker_of("l1")),
                ALLOWED,
            ),
            (
                "create a worktree",
                &started,
                "worktrees.create",
                None,
                ALLOWED,
            ),
            (
                "operator notes.list",
                &user_lead,
                "notes.list",
                None,
                ALLOWED,
            ),
        ] {
            assert_eq!(
                act(who, action, Some(SessionLevel::Lead), target),
                expected,
                "{case}"
            );
        }
    }

    // A8 (design 6.6). Defect: a started lead reads or archives a sibling
    // lead, or cannot list models to pick a worker's.
    #[test]
    fn a8_a_started_lead_reads_and_archives_only_its_workers_and_lists_models() {
        let started = session_caller("l1");
        let lead_scope = act_rejected(RejectCode::LeadScope, LEAD_SCOPE, Tool::AppAction);
        for action in ["sessions.read", "sessions.archive"] {
            assert_eq!(
                act(
                    &started,
                    action,
                    Some(SessionLevel::Lead),
                    Some(SessionLevel::Lead)
                ),
                lead_scope,
                "{action} of a sibling"
            );
            assert_eq!(
                act(
                    &started,
                    action,
                    Some(SessionLevel::Lead),
                    Some(worker_of("l1"))
                ),
                ALLOWED,
                "{action} of its worker"
            );
        }
        assert_eq!(
            act(&started, "models.list", Some(SessionLevel::Lead), None),
            ALLOWED,
            "models.list"
        );
    }

    const WORKER_CANNOT_SPAWN_L1: &str = r#"Only lead sessions can start sessions. Ask your lead (l1): messages.send with send_to "l1"."#;

    // A3. Defect: a worker starts a session, so the tree grows past two
    // levels, or is refused with a code that does not say who may start one.
    #[test]
    fn a3_a_worker_cannot_start_a_session() {
        assert_eq!(
            act(
                &session_caller("w1"),
                "sessions.start",
                Some(worker_of("l1")),
                None
            ),
            act_rejected(
                RejectCode::WorkerCannotSpawn,
                WORKER_CANNOT_SPAWN_L1,
                Tool::StartSession
            )
        );
    }

    // A4, before A5 (design 6.6 table order). Defect: a worker reads notes or
    // acts on sessions, though it may only message its lead; or its send into
    // another worker gets A5's steer_via_lead instead of worker_scope.
    #[test]
    fn a4_a_worker_may_use_no_app_action() {
        for (action, target) in [
            ("notes.list", SessionLevel::Lead),
            ("sessions.list", SessionLevel::Lead),
            ("sessions.read", SessionLevel::Lead),
            ("sessions.send", worker_of("l1")),
        ] {
            assert_eq!(
                act(
                    &session_caller("w1"),
                    action,
                    Some(worker_of("l1")),
                    Some(target)
                ),
                act_rejected(
                    RejectCode::WorkerScope,
                    &worker_scope("l1"),
                    Tool::AppAction
                ),
                "{action}"
            );
        }
    }

    // A5 (design 15 C6). Defect: sessions.send or sessions.draft reaches a
    // worker around its lead, or its lead is refused.
    #[test]
    fn a5_only_its_lead_may_send_or_draft_into_a_worker() {
        for action in ["sessions.send", "sessions.draft"] {
            for (case, who, level) in [
                ("Mono", mono_a_caller(), None),
                ("Giga", caller(CallerKind::MonoChat), None),
                (
                    "another lead",
                    session_caller("l2"),
                    Some(SessionLevel::Lead),
                ),
            ] {
                assert_eq!(
                    act(&who, action, level, Some(worker_of("l1"))),
                    act_rejected(RejectCode::SteerViaLead, &via_lead("l1"), Tool::AppAction),
                    "{case} {action}"
                );
            }
            assert_eq!(
                act(
                    &session_caller("l1"),
                    action,
                    Some(SessionLevel::Lead),
                    Some(worker_of("l1"))
                ),
                ALLOWED,
                "{action}"
            );
        }
        assert_eq!(
            act(
                &mono_a_caller(),
                "sessions.stop",
                None,
                Some(worker_of("l1"))
            ),
            ALLOWED
        );
    }

    // A6. Defect: a lead's start makes a lead, not a worker of that lead.
    #[test]
    fn a6_a_leads_start_makes_a_worker_of_that_lead() {
        for lead in [session_caller("l1"), operator("l1")] {
            assert_eq!(
                act(&lead, "sessions.start", Some(SessionLevel::Lead), None),
                ActionDecision::Allow {
                    lead_session_id: Some(session("l1"))
                },
                "{lead:?}"
            );
        }
    }

    // A7. Defect: a Mono's start makes a worker of the Mono's chat or habit
    // run.
    #[test]
    fn a7_a_monos_start_makes_a_lead() {
        let run = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::HabitRun,
                mono_id: mono("mono-a"),
            },
            session_id: session("a-run"),
            turn_id: "turn-1".into(),
        };
        for who in [mono_a_caller(), run] {
            assert_eq!(act(&who, "sessions.start", None, None), ALLOWED, "{who:?}");
        }
    }

    // Review r1 m10. Defect: the level rules bind only `started` sessions,
    // so an /operator lead messages a sibling lead and an /operator worker
    // starts sessions.
    #[test]
    fn l7_and_a3_bind_operator_sessions_too() {
        let sibling = in_alpha("l2", SessionLevel::Lead);
        assert_eq!(
            decide_as(
                &operator("l1"),
                lead_row(),
                &new("Hi", "l2", false),
                about(sibling.clone())
            ),
            send_rejected(RejectCode::LeadScope, sibling.party(), LEAD_SCOPE),
            "L7"
        );
        assert_eq!(
            act(
                &operator("w1"),
                "sessions.start",
                Some(worker_of("l1")),
                None
            ),
            act_rejected(
                RejectCode::WorkerCannotSpawn,
                WORKER_CANNOT_SPAWN_L1,
                Tool::StartSession
            ),
            "A3"
        );
    }

    /// The accepted `start_session` row (design 16.2) that `from_session`
    /// wrote when it started `started`: no slot, so no turn of its own.
    fn start_row(from_mono: Option<&str>, from_session: &str, started: &str) -> MonoMessage {
        MonoMessage {
            tool: Tool::StartSession,
            from_session_id: session(from_session),
            to: Some(Party::Session {
                session_id: session(started),
            }),
            addressed: "sessions.start".into(),
            status: Status::Delivered {
                wanting_reply: Wanting,
                intent: Intent::New,
                routed: Routed {
                    handled_by_session: session(started),
                    delivery: Delivery::Submit,
                },
                slot: None,
            },
            ..row('s', from_mono, 0, open(1))
        }
    }

    /// A steer through `start` into session `into`, pinned to its turn `turn`.
    fn pinned_steer(start: &MonoMessage, into: &str, turn: &str) -> Decision {
        Decision::Accept {
            hop: start.hop + 1,
            rows: vec![Outgoing {
                to: Party::Session {
                    session_id: session(into),
                },
                route: Route {
                    session: session(into),
                    via: Via::Steer,
                },
                intent: Intent::Steer(SteerTarget::Pinned {
                    message: start.id.clone(),
                    turn_id: turn.into(),
                }),
            }],
        }
    }

    // Design 16.2 and 16.6: a start row names the work of the whole session
    // it started. PR 8 pins a steer with no turn of its own to the turn its
    // session runs when the gate accepts it. Defect: a Mono can never steer
    // the lead it started, the steer reaches an idle lead as new work, or it
    // is not pinned, so no hand-off can deliver it.
    #[test]
    fn a_steer_naming_a_start_row_is_pinned_to_the_turn_that_session_runs() {
        let start = start_row(Some("mono-a"), "a-chat", "l1");
        let steer_through = |m: &MonoMessage, l1: Target| {
            decide_as(
                &mono_a_caller(),
                mono_a_row(),
                &at("l1", steer("m_ssssssssssss")),
                SendFacts {
                    recipient: Recipient::Session {
                        target: l1.clone(),
                        own_project: true,
                        level: SessionLevel::Lead,
                    },
                    reply_to: Some(Referenced {
                        message: m.clone(),
                        session: Some(l1),
                    }),
                    pair: PairCount::default(),
                },
            )
        };
        let steer_l1 = |l1: Target| steer_through(&start, l1);
        let a_later_turn = Target {
            turn: Some("turn-9".into()),
            ..live("l1", true, true)
        };
        assert_eq!(steer_l1(a_later_turn), pinned_steer(&start, "l1", "turn-9"));
        assert_eq!(
            steer_l1(live("l1", false, true)),
            send_rejected(
                RejectCode::TargetGone,
                Some(Party::Session {
                    session_id: session("l1")
                }),
                TARGET_GONE
            )
        );
        // Only a start row: a message to l1 still waiting in its queue
        // started no work, so a steer naming it is target_gone.
        let queued = MonoMessage {
            tool: Tool::SendMessage,
            addressed: "l1".into(),
            ..start.clone()
        };
        assert_eq!(
            steer_through(&queued, live("l1", true, true)),
            send_rejected(
                RejectCode::TargetGone,
                Some(Party::Session {
                    session_id: session("l1")
                }),
                TARGET_GONE
            )
        );
    }

    // Design 16.2: a session caller's own rows have no Mono. Defect: a lead
    // cannot steer the worker it started.
    #[test]
    fn a_lead_steers_its_worker_by_the_start_row_it_wrote() {
        let start = start_row(None, "l1", "w1");
        let w1 = live("w1", true, true);
        assert_eq!(
            decide_as(
                &session_caller("l1"),
                lead_row(),
                &at("w1", steer("m_ssssssssssss")),
                SendFacts {
                    recipient: Recipient::Session {
                        target: w1.clone(),
                        own_project: true,
                        level: worker_of("l1"),
                    },
                    reply_to: Some(Referenced {
                        message: start.clone(),
                        session: Some(w1),
                    }),
                    pair: PairCount::default(),
                },
            ),
            pinned_steer(&start, "w1", "turn-1")
        );
    }

    fn act_on(who: &Caller, hash: &PayloadHash, facts: ActionFacts) -> ActionDecision {
        decide_action(who, "sessions.start", hash, &facts)
    }

    // A1. Defect: a lead's retried start is refused, replayed as a refusal or
    // loses its lead id; a retried refusal is decided again (and writes a
    // second row); or a start whose session ended is launched again.
    #[test]
    fn a1_the_same_action_again_answers_its_first_outcome() {
        let input = json!({"prompt": "Build it", "harness": "codex"});
        let hash = action_hash("sessions.start", &input);
        let lead = session_caller("l1");
        let started = MonoMessage {
            payload_hash: hash.clone(),
            ..start_row(None, "l1", "w1")
        };
        let facts = |level: SessionLevel, prior: MonoMessage, ended: bool| ActionFacts {
            caller_level: Some(level),
            caller_role: None,
            target_level: None,
            prior: Some(prior),
            prior_start_ended: ended,
        };
        assert_eq!(
            act_on(
                &lead,
                &hash,
                facts(SessionLevel::Lead, started.clone(), false)
            ),
            ActionDecision::Allow {
                lead_session_id: Some(session("l1"))
            }
        );
        assert_eq!(
            act_on(&lead, &hash, facts(SessionLevel::Lead, started, true)),
            ActionDecision::Refuse {
                code: Refusal::RequestIdReused,
                hint: START_ENDED.into()
            }
        );
        let worker = session_caller("w1");
        let refused = MonoMessage {
            payload_hash: hash.clone(),
            tool: Tool::StartSession,
            from_session_id: session("w1"),
            to: None,
            addressed: "sessions.start".into(),
            status: Status::Rejected {
                gate_result: RejectCode::WorkerCannotSpawn,
                wanting_reply: false,
                intent: IntentKind::New,
                reply_to: None,
                scope: None,
            },
            ..row('r', None, 0, open(1))
        };
        // The refusal again, even when the caller has become a lead since.
        for level in [worker_of("l1"), SessionLevel::Lead] {
            assert_eq!(
                act_on(&worker, &hash, facts(level.clone(), refused.clone(), false)),
                ActionDecision::Replay {
                    prior: refused.id.clone(),
                    code: RejectCode::WorkerCannotSpawn,
                    hint: if level == SessionLevel::Lead {
                        RejectCode::WorkerCannotSpawn.hint().into()
                    } else {
                        WORKER_CANNOT_SPAWN_L1.into()
                    },
                },
                "{level:?}"
            );
        }
        // m1: a Mono's refused steer of a worker (A5) again names the
        // target's lead, not the generic hint.
        let send = json!({"sessionId": "w1", "prompt": "Stop"});
        let steer_hash = action_hash("sessions.send", &send);
        let steer = MonoMessage {
            payload_hash: steer_hash.clone(),
            status: Status::Rejected {
                gate_result: RejectCode::SteerViaLead,
                wanting_reply: false,
                intent: IntentKind::New,
                reply_to: None,
                scope: None,
            },
            ..row('s', Some("mono-a"), 0, open(1))
        };
        assert_eq!(
            decide_action(
                &mono_a_caller(),
                "sessions.send",
                &steer_hash,
                &ActionFacts {
                    caller_level: None,
                    caller_role: None,
                    target_level: Some(worker_of("l1")),
                    prior: Some(steer.clone()),
                    prior_start_ended: false,
                },
            ),
            ActionDecision::Replay {
                prior: steer.id,
                code: RejectCode::SteerViaLead,
                hint: "That session is a worker of lead l1. Message the lead (l1); only its lead steers its workers.".into(),
            }
        );
    }

    // A2. Defect: a request id that another call already used (a send, or a
    // start with other input) starts a session, or answers as that call did;
    // or a start with no row whose session ended is launched again.
    #[test]
    fn a2_a_request_id_used_with_other_input_is_refused() {
        let who = mono_a_caller();
        let hash = action_hash("sessions.start", &json!({"prompt": "Build it"}));
        let facts = |prior: Option<MonoMessage>, ended: bool| ActionFacts {
            caller_level: None,
            caller_role: None,
            target_level: None,
            prior,
            prior_start_ended: ended,
        };
        let refused = |hint: &str| ActionDecision::Refuse {
            code: Refusal::RequestIdReused,
            hint: hint.into(),
        };
        assert_eq!(
            act_on(
                &who,
                &hash,
                facts(Some(row('a', Some("mono-a"), 0, open(1))), false)
            ),
            refused(REQUEST_ID_REUSED)
        );
        let other_input = MonoMessage {
            payload_hash: action_hash("sessions.start", &json!({"prompt": "Build that"})),
            ..start_row(Some("mono-a"), "a-chat", "l1")
        };
        assert_eq!(
            act_on(&who, &hash, facts(Some(other_input), false)),
            refused(REQUEST_ID_REUSED)
        );
        assert_eq!(act_on(&who, &hash, facts(None, true)), refused(START_ENDED));
    }
}
