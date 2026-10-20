//! The ask and ack rules (Change R plan v2, R5), with no IO. `asks.rs` loads
//! the facts from saved rows and writes what these decide.

use crate::mono_gate::Level;
use crate::mono_messages::SessionId;

/// Ask budget in active minutes: the default and the bounds of `due_minutes`.
pub const DEFAULT_DUE_MINUTES: u32 = 60;
pub const DUE_MINUTES: std::ops::RangeInclusive<u32> = 10..=480;

/// A party to an ask: its level and the session that speaks for it (a
/// Mono's chat for a Giga or Mono).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    pub level: Level,
    pub session: SessionId,
}

/// Plan section 1: Giga to a Mono, Mono to a lead it started, lead to its
/// worker. `started_by_parent` is whether the parent Mono's accepted
/// `start_session` row names the child.
pub fn is_direct_child(parent: &Party, child: &Party, started_by_parent: bool) -> bool {
    match (&parent.level, &child.level) {
        (Level::Giga, Level::Mono) => true,
        (Level::Mono, Level::Lead) => started_by_parent,
        (Level::Lead, Level::Worker { lead }) => *lead == parent.session,
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

/// `mono_asks.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Acked,
    Dropped,
}

/// An ask as `ack` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskState {
    pub state: State,
    pub child_session: SessionId,
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
                "This ask is not yours: only the session it was sent to can ack it."
            }
        }
    }
}

/// Why the guarded ack `UPDATE` changed no row, from the ask as it stands
/// and the acking session. `Ok` is an ask this caller may ack.
pub fn decide_ack(ask: &AskState, caller: &SessionId) -> Result<(), AckRefused> {
    if &ask.child_session != caller {
        return Err(AckRefused::NotYours);
    }
    match ask.state {
        State::Open => Ok(()),
        State::Acked => Err(AckRefused::AlreadyAcked),
        State::Dropped => Err(AckRefused::NoOpenAsk),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(level: Level, session: &str) -> Party {
        Party {
            level,
            session: SessionId::parse(session).unwrap(),
        }
    }

    fn worker_of(lead: &str) -> Level {
        Level::Worker {
            lead: SessionId::parse(lead).unwrap(),
        }
    }

    // Defect: a parent asks a session that is not its direct child (a Giga
    // asks a lead, a Mono a lead it did not start, a lead another lead's
    // worker, a worker anyone), or a direct child is refused.
    #[test]
    fn only_a_direct_child_can_be_asked() {
        let giga = party(Level::Giga, "giga-chat");
        let mono = party(Level::Mono, "mono-chat");
        let lead = party(Level::Lead, "lead");
        let worker = party(worker_of("lead"), "worker");
        let others = party(worker_of("other"), "w2");
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
        let mono = party(Level::Mono, "mono-chat");
        let lead = party(Level::Lead, "lead");
        let worker = party(worker_of("lead"), "worker");
        let giga = party(Level::Giga, "g");
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

    // Defect: an ack of another session's ask, of an acked ask or of a
    // dropped ask is told the wrong reason.
    #[test]
    fn an_ack_that_changes_no_row_names_why() {
        let me = SessionId::parse("worker").unwrap();
        let open = |child: &str| AskState {
            state: State::Open,
            child_session: SessionId::parse(child).unwrap(),
        };
        assert_eq!(decide_ack(&open("w2"), &me), Err(AckRefused::NotYours));
        let with = |state| AskState {
            state,
            ..open("worker")
        };
        assert_eq!(
            decide_ack(&with(State::Acked), &me),
            Err(AckRefused::AlreadyAcked)
        );
        assert_eq!(
            decide_ack(&with(State::Dropped), &me),
            Err(AckRefused::NoOpenAsk)
        );
        assert_eq!(decide_ack(&open("worker"), &me), Ok(()));
    }
}
