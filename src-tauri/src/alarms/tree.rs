//! One pass's tree of who started or asked whom (lead cascade ruling, N6).
//! One stall is one episode, at the stalled agent: an agent above it waits,
//! and is told by that episode's alarm and escalation.
//! - A running agent keeps everyone above it WAITING, over every edge.
//! - An agent with open work of its own, or a result to handle, is held:
//!   the agents that started it wait on it, and so does an asker outside
//!   its cycle. Inside a cycle (a lead asks the Mono that asked it) only a
//!   start waits, so the members do not wait on each other for ever.
//! - Only a plain idle child is a result, and only to the agent that
//!   started it (an asker gets no completion notice).

use std::collections::{HashMap, HashSet};

use super::state::{self, Child, ChildState, Facts, Status, Thresholds, Turn};

/// A parent's edge to a child session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edge {
    pub child: String,
    /// The parent started the child, so the child's result is its to
    /// handle; else the parent only asked it.
    pub started: bool,
}

pub(crate) type Edges = HashMap<String, Vec<Edge>>;

struct Tree<'a> {
    edges: &'a Edges,
    /// child -> (parent, started).
    parents: HashMap<&'a str, Vec<(&'a str, bool)>>,
}

impl<'a> Tree<'a> {
    fn new(edges: &'a Edges) -> Self {
        let mut parents: HashMap<&str, Vec<(&str, bool)>> = HashMap::new();
        for (parent, children) in edges {
            for edge in children {
                parents
                    .entry(edge.child.as_str())
                    .or_default()
                    .push((parent.as_str(), edge.started));
            }
        }
        Self { edges, parents }
    }

    fn children(&self, actor: &str) -> impl Iterator<Item = &'a Edge> {
        self.edges.get(actor).into_iter().flatten()
    }

    /// Whether `to` is reachable from `from` over the edges.
    fn reaches(&self, from: &str, to: &str) -> bool {
        let mut seen = HashSet::new();
        let mut next = vec![from];
        while let Some(actor) = next.pop() {
            if actor == to {
                return true;
            }
            if seen.insert(actor) {
                next.extend(self.children(actor).map(|edge| edge.child.as_str()));
            }
        }
        false
    }

    /// Whether `parent` waits on a held `child`.
    fn waits(&self, parent: &str, child: &str, started: bool) -> bool {
        started || !self.reaches(child, parent)
    }

    /// `seeds` and every agent above them over the edges `follow` keeps.
    fn climb(
        &self,
        seeds: impl IntoIterator<Item = &'a str>,
        follow: impl Fn(&str, &str, bool) -> bool,
    ) -> HashSet<&'a str> {
        let mut found = HashSet::new();
        let mut next: Vec<&str> = seeds.into_iter().collect();
        while let Some(actor) = next.pop() {
            if found.insert(actor) {
                for &(parent, started) in self.parents.get(actor).into_iter().flatten() {
                    if follow(parent, actor, started) {
                        next.push(parent);
                    }
                }
            }
        }
        found
    }

    /// The last turn end of `from` and everything below it.
    fn last_end(&self, from: &str, ends: &HashMap<&str, i64>) -> Option<i64> {
        let mut seen = HashSet::new();
        let mut next = vec![from];
        let mut latest = None;
        while let Some(actor) = next.pop() {
            if seen.insert(actor) {
                latest = latest.max(ends.get(actor).copied());
                next.extend(self.children(actor).map(|edge| edge.child.as_str()));
            }
        }
        latest
    }
}

fn ended_at(facts: &Facts) -> Option<i64> {
    match facts.turn {
        Turn::Idle { ended_at, .. } => Some(ended_at),
        Turn::Running { .. } => None,
    }
}

/// One agent of a pass: its actor, its facts and its thresholds.
pub(crate) type Agent = (String, Facts, Thresholds);

/// Fills in each agent's children and `settled_at`; its `Facts` come in
/// with none.
pub(crate) fn judge(agents: &mut [Agent], edges: &Edges, now: i64) {
    let filled = branches(agents, edges, now);
    for ((_, facts, _), (children, settled_at)) in agents.iter_mut().zip(filled) {
        facts.children = children;
        facts.settled_at = settled_at;
    }
}

/// Each agent's children as it sees them, and the last turn end below its
/// settled ones.
fn branches(agents: &[Agent], edges: &Edges, now: i64) -> Vec<(Vec<Child>, Option<i64>)> {
    let tree = Tree::new(edges);
    let ends: HashMap<&str, i64> = agents
        .iter()
        .filter_map(|(actor, facts, _)| Some((actor.as_str(), ended_at(facts)?)))
        .collect();
    let running = agents
        .iter()
        .filter(|(_, facts, _)| matches!(facts.turn, Turn::Running { .. }))
        .map(|(actor, _, _)| actor.as_str());
    let busy = tree.climb(running, |_, _, _| true);
    let own = agents
        .iter()
        .filter(|(_, facts, limits)| {
            !matches!(
                (state::status(facts, now, *limits), facts.turn),
                (Status::Idle | Status::Gone, Turn::Idle { .. })
            )
        })
        .map(|(actor, _, _)| actor.as_str());
    let waits = |parent: &str, child: &str, started: bool| tree.waits(parent, child, started);
    let mut held = tree.climb(own, waits);
    held.extend(busy.iter().copied());
    let results: Vec<&str> = agents
        .iter()
        .map(|(actor, _, _)| actor.as_str())
        .filter(|actor| !held.contains(actor))
        .filter(|actor| {
            ends.get(actor).is_some_and(|&ended| {
                tree.children(actor).any(|edge| {
                    edge.started
                        && !held.contains(edge.child.as_str())
                        && ends
                            .get(edge.child.as_str())
                            .is_some_and(|&end| end > ended)
                })
            })
        })
        .collect();
    held.extend(tree.climb(results, waits));

    agents
        .iter()
        .map(|(actor, _, _)| {
            let mut settled_at = None;
            let children = tree
                .children(actor)
                .filter_map(|edge| {
                    let child = edge.child.as_str();
                    let unsettled = busy.contains(child)
                        || (held.contains(child) && tree.waits(actor, child, edge.started));
                    let state = if unsettled {
                        ChildState::Unsettled
                    } else {
                        settled_at = settled_at.max(tree.last_end(child, &ends));
                        ChildState::Settled {
                            ended_at: *ends.get(child)?,
                        }
                    };
                    Some(Child {
                        actor: edge.child.clone(),
                        started: edge.started,
                        state,
                    })
                })
                .collect();
            (children, settled_at)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarms::state::{next_state, Cause, OpenWork, Progress, Transition};

    const MIN: i64 = 60_000;
    const LIMITS: Thresholds = Thresholds {
        stall_ms: 15 * MIN,
        dialog_ms: 3 * MIN,
        wake_ms: 2 * MIN,
    };

    fn idle(ended_at: i64) -> Facts {
        Facts {
            gone: false,
            turn: Turn::Idle {
                started_at: Some(ended_at - MIN),
                ended_at,
            },
            dialog: None,
            open_work: Vec::new(),
            children: Vec::new(),
            settled_at: None,
            wake_pending_since: None,
            episode: None,
        }
    }

    fn asked(facts: Facts, id: &str) -> Facts {
        Facts {
            open_work: vec![OpenWork::Ask { id: id.into() }],
            ..facts
        }
    }

    fn edges(list: &[(&str, &str, bool)]) -> Edges {
        let mut edges = Edges::new();
        for (parent, child, started) in list {
            edges.entry((*parent).into()).or_default().push(Edge {
                child: (*child).into(),
                started: *started,
            });
        }
        edges
    }

    /// Who opens an episode at `now`.
    fn opening(agents: Vec<(&str, Facts)>, edges: &Edges, now: i64) -> Vec<(String, Cause)> {
        let mut agents: Vec<Agent> = agents
            .into_iter()
            .map(|(actor, facts)| (actor.to_string(), facts, LIMITS))
            .collect();
        judge(&mut agents, edges, now);
        let mut opened: Vec<(String, Cause)> = agents
            .into_iter()
            .filter_map(|(actor, facts, _)| match next_state(&facts, now, LIMITS) {
                Transition::Open { cause, .. } => Some((actor, cause)),
                _ => None,
            })
            .collect();
        opened.sort_by_key(|(actor, cause)| (actor.clone(), cause.as_str()));
        opened
    }

    /// Giga -> Mono (an ask) -> lead (started, and asked) -> worker
    /// (started). Each above the worker holds the ask of the one above it.
    fn chain() -> Edges {
        edges(&[
            ("mono:giga", "mono:m", false),
            ("mono:m", "session:lead", true),
            ("mono:m", "session:lead", false),
            ("session:lead", "session:worker", true),
        ])
    }

    // Defect (S1 r2 M1, lead cascade ruling): one stall opens an episode at
    // every ancestor in the same pass, up to Tyler: an ask counts as a
    // child result, a grandchild's end reaches the grandparent, or the
    // ancestors' own asks alarm as soon as the branch below goes quiet.
    #[test]
    fn a_stalled_worker_is_the_one_episode_in_its_chain() {
        let start = 100 * MIN;
        let worker_end = start + 60 * MIN;
        let agents = || {
            vec![
                ("mono:giga", asked(idle(start), "ask-general")),
                ("mono:m", asked(idle(start + MIN), "ask-giga")),
                ("session:lead", asked(idle(start + 2 * MIN), "ask-m")),
                (
                    "session:worker",
                    Facts {
                        open_work: vec![OpenWork::Todo],
                        ..idle(worker_end)
                    },
                ),
            ]
        };
        for now in [worker_end + 15 * MIN, worker_end + 15 * MIN + 5_000] {
            assert_eq!(
                opening(agents(), &chain(), now),
                vec![("session:worker".to_string(), Cause::IdleOpenWork)],
                "at {now}"
            );
        }
    }

    // Defect (S1 r2 M1(b), N6): a worker's plain end alarms its lead and
    // the lead's Mono at once; the Mono never had a notice of it.
    #[test]
    fn a_plain_result_is_open_work_only_for_the_agent_that_started_it() {
        let start = 100 * MIN;
        let worker_end = start + 60 * MIN;
        let agents = vec![
            ("mono:giga", asked(idle(start), "ask-general")),
            ("mono:m", asked(idle(start + MIN), "ask-giga")),
            ("session:lead", idle(start + 2 * MIN)),
            ("session:worker", idle(worker_end)),
        ];
        assert_eq!(
            opening(agents, &chain(), worker_end + 15 * MIN),
            vec![("session:lead".to_string(), Cause::IdleOpenWork)]
        );
    }

    // Defect (S1 r2 M1(a)): an ask recipient's turn end is a child result
    // of the asker, which gets no completion notice for it, so a Mono
    // holding a Giga's open ask alarms the Giga's provisioner too.
    #[test]
    fn an_ask_recipient_is_never_a_result_of_its_asker() {
        let start = 100 * MIN;
        let asks = edges(&[("mono:giga", "mono:m", false)]);
        let quiet = vec![
            ("mono:giga", idle(start)),
            ("mono:m", idle(start + 10 * MIN)),
        ];
        assert_eq!(opening(quiet, &asks, start + 600 * MIN), Vec::new());
        let unanswered = vec![
            ("mono:giga", idle(start)),
            ("mono:m", asked(idle(start + 10 * MIN), "ask-giga")),
        ];
        assert_eq!(
            opening(unanswered, &asks, start + 25 * MIN),
            vec![("mono:m".to_string(), Cause::IdleOpenWork)]
        );
    }

    // Defect (lead cascade rule 3): an agent whose own open work waited
    // while its branch ran alarms as soon as the branch goes quiet, or a
    // settled branch below creates open work for it.
    #[test]
    fn a_settled_branch_restarts_the_agents_own_clock() {
        let start = 100 * MIN;
        let branch_end = start + 60 * MIN;
        let asks = edges(&[("session:lead", "session:helper", false)]);
        let agents = || {
            vec![
                ("session:lead", asked(idle(start), "ask-m")),
                ("session:helper", idle(branch_end)),
            ]
        };
        assert_eq!(
            opening(agents(), &asks, branch_end + 15 * MIN - 1),
            Vec::new()
        );
        assert_eq!(
            opening(agents(), &asks, branch_end + 15 * MIN),
            vec![("session:lead".to_string(), Cause::IdleOpenWork)]
        );
        let no_work = vec![
            ("session:lead", idle(start)),
            ("session:helper", idle(branch_end)),
        ];
        assert_eq!(opening(no_work, &asks, branch_end + 600 * MIN), Vec::new());
    }

    // Defect: a lead that asked its Mono a question while holding the
    // Mono's ask, and the Mono, wait on each other, so neither alarms.
    #[test]
    fn a_lead_and_its_mono_asking_each_other_alarm_once() {
        let start = 100 * MIN;
        let mut cycle = chain();
        cycle.entry("session:lead".into()).or_default().push(Edge {
            child: "mono:m".into(),
            started: false,
        });
        let agents = vec![
            ("mono:giga", asked(idle(start), "ask-general")),
            ("mono:m", asked(idle(start + MIN), "ask-lead")),
            ("session:lead", asked(idle(start + 2 * MIN), "ask-m")),
        ];
        assert_eq!(
            opening(agents, &cycle, start + 2 * MIN + 15 * MIN),
            vec![("session:lead".to_string(), Cause::IdleOpenWork)],
            "the lead's alarm wakes the Mono it waits on"
        );
    }

    // Defect: a running descendant two levels down does not keep the
    // agents above it WAITING, or a cycle in the edges never ends.
    #[test]
    fn a_running_descendant_keeps_its_ancestors_waiting_and_a_cycle_ends() {
        let start = 100 * MIN;
        let running = Facts {
            turn: Turn::Running {
                started_at: start,
                progress: Progress::Untapped,
            },
            ..idle(start)
        };
        let mut cycle = chain();
        cycle
            .entry("session:worker".into())
            .or_default()
            .push(Edge {
                child: "mono:giga".into(),
                started: false,
            });
        let agents = vec![
            ("mono:giga", asked(idle(start), "ask-general")),
            ("mono:m", asked(idle(start), "ask-giga")),
            ("session:lead", asked(idle(start), "ask-m")),
            ("session:worker", running),
        ];
        assert_eq!(opening(agents, &cycle, start + 600 * MIN), Vec::new());
    }
}
