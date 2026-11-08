//! Design section 2: the state of one agent, derived from its facts at each
//! detector pass and never stored. `next_state` says what the pass does to
//! the agent's one open episode. Every time is active-clock time.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

/// The thresholds of design section 3, in active milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Thresholds {
    /// Idle with open work, or a running turn with no progress.
    pub stall_ms: i64,
    pub dialog_ms: i64,
    pub wake_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn {
    /// `started_at` is the last turn's start, `None` before any turn.
    Idle {
        started_at: Option<i64>,
        ended_at: i64,
    },
    Running {
        started_at: i64,
        progress: Progress,
    },
}

/// What the stdio taps saw of a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Progress {
    /// No tap reads this harness, so a running turn counts as working.
    Untapped,
    Tapped {
        last_at: i64,
        tool_running: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DialogKind {
    Permission,
    Question,
    /// A plan waits for approval from its turn's end until the next turn.
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dialog {
    pub kind: DialogKind,
    pub since: i64,
}

/// Design section 4. A child's unhandled result is derived from `children`;
/// an undelivered notice, message or reminder is a wake (`wake_pending_since`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OpenWork {
    Ask { id: String },
    ChildResult { child: String },
    Todo,
}

/// What waits for the agent's provider to take it (section 4). An episode
/// lists them beside its open work; a notice or queued item counts toward
/// one episode only (lead N5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Wake {
    Message,
    Reminder,
    /// A Mono completion result not yet in its chat.
    Notice {
        id: String,
    },
    /// An item the chat's saved queue holds that no turn took.
    Queued {
        id: String,
    },
}

/// A child agent as its parent sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildState {
    /// Its turn runs, or a turn of one of its own descendants does.
    Busy,
    /// Its branch is quiet: `ended_at` is the last turn end in it.
    Idle { ended_at: i64 },
}

/// A session the agent started or gave work to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Child {
    pub actor: String,
    pub state: ChildState,
}

/// The agents that run a turn or have a descendant that does: WAITING is
/// transitive (review M1). `parents` maps an agent to the agents that
/// started it or gave it work; an agent seen twice ends a cycle.
pub(crate) fn busy<'a>(
    running: impl IntoIterator<Item = &'a str>,
    parents: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut busy = HashSet::new();
    let mut next: Vec<String> = running.into_iter().map(str::to_string).collect();
    while let Some(actor) = next.pop() {
        if busy.insert(actor.clone()) {
            next.extend(parents.get(&actor).into_iter().flatten().cloned());
        }
    }
    busy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cause {
    IdleOpenWork,
    Dialog,
    Hung,
    WakeMissed,
}

impl Cause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Cause::IdleOpenWork => "idle_open_work",
            Cause::Dialog => "dialog",
            Cause::Hung => "hung",
            Cause::WakeMissed => "wake_missed",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        [
            Cause::IdleOpenWork,
            Cause::Dialog,
            Cause::Hung,
            Cause::WakeMissed,
        ]
        .into_iter()
        .find(|cause| cause.as_str() == text)
    }
}

/// The agent's open episode, as its row holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Episode {
    pub cause: Cause,
    pub stalled_since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The session was archived or deleted, or the Mono deleted.
    pub gone: bool,
    pub turn: Turn,
    pub dialog: Option<Dialog>,
    /// The agent's own open work; a child's result is added from `children`.
    pub open_work: Vec<OpenWork>,
    pub children: Vec<Child>,
    /// When a wake for the agent (message, reminder, notice) was first seen
    /// waiting for its provider.
    pub wake_pending_since: Option<i64>,
    pub episode: Option<Episode>,
}

/// The states of design section 2 before an episode opens. `since` is when
/// the state's threshold started counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Gone,
    Working,
    Idle,
    Waiting,
    IdleOpenWork { since: i64 },
    BlockedOnDialog { since: i64 },
    Hung { since: i64 },
    WakeMissed { since: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReason {
    Progress,
    WorkCleared,
    AgentGone,
}

impl CloseReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CloseReason::Progress => "progress",
            CloseReason::WorkCleared => "work_cleared",
            CloseReason::AgentGone => "agent_gone",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transition {
    Stay,
    Open { cause: Cause, stalled_since: i64 },
    Close(CloseReason),
}

/// When each child's result the agent has not handled came: an idle child
/// that ended after the agent's last turn ended, so no turn of the agent
/// has seen it (lead N4). One that ended during the agent's turn reached
/// it as a completion notice.
fn unhandled(facts: &Facts) -> impl Iterator<Item = (&Child, i64)> {
    let ended = match facts.turn {
        Turn::Idle { ended_at, .. } => Some(ended_at),
        Turn::Running { .. } => None,
    };
    facts
        .children
        .iter()
        .filter_map(move |child| match (child.state, ended) {
            (ChildState::Idle { ended_at }, Some(ended)) if ended_at > ended => {
                Some((child, ended_at))
            }
            (ChildState::Idle { .. } | ChildState::Busy, _) => None,
        })
}

/// When `actor`'s branch went quiet: the last turn end of it and every
/// descendant that has one (`ended`). A lead that waited on its worker is
/// done from the worker's end, not from its own earlier one.
pub(crate) fn settled_at(
    actor: &str,
    children: &HashMap<String, Vec<String>>,
    ended: &HashMap<String, i64>,
) -> Option<i64> {
    let mut seen = HashSet::new();
    let mut next = vec![actor.to_string()];
    let mut latest = None;
    while let Some(actor) = next.pop() {
        if seen.insert(actor.clone()) {
            latest = latest.max(ended.get(&actor).copied());
            next.extend(children.get(&actor).into_iter().flatten().cloned());
        }
    }
    latest
}

/// The open work of `facts`: its own, then each child result it has not
/// handled.
pub(crate) fn open_work(facts: &Facts) -> Vec<OpenWork> {
    let children = unhandled(facts).map(|(child, _)| OpenWork::ChildResult {
        child: child.actor.clone(),
    });
    facts.open_work.iter().cloned().chain(children).collect()
}

/// The dialog that blocks the agent now. A permission or question lives
/// only inside the running turn that asked it; a plan only after the turn
/// that proposed it ended.
fn blocking_dialog(facts: &Facts) -> Option<Dialog> {
    let dialog = facts.dialog?;
    match (dialog.kind, facts.turn) {
        (DialogKind::Permission | DialogKind::Question, Turn::Running { started_at, .. })
            if dialog.since >= started_at =>
        {
            Some(dialog)
        }
        (
            DialogKind::Plan,
            Turn::Idle {
                started_at,
                ended_at,
            },
        ) if started_at.is_none_or(|started| dialog.since >= started) => Some(Dialog {
            since: dialog.since.max(ended_at),
            ..dialog
        }),
        _ => None,
    }
}

pub(crate) fn status(facts: &Facts, now: i64, limits: Thresholds) -> Status {
    if facts.gone {
        return Status::Gone;
    }
    if let Some(dialog) = blocking_dialog(facts) {
        if let (DialogKind::Plan, Some(since)) = (dialog.kind, wake_missed(facts, now, limits)) {
            return Status::WakeMissed { since };
        }
        return Status::BlockedOnDialog {
            since: dialog.since,
        };
    }
    match facts.turn {
        Turn::Running {
            progress: Progress::Untapped,
            ..
        }
        | Turn::Running {
            progress: Progress::Tapped {
                tool_running: true, ..
            },
            ..
        } => Status::Working,
        Turn::Running {
            started_at,
            progress:
                Progress::Tapped {
                    last_at,
                    tool_running: false,
                },
        } => {
            let since = last_at.max(started_at);
            if now - since >= limits.stall_ms {
                Status::Hung { since }
            } else {
                Status::Working
            }
        }
        Turn::Idle { ended_at, .. } => {
            if let Some(since) = wake_missed(facts, now, limits) {
                return Status::WakeMissed { since };
            }
            if facts
                .children
                .iter()
                .any(|child| child.state == ChildState::Busy)
            {
                return Status::Waiting;
            }
            if open_work(facts).is_empty() {
                return Status::Idle;
            }
            Status::IdleOpenWork {
                since: unhandled(facts).map(|(_, at)| at).fold(ended_at, i64::max),
            }
        }
    }
}

/// When an idle agent's wake has waited past its threshold: counted from
/// when it was first seen, or the turn end when that came later.
fn wake_missed(facts: &Facts, now: i64, limits: Thresholds) -> Option<i64> {
    let Turn::Idle { ended_at, .. } = facts.turn else {
        return None;
    };
    let since = facts.wake_pending_since?.max(ended_at);
    (now - since >= limits.wake_ms).then_some(since)
}

impl Status {
    /// The stall this state is, once its threshold passed.
    pub(crate) fn stall(self, now: i64, limits: Thresholds) -> Option<(Cause, i64)> {
        match self {
            Status::Gone | Status::Working | Status::Idle | Status::Waiting => None,
            Status::IdleOpenWork { since } => {
                (now - since >= limits.stall_ms).then_some((Cause::IdleOpenWork, since))
            }
            Status::BlockedOnDialog { since } => {
                (now - since >= limits.dialog_ms).then_some((Cause::Dialog, since))
            }
            Status::Hung { since } => Some((Cause::Hung, since)),
            Status::WakeMissed { since } => Some((Cause::WakeMissed, since)),
        }
    }
}

/// Whether the agent moved since its episode's stall began: a turn started
/// or ended after it, a hung turn printed or ran a tool, or the dialog it
/// was stuck on was answered or withdrawn.
fn progressed(facts: &Facts, episode: Episode) -> bool {
    let since = episode.stalled_since;
    let boundary = match facts.turn {
        Turn::Idle {
            started_at,
            ended_at,
        } => ended_at > since || started_at.is_some_and(|started| started > since),
        Turn::Running { started_at, .. } => started_at > since,
    };
    boundary
        || match episode.cause {
            Cause::Hung => matches!(
                facts.turn,
                Turn::Running {
                    progress: Progress::Tapped { last_at, tool_running },
                    ..
                } if tool_running || last_at > since
            ),
            Cause::Dialog => blocking_dialog(facts).is_none_or(|dialog| dialog.since != since),
            Cause::IdleOpenWork | Cause::WakeMissed => false,
        }
}

/// What this pass does to the agent's episode. A still-stalled agent keeps
/// its episode and its first cause.
pub(crate) fn next_state(facts: &Facts, now: i64, limits: Thresholds) -> Transition {
    let status = status(facts, now, limits);
    let stall = status.stall(now, limits);
    match (facts.episode, stall) {
        (None, None) => Transition::Stay,
        (None, Some((cause, stalled_since))) => Transition::Open {
            cause,
            stalled_since,
        },
        (Some(_), _) if status == Status::Gone => Transition::Close(CloseReason::AgentGone),
        (Some(episode), _) if progressed(facts, episode) => {
            Transition::Close(CloseReason::Progress)
        }
        (Some(_), Some(_)) => Transition::Stay,
        (Some(_), None) => Transition::Close(CloseReason::WorkCleared),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const LIMITS: Thresholds = Thresholds {
        stall_ms: 15 * MIN,
        dialog_ms: 3 * MIN,
        wake_ms: 2 * MIN,
    };
    /// The agent's last turn ended here.
    const ENDED: i64 = 100 * MIN;

    fn idle() -> Facts {
        Facts {
            gone: false,
            turn: Turn::Idle {
                started_at: Some(ENDED - 10 * MIN),
                ended_at: ENDED,
            },
            dialog: None,
            open_work: Vec::new(),
            children: Vec::new(),
            wake_pending_since: None,
            episode: None,
        }
    }

    fn with_ask(facts: Facts) -> Facts {
        Facts {
            open_work: vec![OpenWork::Ask {
                id: "m_aaaaaaaaaaaa".into(),
            }],
            ..facts
        }
    }

    fn running(started_at: i64, last_at: i64, tool_running: bool) -> Facts {
        Facts {
            turn: Turn::Running {
                started_at,
                progress: Progress::Tapped {
                    last_at,
                    tool_running,
                },
            },
            ..idle()
        }
    }

    fn child(state: ChildState) -> Child {
        Child {
            actor: "session:worker".into(),
            state,
        }
    }

    fn after(facts: Facts, cause: Cause, stalled_since: i64) -> Facts {
        Facts {
            episode: Some(Episode {
                cause,
                stalled_since,
            }),
            ..facts
        }
    }

    // Defect: a busy agent (output within N, or a tool still running) is
    // counted as hung.
    #[test]
    fn a_running_turn_with_recent_output_or_a_running_tool_is_working() {
        let start = ENDED + MIN;
        let now = start + 20 * MIN;
        assert_eq!(
            status(&running(start, now - MIN, false), now, LIMITS),
            Status::Working
        );
        assert_eq!(
            status(&running(start, start, true), now, LIMITS),
            Status::Working,
            "a long tool is work"
        );
        let untapped = Facts {
            turn: Turn::Running {
                started_at: start,
                progress: Progress::Untapped,
            },
            ..idle()
        };
        assert_eq!(next_state(&untapped, now, LIMITS), Transition::Stay);
    }

    // Defect: an agent with nothing open alarms its provisioner.
    #[test]
    fn an_idle_agent_with_no_open_work_never_stalls() {
        assert_eq!(status(&idle(), ENDED + 600 * MIN, LIMITS), Status::Idle);
        assert_eq!(
            next_state(&idle(), ENDED + 600 * MIN, LIMITS),
            Transition::Stay
        );
    }

    // Defect: a lead waiting on its running worker alarms at N.
    #[test]
    fn an_idle_agent_with_a_busy_child_is_waiting_even_with_its_own_ask() {
        let facts = Facts {
            children: vec![child(ChildState::Busy)],
            ..with_ask(idle())
        };
        assert_eq!(status(&facts, ENDED + 60 * MIN, LIMITS), Status::Waiting);
        assert_eq!(
            next_state(&facts, ENDED + 60 * MIN, LIMITS),
            Transition::Stay
        );
    }

    // Defect: idle with an open ask alarms before N, or never.
    #[test]
    fn idle_open_work_stalls_at_n_after_the_turn_ended() {
        let facts = with_ask(idle());
        assert_eq!(
            next_state(&facts, ENDED + 15 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&facts, ENDED + 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::IdleOpenWork,
                stalled_since: ENDED
            }
        );
    }

    // Defect: a worker's result the lead never saw is not open work, or the
    // lead's clock starts at its own turn end, before the worker finished;
    // or (lead N4) a result that came during the lead's last turn, as a
    // completion notice that turn took, alarms at N.
    #[test]
    fn a_child_that_ended_after_the_agents_turn_ended_is_open_work_from_its_end() {
        let done = ENDED + 5 * MIN;
        let facts = Facts {
            children: vec![child(ChildState::Idle { ended_at: done })],
            ..idle()
        };
        assert_eq!(
            open_work(&facts),
            vec![OpenWork::ChildResult {
                child: "session:worker".into()
            }]
        );
        assert_eq!(
            next_state(&facts, done + 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::IdleOpenWork,
                stalled_since: done
            }
        );
        for (ended_at, when) in [
            (ENDED - 20 * MIN, "before the turn"),
            (ENDED - MIN, "during the turn"),
            (ENDED, "with the turn"),
        ] {
            let seen = Facts {
                children: vec![child(ChildState::Idle { ended_at })],
                ..idle()
            };
            assert_eq!(open_work(&seen), Vec::new(), "ended {when}");
            assert_eq!(
                next_state(&seen, ENDED + 600 * MIN, LIMITS),
                Transition::Stay,
                "ended {when}"
            );
        }
    }

    // Defect (review M1): WAITING looks only at direct children, so a Mono
    // whose idle lead waits on a running worker alarms at the lead's end
    // + N; or a cycle in the started-by graph never ends.
    #[test]
    fn an_agent_is_busy_while_any_descendant_runs() {
        let parents = HashMap::from([
            (
                "session:worker".to_string(),
                vec!["session:lead".to_string()],
            ),
            ("session:lead".to_string(), vec!["mono:m".to_string()]),
            ("mono:m".to_string(), vec!["mono:giga".to_string()]),
            ("mono:giga".to_string(), vec!["mono:m".to_string()]),
            ("session:other".to_string(), vec!["mono:x".to_string()]),
        ]);
        let busy = busy(["session:worker"], &parents);
        let mut busy: Vec<&str> = busy.iter().map(String::as_str).collect();
        busy.sort_unstable();
        assert_eq!(
            busy,
            ["mono:giga", "mono:m", "session:lead", "session:worker"]
        );
    }

    // Defect: once a lead's worker ends, the Mono's clock on the lead's
    // result starts at the lead's own end, before it waited on the worker,
    // so the Mono alarms at once.
    #[test]
    fn a_branch_settles_at_its_last_turn_end() {
        let children = HashMap::from([
            (
                "session:lead".to_string(),
                vec!["session:worker".to_string()],
            ),
            (
                "session:worker".to_string(),
                vec!["session:lead".to_string()],
            ),
        ]);
        let ended = HashMap::from([
            ("session:lead".to_string(), ENDED),
            ("session:worker".to_string(), ENDED + 60 * MIN),
        ]);
        assert_eq!(
            settled_at("session:lead", &children, &ended),
            Some(ENDED + 60 * MIN)
        );
        assert_eq!(settled_at("session:none", &children, &ended), None);
    }

    // Defect: a permission dialog alarms before 3 minutes, never, or after
    // its turn ended.
    #[test]
    fn a_dialog_in_the_running_turn_stalls_at_three_minutes() {
        let start = ENDED + MIN;
        let asked = start + MIN;
        let facts = Facts {
            dialog: Some(Dialog {
                kind: DialogKind::Permission,
                since: asked,
            }),
            ..running(start, asked, false)
        };
        assert_eq!(
            status(&facts, asked + MIN, LIMITS),
            Status::BlockedOnDialog { since: asked }
        );
        assert_eq!(
            next_state(&facts, asked + 3 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&facts, asked + 3 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::Dialog,
                stalled_since: asked
            }
        );
        let ended = Facts {
            turn: Turn::Idle {
                started_at: Some(start),
                ended_at: asked + MIN,
            },
            ..facts
        };
        assert_eq!(
            status(&ended, asked + 10 * MIN, LIMITS),
            Status::Idle,
            "a dialog dies with its turn"
        );
    }

    // Defect: a plan left for approval is not seen as a dialog, or an old
    // plan still blocks after the next turn started.
    #[test]
    fn a_plan_blocks_from_its_turns_end_until_the_next_turn() {
        let facts = Facts {
            dialog: Some(Dialog {
                kind: DialogKind::Plan,
                since: ENDED - MIN,
            }),
            ..idle()
        };
        assert_eq!(
            next_state(&facts, ENDED + 3 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::Dialog,
                stalled_since: ENDED
            }
        );
        let next_turn = Facts {
            turn: Turn::Idle {
                started_at: Some(ENDED + MIN),
                ended_at: ENDED + 2 * MIN,
            },
            ..facts
        };
        assert_eq!(status(&next_turn, ENDED + 30 * MIN, LIMITS), Status::Idle);
    }

    // Defect: a turn that went silent with no tool running is never hung.
    #[test]
    fn a_silent_running_turn_with_no_tool_is_hung_at_n() {
        let start = ENDED + MIN;
        let last = start + 2 * MIN;
        let facts = running(start, last, false);
        assert_eq!(
            next_state(&facts, last + 15 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&facts, last + 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::Hung,
                stalled_since: last
            }
        );
    }

    // Defect: a message the provider never took stalls the agent silently.
    #[test]
    fn a_wake_waiting_two_minutes_with_no_turn_is_wake_missed() {
        let seen = ENDED + 30 * MIN;
        let facts = Facts {
            wake_pending_since: Some(seen),
            ..idle()
        };
        assert_eq!(
            next_state(&facts, seen + 2 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&facts, seen + 2 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::WakeMissed,
                stalled_since: seen
            }
        );
    }

    // Defect: precedence (design 2): an untaken wake reads as plain idle
    // work, or a hung turn hides an unanswered dialog.
    #[test]
    fn wake_missed_wins_over_idle_and_dialog_wins_over_hung() {
        let seen = ENDED + 20 * MIN;
        let both = Facts {
            wake_pending_since: Some(seen),
            ..with_ask(idle())
        };
        assert_eq!(
            status(&both, seen + 2 * MIN, LIMITS),
            Status::WakeMissed { since: seen }
        );
        let start = ENDED + MIN;
        let hung_with_dialog = Facts {
            dialog: Some(Dialog {
                kind: DialogKind::Question,
                since: start,
            }),
            ..running(start, start, false)
        };
        assert_eq!(
            status(&hung_with_dialog, start + 60 * MIN, LIMITS),
            Status::BlockedOnDialog { since: start }
        );
    }

    // Defect: re-running a pass on a stalled agent opens a second episode or
    // closes the first.
    #[test]
    fn a_still_stalled_agent_keeps_its_episode() {
        let facts = after(with_ask(idle()), Cause::IdleOpenWork, ENDED);
        assert_eq!(
            next_state(&facts, ENDED + 120 * MIN, LIMITS),
            Transition::Stay
        );
        let now_wake = Facts {
            wake_pending_since: Some(ENDED + 30 * MIN),
            ..facts
        };
        assert_eq!(
            next_state(&now_wake, ENDED + 120 * MIN, LIMITS),
            Transition::Stay,
            "a second cause keeps the first episode"
        );
    }

    // Defect: an episode outlives the agent's next turn, its answered
    // dialog or its hung turn's next output.
    #[test]
    fn progress_closes_the_episode() {
        let woke = Facts {
            turn: Turn::Running {
                started_at: ENDED + 20 * MIN,
                progress: Progress::Untapped,
            },
            ..after(with_ask(idle()), Cause::IdleOpenWork, ENDED)
        };
        assert_eq!(
            next_state(&woke, ENDED + 21 * MIN, LIMITS),
            Transition::Close(CloseReason::Progress)
        );
        let start = ENDED + MIN;
        let answered = after(running(start, start, false), Cause::Dialog, start);
        assert_eq!(
            next_state(&answered, start + 4 * MIN, LIMITS),
            Transition::Close(CloseReason::Progress)
        );
        let printed = after(
            running(start, start + 30 * MIN, false),
            Cause::Hung,
            start + 2 * MIN,
        );
        assert_eq!(
            next_state(&printed, start + 31 * MIN, LIMITS),
            Transition::Close(CloseReason::Progress)
        );
    }

    // Defect: an episode stays open after its ask was acked elsewhere, or
    // after the session was archived.
    #[test]
    fn cleared_work_and_a_gone_agent_close_the_episode() {
        let cleared = after(idle(), Cause::IdleOpenWork, ENDED);
        assert_eq!(
            next_state(&cleared, ENDED + 20 * MIN, LIMITS),
            Transition::Close(CloseReason::WorkCleared)
        );
        let gone = Facts {
            gone: true,
            ..after(with_ask(idle()), Cause::IdleOpenWork, ENDED)
        };
        assert_eq!(
            next_state(&gone, ENDED + 20 * MIN, LIMITS),
            Transition::Close(CloseReason::AgentGone)
        );
        let gone_fresh = Facts {
            gone: true,
            ..with_ask(idle())
        };
        assert_eq!(
            next_state(&gone_fresh, ENDED + 20 * MIN, LIMITS),
            Transition::Stay,
            "a gone agent opens nothing"
        );
    }
}
