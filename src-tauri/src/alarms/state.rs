//! Design section 2: the state of one agent, derived from its facts at each
//! detector pass and never stored. `next_state` says what the pass does to
//! the agent's one open episode. Every time is active-clock time.

use serde::Serialize;

/// How many stall thresholds an agent may wait on a background tool: a
/// silent turn before it counts as hung (lead n3), or an idle agent with
/// open work before it alarms (lead ruling S1 r3 b).
pub(crate) const BACKGROUND_STALLS: i64 = 4;

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

/// A background tool (a Claude task) of the agent, which may outlive the
/// turn that started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Background {
    None,
    Running,
    Ended { at: i64 },
}

/// What the stdio taps saw of a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Progress {
    /// No tap reads this harness, so a running turn counts as working.
    Untapped,
    Tapped {
        last_at: i64,
        /// A tool call of the turn runs.
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
    /// A Mono completion result not yet in its chat, or the bare row of a
    /// session that stopped with no result (ADE-271).
    Notice {
        id: String,
    },
    /// An item the chat's saved queue holds that no turn took.
    Queued {
        id: String,
    },
}

/// A child agent as its parent sees it (`tree`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildState {
    /// It runs, waits, or has open work of its own: the parent waits.
    Unsettled,
    /// Plain idle since its own last turn end.
    Settled { ended_at: i64 },
}

/// A session the agent started or asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Child {
    pub actor: String,
    pub started: bool,
    pub state: ChildState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cause {
    IdleOpenWork,
    Dialog,
    Hung,
    WakeMissed,
    /// The sleep hold's (rows H and P2); the detector never opens one.
    HoldMissing,
}

/// An episode cause this build does not know, from a newer build's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownCause(pub String);

impl Cause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Cause::IdleOpenWork => "idle_open_work",
            Cause::Dialog => "dialog",
            Cause::Hung => "hung",
            Cause::WakeMissed => "wake_missed",
            Cause::HoldMissing => "hold_missing",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, UnknownCause> {
        [
            Cause::IdleOpenWork,
            Cause::Dialog,
            Cause::Hung,
            Cause::WakeMissed,
            Cause::HoldMissing,
        ]
        .into_iter()
        .find(|cause| cause.as_str() == text)
        .ok_or_else(|| UnknownCause(text.to_string()))
    }
}

/// The agent's open episode, as its row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Episode {
    pub cause: Result<Cause, UnknownCause>,
    pub stalled_since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The session was archived or deleted, or the Mono deleted.
    pub gone: bool,
    pub turn: Turn,
    pub dialog: Option<Dialog>,
    pub background: Background,
    /// The agent's own open work; a child's result is added from `children`.
    pub open_work: Vec<OpenWork>,
    pub children: Vec<Child>,
    /// The last turn end below its settled children: open work it held
    /// while it waited on them counts from there (lead cascade rule 3).
    pub settled_at: Option<i64>,
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

/// When each child's result the agent has not handled came: a settled
/// child the agent started that ended after the agent's last turn ended,
/// so no turn of the agent has seen it (lead N4). One that ended during the
/// agent's turn reached it as a completion notice.
fn unhandled(facts: &Facts) -> impl Iterator<Item = (&Child, i64)> {
    let ended = match facts.turn {
        Turn::Idle { ended_at, .. } => Some(ended_at),
        Turn::Running { .. } => None,
    };
    facts
        .children
        .iter()
        .filter_map(move |child| match (child.started, child.state, ended) {
            (true, ChildState::Settled { ended_at }, Some(ended)) if ended_at > ended => {
                Some((child, ended_at))
            }
            _ => None,
        })
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
            let silent = now - since;
            let hung_at = if facts.background == Background::Running {
                BACKGROUND_STALLS * limits.stall_ms
            } else {
                limits.stall_ms
            };
            if silent >= hung_at {
                Status::Hung { since }
            } else if silent >= limits.stall_ms {
                Status::Waiting
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
                .any(|child| child.state == ChildState::Unsettled)
            {
                return Status::Waiting;
            }
            if open_work(facts).is_empty() {
                return Status::Idle;
            }
            let background_end = match facts.background {
                Background::Ended { at } => Some(at),
                Background::None | Background::Running => None,
            };
            let since = unhandled(facts)
                .map(|(_, at)| at)
                .chain(facts.settled_at)
                .chain(background_end)
                .fold(ended_at, i64::max);
            if facts.background == Background::Running
                && now - since < BACKGROUND_STALLS * limits.stall_ms
            {
                return Status::Waiting;
            }
            Status::IdleOpenWork { since }
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
pub(crate) fn progressed(facts: &Facts, episode: &Episode) -> bool {
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
            Ok(Cause::Hung) => matches!(
                facts.turn,
                Turn::Running {
                    progress: Progress::Tapped { last_at, tool_running, .. },
                    ..
                } if tool_running || last_at > since
            ),
            Ok(Cause::Dialog) => blocking_dialog(facts).is_none_or(|dialog| dialog.since != since),
            Ok(Cause::IdleOpenWork | Cause::WakeMissed | Cause::HoldMissing) | Err(_) => false,
        }
}

/// What this pass does to the agent's episode. A still-stalled agent keeps
/// its episode and its first cause. An episode the detector does not judge
/// (the sleep hold's, or a newer build's cause) closes here only when the
/// agent is gone.
pub(crate) fn next_state(facts: &Facts, now: i64, limits: Thresholds) -> Transition {
    let status = status(facts, now, limits);
    let stall = status.stall(now, limits);
    match (&facts.episode, stall) {
        (None, None) => Transition::Stay,
        (None, Some((cause, stalled_since))) => Transition::Open {
            cause,
            stalled_since,
        },
        (Some(_), _) if status == Status::Gone => Transition::Close(CloseReason::AgentGone),
        (
            Some(Episode {
                cause: Ok(Cause::HoldMissing) | Err(_),
                ..
            }),
            _,
        ) => Transition::Stay,
        (Some(episode), _) if progressed(facts, episode) => {
            Transition::Close(CloseReason::Progress)
        }
        (Some(_), Some(_)) => Transition::Stay,
        // Still stalled, only under a raised threshold (alarms.set, lead
        // ruling b): the episode stands until progress or the work goes.
        (Some(_), None) if !matches!(status, Status::Idle | Status::Waiting) => Transition::Stay,
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
            background: Background::None,
            open_work: Vec::new(),
            children: Vec::new(),
            settled_at: None,
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
            started: true,
            state,
        }
    }

    fn after(facts: Facts, cause: Cause, stalled_since: i64) -> Facts {
        Facts {
            episode: Some(Episode {
                cause: Ok(cause),
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
            children: vec![child(ChildState::Unsettled)],
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
            children: vec![child(ChildState::Settled { ended_at: done })],
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
                children: vec![child(ChildState::Settled { ended_at })],
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

    // Defect (S1 r2 M1(a)): a child the agent only asked counts as a
    // result it must handle, though no completion notice reaches an asker.
    #[test]
    fn only_a_started_child_is_a_result() {
        let asked = Child {
            started: false,
            ..child(ChildState::Settled {
                ended_at: ENDED + 5 * MIN,
            })
        };
        let facts = Facts {
            children: vec![asked],
            ..idle()
        };
        assert_eq!(open_work(&facts), Vec::new());
        assert_eq!(
            next_state(&facts, ENDED + 600 * MIN, LIMITS),
            Transition::Stay
        );
    }

    // Defect (lead n3): a turn silent on a background tool (a dev server,
    // a monitor) can never be hung, or is hung at N though the turn may
    // really wait on it.
    #[test]
    fn a_turn_silent_on_a_background_tool_waits_until_four_n_then_is_hung() {
        let start = ENDED + MIN;
        let last = start + MIN;
        let facts = Facts {
            turn: Turn::Running {
                started_at: start,
                progress: Progress::Tapped {
                    last_at: last,
                    tool_running: false,
                },
            },
            background: Background::Running,
            ..idle()
        };
        assert_eq!(status(&facts, last + 15 * MIN, LIMITS), Status::Waiting);
        assert_eq!(
            next_state(&facts, last + 4 * 15 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&facts, last + 4 * 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::Hung,
                stalled_since: last
            }
        );
    }

    // Defect (S1 r3 M2, lead ruling b): an agent that backgrounds a wait
    // and ends its turn with open work alarms at N while the job runs, or
    // never; or once the job ends its clock does not restart there.
    #[test]
    fn an_idle_agent_on_a_background_tool_waits_until_four_n() {
        let with_background = |background| Facts {
            background,
            ..with_ask(idle())
        };
        let running = with_background(Background::Running);
        for at in [ENDED + 15 * MIN, ENDED + 4 * 15 * MIN - 1] {
            assert_eq!(next_state(&running, at, LIMITS), Transition::Stay, "{at}");
        }
        assert_eq!(
            next_state(&running, ENDED + 4 * 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::IdleOpenWork,
                stalled_since: ENDED
            }
        );
        let finished = ENDED + 20 * MIN;
        let ended = with_background(Background::Ended { at: finished });
        assert_eq!(
            next_state(&ended, finished + 15 * MIN - 1, LIMITS),
            Transition::Stay
        );
        assert_eq!(
            next_state(&ended, finished + 15 * MIN, LIMITS),
            Transition::Open {
                cause: Cause::IdleOpenWork,
                stalled_since: finished
            }
        );
        assert_eq!(
            status(
                &Facts {
                    open_work: Vec::new(),
                    ..running
                },
                ENDED + 600 * MIN,
                LIMITS
            ),
            Status::Idle,
            "no open work, no alarm"
        );
    }

    // Defect (S1 r2 F2): an episode whose cause this build does not judge
    // (the sleep hold's, or a newer build's) is closed as work_cleared the
    // pass after it opens, or never closes when its agent goes.
    #[test]
    fn an_episode_the_detector_does_not_judge_stays_until_its_agent_goes() {
        assert_eq!(Cause::parse("hold_missing"), Ok(Cause::HoldMissing));
        assert_eq!(Cause::parse("asleep"), Err(UnknownCause("asleep".into())));
        for cause in [Cause::parse("hold_missing"), Cause::parse("asleep")] {
            let open = Facts {
                episode: Some(Episode {
                    cause: cause.clone(),
                    stalled_since: ENDED,
                }),
                turn: Turn::Running {
                    started_at: ENDED + MIN,
                    progress: Progress::Untapped,
                },
                ..idle()
            };
            assert_eq!(
                next_state(&open, ENDED + 20 * MIN, LIMITS),
                Transition::Stay,
                "{cause:?}"
            );
            let gone = Facts { gone: true, ..open };
            assert_eq!(
                next_state(&gone, ENDED + 20 * MIN, LIMITS),
                Transition::Close(CloseReason::AgentGone),
                "{cause:?}"
            );
        }
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

    // Defect: raising N mid-episode closes it as work_cleared, though the
    // work is still open and the agent has not moved.
    #[test]
    fn a_raised_threshold_keeps_a_stall_that_has_not_moved_open() {
        let raised = Thresholds {
            stall_ms: 60 * MIN,
            ..LIMITS
        };
        let waiting = after(with_ask(idle()), Cause::IdleOpenWork, ENDED);
        assert_eq!(
            next_state(&waiting, ENDED + 20 * MIN, raised),
            Transition::Stay
        );
        let start = ENDED + MIN;
        let hung = after(running(start, start, false), Cause::Hung, start);
        assert_eq!(
            next_state(&hung, start + 20 * MIN, raised),
            Transition::Stay
        );
        assert_eq!(
            next_state(
                &after(idle(), Cause::IdleOpenWork, ENDED),
                ENDED + 20 * MIN,
                raised
            ),
            Transition::Close(CloseReason::WorkCleared),
            "the work went"
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
