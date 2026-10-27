//! The ask and ack rules (Change R plan v2, R5 and R5c), with no IO.
//! `asks.rs` loads the facts from saved rows and writes what these decide.

use std::fmt;

use crate::mono_gate::{Level, SessionLevel};
use crate::mono_messages::{MonoId, SessionId};
use crate::mono_roster::MonoRole;

/// Ask budget in active minutes: the default and the bounds of `due_minutes`.
pub const DEFAULT_DUE_MINUTES: u32 = 60;
pub const DUE_MINUTES: std::ops::RangeInclusive<u32> = 10..=480;

/// Who asks or is asked (R5c). A Giga or Mono is its mono id, so it acks
/// from any chat it has and a chat reset changes nothing. A lead or worker
/// is its session. `mono_asks` stores it as `mono:<id>` or `session:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Mono(MonoId),
    Session(SessionId),
}

impl Actor {
    pub fn parse(stored: &str) -> Option<Self> {
        match stored.split_once(':')? {
            ("mono", id) => MonoId::parse(id).map(Actor::Mono),
            ("session", id) => SessionId::parse(id).map(Actor::Session),
            _ => None,
        }
    }

    /// The id a `send_to` names this actor by.
    pub fn send_to(&self) -> &str {
        match self {
            Actor::Mono(id) => id.as_str(),
            Actor::Session(id) => id.as_str(),
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Actor::Mono(id) => write!(f, "mono:{}", id.as_str()),
            Actor::Session(id) => write!(f, "session:{}", id.as_str()),
        }
    }
}

/// A party to an ask, built from a Mono's role or a session's level, so a
/// Giga or Mono is always a `mono:` actor and a lead or worker a `session:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    level: Level,
    actor: Actor,
}

impl Party {
    pub fn mono(role: MonoRole, mono: MonoId) -> Self {
        let level = match role {
            MonoRole::Giga => Level::Giga,
            MonoRole::Mono => Level::Mono,
        };
        Party {
            level,
            actor: Actor::Mono(mono),
        }
    }

    pub fn session(level: SessionLevel, session: SessionId) -> Self {
        let level = match level {
            SessionLevel::Lead => Level::Lead,
            SessionLevel::Worker { lead } => Level::Worker { lead },
        };
        Party {
            level,
            actor: Actor::Session(session),
        }
    }

    pub fn level(&self) -> &Level {
        &self.level
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    pub fn into_actor(self) -> Actor {
        self.actor
    }
}

/// Plan section 1: Giga to a Mono, Mono to a lead it started, lead to its
/// worker. `started_by_parent` is whether the parent Mono's accepted
/// `start_session` row names the child.
pub fn is_direct_child(parent: &Party, child: &Party, started_by_parent: bool) -> bool {
    match (&parent.level, &child.level) {
        (Level::Giga, Level::Mono) => true,
        (Level::Mono, Level::Lead) => started_by_parent,
        (Level::Lead, Level::Worker { lead }) => {
            matches!(&parent.actor, Actor::Session(session) if session == lead)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskRefused {
    NotDirectChild,
}

impl AskRefused {
    pub fn code(self) -> &'static str {
        "not_direct_child"
    }

    pub fn hint(self) -> &'static str {
        "You can ask only your direct child: a Giga asks a Mono, a Mono asks a lead it started, a lead asks its own worker. Send others a message instead."
    }
}

/// An ask from `parent` to `child`, both from saved rows, at active time
/// `active_ms`: its `due_active_ms`, `None` exactly when a Giga asks (a
/// level 1 parent sets no clock). A party with no saved row never gets
/// here: the send gate answers it (`unknown_caller`, `not_a_mono`).
pub fn decide_ask(
    parent: &Party,
    child: &Party,
    started_by_parent: bool,
    due_minutes: Option<u32>,
    active_ms: i64,
) -> Result<Option<i64>, AskRefused> {
    if !is_direct_child(parent, child, started_by_parent) {
        return Err(AskRefused::NotDirectChild);
    }
    let minutes = i64::from(due_minutes.unwrap_or(DEFAULT_DUE_MINUTES));
    Ok((parent.level != Level::Giga).then_some(active_ms + minutes * 60_000))
}

/// `mono_asks.state`. Nothing expires (R5c): an open ask stays open until
/// its child acks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Acked,
}

/// An ask as `ack` reads it: its state, who asked (where the ack goes) and
/// who may ack it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskState {
    pub state: State,
    pub parent: Actor,
    pub child: Actor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckRefused {
    NoOpenAsk,
    AlreadyAcked,
    NotYours,
}

impl AckRefused {
    pub fn hint(self) -> &'static str {
        match self {
            AckRefused::NoOpenAsk => "No open ask has this id.",
            AckRefused::AlreadyAcked => {
                "This ask was already acked. A new ask from your parent has a new id."
            }
            AckRefused::NotYours => {
                "This ask is not yours: only the Mono or session it was sent to can ack it."
            }
        }
    }
}

/// Why the guarded ack `UPDATE` changed no row, from the ask as it stands
/// and the acking actor. `Ok` is an ask this caller may ack.
pub fn decide_ack(ask: &AskState, caller: &Actor) -> Result<(), AckRefused> {
    if &ask.child != caller {
        return Err(AckRefused::NotYours);
    }
    match ask.state {
        State::Open => Ok(()),
        State::Acked => Err(AckRefused::AlreadyAcked),
    }
}

/// R14: what is said on an open ask while its key stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Talk {
    /// The child asks its parent; the ask's clock pauses until the answer.
    Question,
    /// The parent answers question `seq`; the clock resumes.
    Answer { seq: i64 },
    /// The child reports progress, once; the clock runs on.
    Update,
}

/// Questions one ask may carry (`mono_ask_events` CHECKs the same).
pub const MAX_QUESTIONS: i64 = 3;

impl Talk {
    /// The tool and the `mono_ask_events.kind`.
    pub fn kind(self) -> &'static str {
        match self {
            Talk::Question => "question",
            Talk::Answer { .. } => "answer",
            Talk::Update => "update",
        }
    }

    pub fn refused_code(self) -> &'static str {
        match self {
            Talk::Question => "question_refused",
            Talk::Answer { .. } => "answer_refused",
            Talk::Update => "update_refused",
        }
    }

    /// Who may say it and who hears it: the child asks and updates its
    /// asker, and the asker answers its child.
    pub fn parties(self, ask: &AskState) -> (&Actor, &Actor) {
        match self {
            Talk::Answer { .. } => (&ask.parent, &ask.child),
            Talk::Question | Talk::Update => (&ask.child, &ask.parent),
        }
    }

    /// The refusal for a caller that may not say it on an ask.
    pub fn not_a_party(self) -> TalkRefused {
        match self {
            Talk::Answer { .. } => TalkRefused::NotYourAsk,
            Talk::Question | Talk::Update => TalkRefused::NotYours,
        }
    }

    /// The seq an answer names: part of the call, for dedupe by request id.
    pub fn answers(self) -> Option<i64> {
        match self {
            Talk::Answer { seq } => Some(seq),
            Talk::Question | Talk::Update => None,
        }
    }
}

/// What an ask already carries, as `decide_talk` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Said {
    pub questions: i64,
    /// A question waits for its answer: `paused_at_active_ms` is set.
    pub paused: bool,
    pub updated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkRefused {
    NoOpenAsk,
    AlreadyAcked,
    NotYours,
    NotYourAsk,
    QuestionWaits { seq: i64 },
    AnswerOtherSeq { seq: i64 },
    NoQuestionsLeft,
    NoQuestionWaits,
    UpdateSent,
}

impl TalkRefused {
    pub fn hint(self) -> String {
        match self {
            TalkRefused::NoOpenAsk => "No open ask has this id. Name it with ask_id.".into(),
            TalkRefused::AlreadyAcked => "This ask was already acked.".into(),
            TalkRefused::NotYours => "This ask is not yours: only the Mono or session it was sent to can ask a question or send an update on it.".into(),
            TalkRefused::NotYourAsk => "Only the one who asked can answer its questions.".into(),
            TalkRefused::QuestionWaits { seq } => format!("Question {seq} on this ask waits for its answer. Ask the next one after it comes."),
            TalkRefused::AnswerOtherSeq { seq } => format!("The question that waits is seq {seq}. Answer it with seq {seq}."),
            TalkRefused::NoQuestionsLeft => format!("This ask already had its {MAX_QUESTIONS} questions. Decide, or ack and say what is missing."),
            TalkRefused::NoQuestionWaits => "No question on this ask waits for an answer.".into(),
            TalkRefused::UpdateSent => "This ask already had its one update. Ack it when the work is done.".into(),
        }
    }
}

/// Plan section 2: the one extension moves the deadline by 15 active minutes.
pub const EXTENSION_MS: i64 = 15 * 60_000;

/// An ask as `extend` reads it: `due` is its deadline (none for a Giga's
/// ask), `stage` the ladder rungs already enqueued (`ask_scheduler`: 0
/// none, 1 `t5`, 2 `deadline`, 3 `why`), `extended` that its one
/// `extension` event exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendState {
    pub ask: AskState,
    pub due: Option<i64>,
    pub stage: i64,
    pub extended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendRefused {
    NoOpenAsk,
    NoSingleOpenAsk,
    NotYours,
    AlreadyAcked,
    NoDeadline,
    BeforeReminder,
    PastDeadline,
    AlreadyExtended,
}

impl ExtendRefused {
    pub fn hint(self) -> &'static str {
        match self {
            ExtendRefused::NoOpenAsk => {
                "No open ask of yours has this id. Pass the ask_id of the <mono_reminder>; you may leave it out when you have exactly one open ask."
            }
            ExtendRefused::NoSingleOpenAsk => {
                "You have no open ask or more than one. Pass the ask_id of the <mono_reminder>."
            }
            ExtendRefused::NotYours => {
                "This ask is not yours: only the Mono or session it was sent to can extend it."
            }
            ExtendRefused::AlreadyAcked => "This ask was already acked, so it has no deadline left.",
            ExtendRefused::NoDeadline => "A Giga's ask has no deadline to extend.",
            ExtendRefused::BeforeReminder => {
                "The extension opens with the reminder 5 active minutes before the deadline. Work on, or ask your parent a question."
            }
            ExtendRefused::PastDeadline => {
                "The deadline has passed, so the extension is closed. Finish the work and ack it; when your parent asks why, give your reason with messages.reason."
            }
            ExtendRefused::AlreadyExtended => {
                "This ask was already extended once; its deadline moves only once. Finish the work and ack it."
            }
        }
    }
}

/// R14: whether `caller` may say `talk` on `ask`, and the event's `seq`.
/// The child asks up to 3 questions, one at a time, and sends one update;
/// the parent answers the question that waits, by its seq.
pub fn decide_talk(
    ask: &AskState,
    said: Said,
    talk: Talk,
    caller: &Actor,
) -> Result<i64, TalkRefused> {
    if talk.parties(ask).0 != caller {
        return Err(talk.not_a_party());
    }
    if ask.state == State::Acked {
        return Err(TalkRefused::AlreadyAcked);
    }
    match talk {
        Talk::Question if said.paused => Err(TalkRefused::QuestionWaits {
            seq: said.questions,
        }),
        Talk::Question if said.questions >= MAX_QUESTIONS => Err(TalkRefused::NoQuestionsLeft),
        Talk::Question => Ok(said.questions + 1),
        Talk::Answer { .. } if !said.paused => Err(TalkRefused::NoQuestionWaits),
        Talk::Answer { seq } if seq != said.questions => Err(TalkRefused::AnswerOtherSeq {
            seq: said.questions,
        }),
        Talk::Answer { seq } => Ok(seq),
        Talk::Update if said.updated => Err(TalkRefused::UpdateSent),
        Talk::Update => Ok(1),
    }
}

/// R14: the deadline once an answer resumes the clock where the question
/// paused it, moved by the active time the question waited.
pub fn resumed_due(due: Option<i64>, paused_at: i64, active_ms: i64) -> Option<i64> {
    due.map(|due| due + active_ms - paused_at)
}

/// Plan R10, R11: only the child extends, only an open ask with a deadline,
/// only once, and only between its `t5` and `deadline` reminders. `Ok` is
/// the new deadline.
pub fn decide_extend(ask: &ExtendState, caller: &Actor) -> Result<i64, ExtendRefused> {
    if &ask.ask.child != caller {
        return Err(ExtendRefused::NotYours);
    }
    if ask.ask.state == State::Acked {
        return Err(ExtendRefused::AlreadyAcked);
    }
    let Some(due) = ask.due else {
        return Err(ExtendRefused::NoDeadline);
    };
    if ask.extended {
        return Err(ExtendRefused::AlreadyExtended);
    }
    match ask.stage {
        ..=0 => Err(ExtendRefused::BeforeReminder),
        1 => Ok(due + EXTENSION_MS),
        _ => Err(ExtendRefused::PastDeadline),
    }
}

/// An ask as `reason` reads it: `missed` that its `miss` event exists,
/// `reasoned` that the miss already holds a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonState {
    pub ask: AskState,
    pub missed: bool,
    pub reasoned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonRefused {
    NoOpenAsk,
    NoSingleOpenAsk,
    NotYours,
    AlreadyAcked,
    NoMiss,
    AlreadyGiven,
}

impl ReasonRefused {
    pub fn hint(self) -> &'static str {
        match self {
            ReasonRefused::NoOpenAsk => {
                "No open ask of yours has this id. Pass the ask_id of the <mono_reminder>; you may leave it out when you have exactly one open ask."
            }
            ReasonRefused::NoSingleOpenAsk => {
                "You have no open ask or more than one. Pass the ask_id of the <mono_reminder>."
            }
            ReasonRefused::NotYours => {
                "This ask is not yours: only the Mono or session it was sent to can give its reason."
            }
            ReasonRefused::AlreadyAcked => "This ask was already acked; it needs no reason.",
            ReasonRefused::NoMiss => {
                "This ask has not missed its deadline. A reason opens once your parent is told of the miss; until then, finish the work and ack it."
            }
            ReasonRefused::AlreadyGiven => {
                "This miss already has its reason; it is stored once. Finish the work and ack the ask."
            }
        }
    }
}

/// Plan R11: only the child gives the reason, only for an open ask whose
/// `miss` exists, and only once. The key stays: a later `ack` closes the ask.
pub fn decide_reason(ask: &ReasonState, caller: &Actor) -> Result<(), ReasonRefused> {
    if &ask.ask.child != caller {
        return Err(ReasonRefused::NotYours);
    }
    if ask.ask.state == State::Acked {
        return Err(ReasonRefused::AlreadyAcked);
    }
    if !ask.missed {
        return Err(ReasonRefused::NoMiss);
    }
    if ask.reasoned {
        return Err(ReasonRefused::AlreadyGiven);
    }
    Ok(())
}

/// How a reminder reaches its chat (plan R9): into the running turn, as a
/// new turn, or not yet. `ask_scheduler::claim` turns it into `Claimed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Steer,
    Wake,
    AfterTurn,
}

/// Plan R9: an idle chat wakes with a new turn; a busy one takes the
/// reminder as a steer when its window can steer the running turn now, and
/// otherwise waits for the turn end. `running` comes from `ControlHost`,
/// `steerable` from the window that would deliver it.
pub fn delivery_for(running: bool, steerable: bool) -> Delivery {
    match (running, steerable) {
        (false, _) => Delivery::Wake,
        (true, true) => Delivery::Steer,
        (true, false) => Delivery::AfterTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(role: MonoRole, id: &str) -> Party {
        Party::mono(role, MonoId::parse(id).unwrap())
    }

    fn session(level: SessionLevel, id: &str) -> Party {
        Party::session(level, SessionId::parse(id).unwrap())
    }

    fn worker_of(lead: &str) -> SessionLevel {
        SessionLevel::Worker {
            lead: SessionId::parse(lead).unwrap(),
        }
    }

    // Defect: a parent asks a session that is not its direct child (a Giga
    // asks a lead, a Mono a lead it did not start, a lead another lead's
    // worker, a worker anyone), or a direct child is refused.
    #[test]
    fn only_a_direct_child_can_be_asked() {
        let giga = mono(MonoRole::Giga, "giga");
        let mono = mono(MonoRole::Mono, "mono");
        let lead = session(SessionLevel::Lead, "lead");
        let worker = session(worker_of("lead"), "worker");
        let others = session(worker_of("other"), "w2");
        for (parent, child, started, direct) in [
            (&giga, &mono, false, true),
            (&mono, &lead, true, true),
            (&mono, &lead, false, false),
            (&lead, &worker, false, true),
            (&lead, &others, false, false),
            (&giga, &lead, true, false),
            (&mono, &worker, true, false),
            (&worker, &lead, true, false),
            (&mono, &giga, true, false),
        ] {
            assert_eq!(
                decide_ask(parent, child, started, None, 0).is_ok(),
                direct,
                "{parent:?} -> {child:?}, started {started}"
            );
        }
    }

    // Defect: a Giga's ask gets a deadline, or a lower parent's deadline is
    // not the active time now plus its budget.
    #[test]
    fn the_due_time_is_active_time_plus_the_budget_and_none_for_a_giga() {
        let giga = mono(MonoRole::Giga, "g");
        let mono = mono(MonoRole::Mono, "mono");
        let lead = session(SessionLevel::Lead, "lead");
        let worker = session(worker_of("lead"), "worker");
        assert_eq!(decide_ask(&giga, &mono, false, Some(30), 5_000), Ok(None));
        assert_eq!(
            decide_ask(&mono, &lead, true, None, 5_000),
            Ok(Some(5_000 + 60 * 60_000))
        );
        assert_eq!(
            decide_ask(&lead, &worker, false, Some(10), 7),
            Ok(Some(7 + 10 * 60_000))
        );
    }

    // Defect: an ack of another actor's ask or of an acked ask is told the
    // wrong reason, or a Mono's ask is the session's of the same id.
    #[test]
    fn an_ack_that_changes_no_row_names_why() {
        let me = Actor::parse("session:worker").unwrap();
        let open = |child: &str| AskState {
            state: State::Open,
            parent: Actor::parse("session:lead").unwrap(),
            child: Actor::parse(child).unwrap(),
        };
        assert_eq!(
            decide_ack(&open("session:w2"), &me),
            Err(AckRefused::NotYours)
        );
        assert_eq!(
            decide_ack(&open("mono:worker"), &me),
            Err(AckRefused::NotYours)
        );
        let acked = AskState {
            state: State::Acked,
            ..open("session:worker")
        };
        assert_eq!(decide_ack(&acked, &me), Err(AckRefused::AlreadyAcked));
        assert_eq!(decide_ack(&open("session:worker"), &me), Ok(()));
    }
}
