//! The messaging gate (design 6): one pure function per tool from a call and
//! the facts `mono_messages::submit` read in its transaction to a
//! [`Decision`]. The first matching row of design 16.5 or 6.4 wins.
//!
//! PR I2 has rows S1-S7, I1, I2, S9, S10, S13-S17, S20 and R1-R8, R10, R11.
//! The direction matrix, rule (b), hop and rate limits come with PR 7.

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::mono_messages::{
    Delivery, Handle, Intent, IntentKind, MessageId, MonoId, MonoMessage, Party, PayloadHash,
    RejectCode, SessionId, Status, Tool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerKind {
    MonoChat,
    HabitRun,
}

/// The Mono a call comes from, as the web app resolved it (design 2 step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub kind: CallerKind,
    pub mono_id: MonoId,
    pub session_id: SessionId,
    /// The turn that authorized the call (design 2 step 4). A reply answers
    /// only the messages of this turn.
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendInput {
    pub message: String,
    pub send_to: String,
    pub wanting_reply: bool,
    /// As asked: rows I1 and I2 refuse the ones a send cannot carry.
    pub intent: IntentKind,
    /// A message id only (PR I2 review r1): a handle names a message the
    /// caller's session received, which S10 refuses as a steer target.
    pub reply_to: Option<MessageId>,
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
            Input::Send(send) => json!(["send_message", {
                "message": send.message,
                "send_to": send.send_to,
                "wanting_reply": send.wanting_reply,
                "intent": send.intent.as_str(),
                "reply_to": send.reply_to.as_ref().map(MessageId::as_str),
            }]),
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
        let digest = format!("{:x}", Sha256::digest(canonical.to_string().as_bytes()));
        PayloadHash::parse(&digest).expect("sha256 is 64 lowercase hex digits")
    }
}

/// S1 and R1: the input the model sent, or the `invalid_input` hint.
pub fn parse(tool: Tool, raw: &Value) -> Result<Input, String> {
    let empty = Map::new();
    let fields = raw.as_object().unwrap_or(&empty);
    let allowed: &[&str] = match tool {
        Tool::SendMessage => &["message", "send_to", "wanting_reply", "intent", "reply_to"],
        Tool::Reply => &["message", "wanting_reply", "to"],
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
    let message = match fields.get("message") {
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
    let reply_to = fields
        .get("reply_to")
        .map(|value| {
            value
                .as_str()
                .and_then(MessageId::parse)
                .ok_or(REPLY_TO_RULE)
        })
        .transpose()?;
    Ok(Input::Send(SendInput {
        message,
        send_to,
        wanting_reply,
        intent,
        reply_to,
    }))
}

const INTENT_RULE: &str = r#"intent is required and is one of "new", "steer", "reply" or "continue". Use "new" for new work, or "steer" with reply_to to change the work a message started."#;
/// Also the answer to a handle (PR I2 review r1): a steer names a message
/// the caller sent, and only its id can.
const REPLY_TO_RULE: &str = "reply_to must be one message_id (m_…). A steer names a message you sent: use the message_id your send_message returned.";

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
        chat: Option<Target>,
    },
    /// An existing session. `own_project` is false for Mono chats, habit runs
    /// and sessions outside the caller's projects.
    Session { target: Target, own_project: bool },
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

/// The work a steer's target started (design 16.6): the turn that handles
/// it, or none yet when it waits in a queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPin {
    Turn(String),
    NotStarted,
}

/// Where a steer that names M goes (design 16.6, receiver side; rule (b)'s
/// `Giga` variant comes with PR 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSession {
    /// M is not a message from the caller to the recipient.
    NotBetween,
    /// The session handling M, `None` once a drop took it away.
    Receiver {
        session: Option<SessionId>,
        pin: TurnPin,
    },
}

/// Design 16.6: a steer reaches the session handling `reply_to`, and only
/// when the caller sent M to this recipient.
pub fn target_session(caller: &Caller, recipient: &Recipient, m: &MonoMessage) -> TargetSession {
    let between = m.from_mono_id.as_ref() == Some(&caller.mono_id)
        && m.to.is_some()
        && m.to == recipient.party();
    if !between {
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
            slot.as_ref().map(|slot| turn(&slot.turn_id)),
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
/// for it as the live list and `ControlHost` see it.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts<About> {
    /// The caller's `monos` row with its current chat; `None` when the row
    /// is gone.
    pub caller: Option<Option<SessionId>>,
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
    /// One `rejected` row.
    Reject {
        code: RejectCode,
        hint: String,
        to: Option<Party>,
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
const REQUEST_ID_REUSED: &str =
    "This request id was already used with different input. Use a new request id.";

fn reject(code: RejectCode, to: Option<Party>) -> Decision {
    Decision::Reject {
        code,
        hint: code.hint().into(),
        to,
        open_handles: Vec::new(),
    }
}

/// The rows every tool shares: the caller's Mono, then S2/S3 (R2/R3).
/// `again` builds the answer of a call that was rejected.
fn prelude<A>(
    facts: &Facts<A>,
    hash: &PayloadHash,
    again: impl FnOnce(RejectCode) -> Rejection,
) -> Option<Decision> {
    if facts.caller.is_none() {
        return Some(Decision::Refuse {
            code: Refusal::NotAMono,
            hint: NOT_A_MONO.into(),
        });
    }
    let first = facts.prior.first()?;
    Some(if &first.payload_hash == hash {
        Decision::Duplicate {
            prior: facts.prior.clone(),
            rejection: match first.status {
                Status::Rejected { gate_result, .. } => Some(again(gate_result)),
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

/// The hint of a send rejection (design 16.5): the intent codes and S15 name
/// what this call asked for.
fn send_hint(code: RejectCode, input: &SendInput) -> String {
    match (code, input.intent) {
        (RejectCode::IntentMismatch, intent) if intent != IntentKind::New => format!(
            r#"intent "{}" needs reply_to: one message_id (m_…)."#,
            intent.as_str()
        ),
        (RejectCode::IntentNotAllowed, IntentKind::Reply) => r##"send_message cannot reply. Use reply (to: "#n") to answer a message that waits for you."##.into(),
        (RejectCode::TargetGone, _) => r#"The work that reply_to started is not running now: it finished, it has not started, or its session is idle, cleared or deleted. Send it with intent "new"."#.into(),
        _ => code.hint().into(),
    }
}

/// Design 16.5, the rows of PR 4a and PR I2 in order.
pub fn decide_send(
    caller: &Caller,
    input: &SendInput,
    hash: &PayloadHash,
    facts: &Facts<SendFacts>,
) -> Decision {
    let again = |code: RejectCode| Rejection {
        code,
        hint: send_hint(code, input),
        open_handles: Vec::new(),
    };
    if let Some(decision) = prelude(facts, hash, again) {
        return decision;
    }
    let recipient = &facts.about.recipient;
    let to = recipient.party();
    let reject = |code: RejectCode, to: Option<Party>| Decision::Reject {
        code,
        hint: send_hint(code, input),
        to,
        open_handles: Vec::new(),
    };
    if too_large(&input.message) {
        return reject(RejectCode::TooLarge, to);
    }
    // I1, then I2: the call alone, whatever it names.
    let steer = match (input.intent, &input.reply_to) {
        (IntentKind::New, None) => false,
        (IntentKind::Steer, Some(_)) => true,
        (IntentKind::New, Some(_)) | (_, None) => return reject(RejectCode::IntentMismatch, to),
        (IntentKind::Reply | IntentKind::Continue, Some(_)) => {
            return reject(RejectCode::IntentNotAllowed, to)
        }
    };
    let own_chat = facts.caller.clone().flatten();
    match recipient {
        Recipient::Unknown => return reject(RejectCode::UnknownTarget, None),
        Recipient::Mono { mono_id, .. } if *mono_id == caller.mono_id => {
            return reject(RejectCode::SelfSend, to)
        }
        Recipient::Session { target, .. }
            if target.session == caller.session_id
                || Some(&target.session) == own_chat.as_ref() =>
        {
            return reject(RejectCode::SelfSend, to)
        }
        Recipient::Session {
            own_project: false, ..
        } => return reject(RejectCode::DirectionForbidden, to),
        Recipient::Session { .. } | Recipient::Mono { .. } => {}
    }
    let steered = if steer {
        // S9, then S10.
        let Some(referenced) = &facts.about.reply_to else {
            return reject(RejectCode::UnknownMessage, to);
        };
        match target_session(caller, recipient, &referenced.message) {
            TargetSession::NotBetween => return reject(RejectCode::SteerTargetMismatch, to),
            TargetSession::Receiver { session, pin } => Some((referenced, session, pin)),
        }
    } else {
        None
    };
    // S13, S14.
    match recipient {
        Recipient::Session { .. } if input.wanting_reply => {
            return reject(RejectCode::ReplyUnsupportedTarget, to)
        }
        Recipient::Mono { .. } if caller.kind == CallerKind::HabitRun && input.wanting_reply => {
            return reject(RejectCode::HabitCannotWait, to)
        }
        _ => {}
    }
    let to_party = || to.clone().expect("a resolved recipient is a party");
    if let Some((referenced, session, pin)) = steered {
        // S15: the work M started must run now, in the turn that runs it.
        let running = referenced.session.as_ref().filter(|target| {
            session.as_ref() == Some(&target.session)
                && target.exists
                && match &pin {
                    TurnPin::Turn(turn) => target.turn.as_ref() == Some(turn),
                    TurnPin::NotStarted => false,
                }
        });
        let Some(target) = running else {
            return reject(RejectCode::TargetGone, to);
        };
        if !target.steerable {
            return reject(RejectCode::CannotSteer, to);
        }
        return Decision::Accept {
            hop: referenced.message.hop + 1,
            rows: vec![Outgoing {
                to: to_party(),
                route: Route {
                    session: target.session.clone(),
                    via: Via::Steer,
                },
                intent: Intent::Steer(referenced.message.id.clone()),
            }],
        };
    }
    // S16: new work goes to the recipient's chat or the project session,
    // and always waits for the turn end.
    let session = match recipient {
        Recipient::Session { target, .. } => &target.session,
        Recipient::Mono {
            chat: Some(chat), ..
        } if chat.exists => &chat.session,
        _ => return reject(RejectCode::TargetNoConversation, to),
    };
    Decision::Accept {
        hop: 0,
        rows: vec![Outgoing {
            to: to_party(),
            route: Route {
                session: session.clone(),
                via: Via::Queue,
            },
            intent: Intent::New,
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
    if let Some(decision) = prelude(facts, hash, |code| reply_again(code, to, &facts.about.then)) {
        return decision;
    }
    if too_large(&input.message) {
        return reject(RejectCode::TooLarge, None);
    }
    let named = match answers(to, &facts.about.handled) {
        Ok(named) => named,
        Err(rejection) => return rejection.into(),
    };
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
    Decision::Accept {
        hop: named
            .iter()
            .map(|handled| handled.message.hop)
            .max()
            .unwrap_or(0)
            + 1,
        rows,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mono_messages::{
        Body, Part, PlainSlot, RequestId, Routed, Status, WaitingSlot, Wanting,
    };

    fn mono(id: &str) -> MonoId {
        MonoId::parse(id).unwrap()
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn caller(kind: CallerKind) -> Caller {
        Caller {
            kind,
            mono_id: mono("giga"),
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
            reply_to: None,
        }
    }

    fn asking(intent: IntentKind, reply_to: Option<&str>) -> SendInput {
        SendInput {
            intent,
            reply_to: reply_to.map(|named| MessageId::parse(named).unwrap()),
            ..new("Hi", "mono-a", false)
        }
    }

    fn steer(reply_to: &str) -> SendInput {
        asking(IntentKind::Steer, Some(reply_to))
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
            chat,
        }
    }

    fn facts<A>(about: A) -> Facts<A> {
        Facts {
            caller: Some(Some(session("giga-chat"))),
            prior: Vec::new(),
            about,
        }
    }

    fn sends(recipient: Recipient) -> Facts<SendFacts> {
        facts(SendFacts {
            recipient,
            reply_to: None,
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
            "Unknown send_message fields: action_type. Allowed: message, send_to, wanting_reply, intent, reply_to."
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
            Ok(Input::Send(SendInput {
                reply_to: MessageId::parse("m_aaaaaaaaaaaa"),
                ..asking(IntentKind::Steer, None)
            }))
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
        );
        assert!(matches!(emoji, Decision::Accept { .. }), "{emoji:?}");
    }

    /// The rejection a send gets, with the hint of design 16.5.
    fn send_rejected(code: RejectCode, to: Option<Party>, hint: &str) -> Decision {
        Decision::Reject {
            code,
            hint: hint.into(),
            to,
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
            )
        };
        let new_with_target = r#"intent "new" takes no reply_to. Remove reply_to, or use intent "steer" to change the work that message started."#;
        let needs =
            |intent: &str| format!(r#"intent "{intent}" needs reply_to: one message_id (m_…)."#);
        for (input, code, hint) in [
            (
                asking(IntentKind::New, Some("m_aaaaaaaaaaaa")),
                RejectCode::IntentMismatch,
                new_with_target.to_string(),
            ),
            (
                asking(IntentKind::Steer, None),
                RejectCode::IntentMismatch,
                needs("steer"),
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

    // PR I2. Defect: the intent rows run before S4 or after S5, so a model
    // fixes the wrong thing first.
    #[test]
    fn s4_runs_before_i1_and_i1_before_s5() {
        let long = SendInput {
            message: "x".repeat(8001),
            ..asking(IntentKind::New, Some("m_aaaaaaaaaaaa"))
        };
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &long,
                &hash(),
                &sends(mono_a(Some(live("a-chat", true, true)))),
            ),
            rejected(RejectCode::TooLarge, party_a())
        );
        let nowhere = SendInput {
            send_to: "nope".into(),
            ..asking(IntentKind::Steer, None)
        };
        assert!(matches!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &nowhere,
                &hash(),
                &sends(Recipient::Unknown),
            ),
            Decision::Reject {
                code: RejectCode::IntentMismatch,
                to: None,
                ..
            }
        ));
    }

    #[test]
    fn s5_no_mono_and_no_session_is_unknown_target() {
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "nope", false),
                &hash(),
                &sends(Recipient::Unknown),
            ),
            rejected(RejectCode::UnknownTarget, None)
        );
    }

    #[test]
    fn s6_sending_to_yourself_or_your_own_chat_is_self_send() {
        let me = Recipient::Mono {
            mono_id: mono("giga"),
            chat: Some(live("giga-chat", true, true)),
        };
        let own_chat = Recipient::Session {
            target: live("giga-chat", true, true),
            own_project: true,
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
                }),
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
                }),
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
                    Intent::Steer(MessageId::parse("m_zzzzzzzzzzzz").unwrap())
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
                }),
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
            ),
            Decision::Accept { .. }
        ));
    }

    // PR I2, S20. Defect: new work still steers a running turn, a steer
    // goes anywhere but the session handling its target, or its hop does
    // not grow, so the hop limit cannot bound a chain of steers.
    #[test]
    fn s20_new_work_is_queued_and_a_steer_goes_into_the_turn_that_runs_its_target() {
        let a_chat = live("a-chat", true, true);
        assert_eq!(
            decide_send(
                &caller(CallerKind::MonoChat),
                &new("Hi", "mono-a", true),
                &hash(),
                &sends(mono_a(Some(a_chat.clone()))),
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
            ),
            Decision::Accept {
                hop: 3,
                rows: vec![Outgoing {
                    to: party_a().unwrap(),
                    route: Route {
                        session: session("a-old"),
                        via: Via::Steer
                    },
                    intent: Intent::Steer(MessageId::parse("m_aaaaaaaaaaaa").unwrap()),
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
                    },
                    to_project,
                    Some(project),
                ),
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
                    intent: Intent::Steer(MessageId::parse("m_pppppppppppp").unwrap()),
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
}
