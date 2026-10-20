//! The ask rules (Change R plan v2, R5), with no IO. `asks.rs` loads the
//! facts from saved rows and writes what these decide. R5 (2/2) adds ack.

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

/// The ask row a call writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAsk {
    pub parent_level: u8,
    pub child_level: u8,
    /// `None` exactly when the parent is a Giga: a level 1 parent sets no clock.
    pub due_active_ms: Option<i64>,
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

/// An ask from `parent` (`None`: the caller has no saved row) to `child`
/// (`None`: no saved Mono or session), at active time `active_ms`.
pub fn decide_ask(
    parent: Option<&Party>,
    child: Option<&Party>,
    started_by_parent: bool,
    due_minutes: Option<u32>,
    active_ms: i64,
) -> Result<NewAsk, AskRefused> {
    let (Some(parent), Some(child)) = (parent, child) else {
        return Err(AskRefused::NotDirectChild);
    };
    if !is_direct_child(parent, child, started_by_parent) {
        return Err(AskRefused::NotDirectChild);
    }
    let minutes = i64::from(due_minutes.unwrap_or(DEFAULT_DUE_MINUTES));
    Ok(NewAsk {
        parent_level: parent.level.rank(),
        child_level: child.level.rank(),
        due_active_ms: (parent.level != Level::Giga).then_some(active_ms + minutes * 60_000),
    })
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
                decide_ask(Some(parent), Some(child), started, None, 0).is_ok(),
                direct,
                "{parent:?} -> {child:?}, started {started}"
            );
        }
        assert_eq!(
            decide_ask(None, Some(&mono), false, None, 0),
            Err(AskRefused::NotDirectChild)
        );
        assert_eq!(
            decide_ask(Some(&giga), None, false, None, 0),
            Err(AskRefused::NotDirectChild)
        );
    }

    // Defect: a Giga's ask gets a deadline, or a lower parent's deadline is
    // not the active time now plus its budget.
    #[test]
    fn the_due_time_is_active_time_plus_the_budget_and_none_for_a_giga() {
        let mono = party(Level::Mono, "mono-chat");
        let lead = party(Level::Lead, "lead");
        let worker = party(worker_of("lead"), "worker");
        assert_eq!(
            decide_ask(
                Some(&party(Level::Giga, "g")),
                Some(&mono),
                false,
                Some(30),
                5_000
            ),
            Ok(NewAsk {
                parent_level: 1,
                child_level: 2,
                due_active_ms: None
            })
        );
        assert_eq!(
            decide_ask(Some(&mono), Some(&lead), true, None, 5_000),
            Ok(NewAsk {
                parent_level: 2,
                child_level: 3,
                due_active_ms: Some(5_000 + 60 * 60_000)
            })
        );
        assert_eq!(
            decide_ask(Some(&lead), Some(&worker), false, Some(10), 7),
            Ok(NewAsk {
                parent_level: 3,
                child_level: 4,
                due_active_ms: Some(7 + 10 * 60_000)
            })
        );
    }
}
