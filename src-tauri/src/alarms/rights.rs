//! Design section 7, who may act on an alarm (lead ruling 2026-11-08,
//! rights accumulate): every recipient told so far and every provisioner
//! above it acts, each on the one just below it on the chain.

use super::provisioner::Provisioner;
use crate::ask_gate::Actor;

/// The caller's index on an episode's chain: `chain[0]` is the stalled
/// agent, `chain[k]` the recipient of level k. The agent and strangers
/// have none.
pub(crate) fn standing(chain: &[Provisioner], caller: &Actor) -> Option<usize> {
    let caller = Provisioner::Agent(caller.clone());
    chain
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, link)| (*link == caller).then_some(index))
}

/// The agent the caller at `index` acts on: the one just below it.
pub(crate) fn target_of(chain: &[Provisioner], index: usize) -> Option<&Actor> {
    match chain.get(index.checked_sub(1)?)? {
        Provisioner::Agent(actor) => Some(actor),
        Provisioner::Tyler => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    Read,
    Answer,
    Steer,
    Nudge,
    Dismiss,
}

/// Why the caller at an index may not do an act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denied {
    NotOnChain,
    MovedUp,
    AnswerViaRecipient,
}

/// Whom the caller at `index` acts on with `how` while the episode is at
/// `level` (lead ruling, rights accumulate): the one just below it. The
/// direct provisioner (index 1) keeps answer, steer and nudge on the agent
/// at every level; only it answers a dialog.
pub(crate) fn authorize(
    chain: &[Provisioner],
    level: usize,
    index: usize,
    how: How,
) -> Result<&Actor, Denied> {
    let target = target_of(chain, index).ok_or(Denied::NotOnChain)?;
    match how {
        How::Answer if index != 1 => Err(Denied::AnswerViaRecipient),
        How::Dismiss if index < level => Err(Denied::MovedUp),
        _ => Ok(target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mono_messages::{MonoId, SessionId};

    fn session(id: &str) -> Actor {
        Actor::Session(SessionId::parse(id).unwrap())
    }

    fn mono(id: &str) -> Actor {
        Actor::Mono(MonoId::parse(id).unwrap())
    }

    // Defect: a recipient the alarm went past loses its acts (lead ruling:
    // rights accumulate), the stalled agent or a stranger acts, someone
    // but the direct provisioner answers the dialog, a lower recipient
    // dismisses the level above it, or a caller acts on anyone but the one
    // just below it.
    #[test]
    fn every_recipient_so_far_acts_one_level_down_and_only_the_provisioner_answers() {
        use Denied::{AnswerViaRecipient as Avr, MovedUp, NotOnChain};
        let (worker, lead, pr_mono, giga, stranger) = (
            session("worker"),
            session("lead"),
            mono("pr-mono"),
            mono("general"),
            mono("other"),
        );
        let chain = vec![
            Provisioner::Agent(worker.clone()),
            Provisioner::Agent(lead.clone()),
            Provisioner::Agent(pr_mono.clone()),
            Provisioner::Agent(giga.clone()),
            Provisioner::Tyler,
        ];
        let acts = [How::Read, How::Answer, How::Steer, How::Nudge, How::Dismiss];
        type Acts<'a> = [Result<&'a Actor, Denied>; 5];
        let none: Acts = [Err(NotOnChain); 5];
        // Rows: caller; per episode level 1..=4 (4 is Tyler's), the result
        // of read, answer, steer, nudge and dismiss.
        let w = Ok(&worker);
        let l = Ok(&lead);
        let m = Ok(&pr_mono);
        let table: [(&Actor, [Acts; 4]); 5] = [
            (&worker, [none; 4]),
            (
                &lead,
                [
                    [w, w, w, w, w],
                    [w, w, w, w, Err(MovedUp)],
                    [w, w, w, w, Err(MovedUp)],
                    [w, w, w, w, Err(MovedUp)],
                ],
            ),
            (
                &pr_mono,
                [
                    [l, Err(Avr), l, l, l],
                    [l, Err(Avr), l, l, l],
                    [l, Err(Avr), l, l, Err(MovedUp)],
                    [l, Err(Avr), l, l, Err(MovedUp)],
                ],
            ),
            (
                &giga,
                [
                    [m, Err(Avr), m, m, m],
                    [m, Err(Avr), m, m, m],
                    [m, Err(Avr), m, m, m],
                    [m, Err(Avr), m, m, Err(MovedUp)],
                ],
            ),
            (&stranger, [none; 4]),
        ];
        for (caller, levels) in table {
            for (level, row) in (1..=4).zip(levels) {
                for (how, expected) in acts.into_iter().zip(row) {
                    let got = standing(&chain, caller)
                        .ok_or(NotOnChain)
                        .and_then(|index| authorize(&chain, level, index, how));
                    assert_eq!(got, expected, "{caller} {how:?} at level {level}");
                }
            }
        }
    }
}
