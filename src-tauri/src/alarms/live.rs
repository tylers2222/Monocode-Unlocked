//! What the taps saw, kept in memory (lead decision 2026-11-08, S1 review
//! M3/F3). The stdout reader, `harness_write` and the turn hooks record
//! here behind one leaf mutex and never take the store lock. The detector
//! pass on the clock thread is the only writer of `agent_activity`: it
//! takes the records and writes them in its own transaction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::taps::{Harness, Pipe, Snapshot, Tap};
use crate::mono_messages::SessionId;

/// One change to a session's row, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Write {
    TurnStarted {
        turn_id: String,
        at: i64,
    },
    TurnEnded {
        turn_id: String,
        at: i64,
    },
    /// A new child; `harness` names the tap that reads it (`None`: none).
    Spawned {
        harness: Option<Harness>,
        at: i64,
    },
    Tap(Snapshot),
}

/// Which edge of a turn a hook saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Started,
    Ended,
}

pub(crate) type Taken = HashMap<SessionId, Vec<Write>>;

struct Hub {
    /// The active time of this process's clock at its last tick; -1 while
    /// no clock runs here (another process holds the lease and the rows).
    now: AtomicI64,
    pending: Mutex<Taken>,
    /// The detector fault logged last, so a lasting one logs once.
    fault: Mutex<Option<String>>,
}

/// The store's handle on the hub; a clone shares it.
#[derive(Clone)]
pub(crate) struct Live(Arc<Hub>);

impl Default for Live {
    fn default() -> Self {
        Self(Arc::new(Hub {
            now: AtomicI64::new(-1),
            pending: Mutex::default(),
            fault: Mutex::default(),
        }))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Live {
    /// The clock thread's tick: taps measure against `active` from now on.
    pub(crate) fn set_now(&self, active: i64) {
        self.0.now.store(active, Ordering::Relaxed);
    }

    fn now(&self) -> Option<i64> {
        let now = self.0.now.load(Ordering::Relaxed);
        (now >= 0).then_some(now)
    }

    fn record(&self, session: &SessionId, write: Write) {
        let mut pending = lock(&self.0.pending);
        let writes = pending.entry(session.clone()).or_default();
        // A newer snapshot of the tap holds everything the older one did.
        if let (Write::Tap(snapshot), Some(Write::Tap(last))) = (&write, writes.last_mut()) {
            last.clone_from(snapshot);
            return;
        }
        writes.push(write);
    }

    /// Turn `turn_id` of `session` started or ended now.
    pub(crate) fn turn(&self, session: &str, turn_id: &str, edge: Edge) {
        let (Some(session), Some(at)) = (SessionId::parse(session), self.now()) else {
            return;
        };
        let turn_id = turn_id.to_string();
        self.record(
            &session,
            match edge {
                Edge::Started => Write::TurnStarted { turn_id, at },
                Edge::Ended => Write::TurnEnded { turn_id, at },
            },
        );
    }

    /// A child of `session` by `provider` was spawned. Recorded for every
    /// harness, so a session that moves to one no tap reads is not judged
    /// by the last tap's row.
    pub(crate) fn spawned(&self, session: &str, provider: Option<&str>) {
        let (Some(session), Some(at)) = (SessionId::parse(session), self.now()) else {
            return;
        };
        let harness = provider.and_then(Harness::parse);
        self.record(&session, Write::Spawned { harness, at });
    }

    /// The tap for a child of `session` by `provider`, when this build
    /// reads that harness.
    pub(crate) fn tap(&self, session: &str, provider: Option<&str>) -> Option<ChildTap> {
        Some(ChildTap {
            live: self.clone(),
            session: SessionId::parse(session)?,
            tap: Mutex::new(Tap::new(provider.and_then(Harness::parse)?)),
        })
    }

    /// Everything recorded since the last take.
    pub(crate) fn take(&self) -> Taken {
        std::mem::take(&mut *lock(&self.0.pending))
    }

    /// A take whose writes did not commit goes back before what came since.
    pub(crate) fn put_back(&self, taken: Taken) {
        let mut pending = lock(&self.0.pending);
        for (session, mut writes) in taken {
            let later = pending.remove(&session).unwrap_or_default();
            writes.extend(later);
            pending.insert(session, writes);
        }
    }

    /// Whether `fault` is new since the last one logged; `None` clears it.
    pub(crate) fn new_fault(&self, fault: Option<String>) -> bool {
        let mut last = lock(&self.0.fault);
        let new = fault.is_some() && *last != fault;
        *last = fault;
        new
    }

    /// The last pass's fault, `None` when it skipped no agent.
    #[cfg(test)]
    pub(crate) fn last_fault(&self) -> Option<String> {
        lock(&self.0.fault).clone()
    }
}

/// The stall tap on one child's stdio. Its lock orders the stdout reader's
/// and `harness_write`'s lines; under it only the hub's leaf lock is taken.
pub(crate) struct ChildTap {
    live: Live,
    session: SessionId,
    tap: Mutex<Tap>,
}

impl ChildTap {
    pub(crate) fn line(&self, pipe: Pipe, line: &str) {
        let Some(now) = self.live.now() else {
            return;
        };
        let mut tap = lock(&self.tap);
        if let Some(row) = tap.line(pipe, line, now) {
            tap.wrote(row.clone(), now);
            self.live.record(&self.session, Write::Tap(row));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    // Defect: a tap or turn hook records before this process runs a clock,
    // so its time is not active time; or tap snapshots pile up instead of
    // the newest one replacing the last.
    #[test]
    fn records_wait_for_the_clock_and_a_newer_snapshot_replaces_the_last() {
        let live = Live::default();
        live.turn("s-1", "t1", Edge::Started);
        assert!(live.take().is_empty(), "no clock runs here");
        live.set_now(7);
        let tap = live.tap("s-1", Some("claude")).unwrap();
        live.turn("s-1", "t1", Edge::Started);
        tap.line(Pipe::Stdout, r#"{"type":"system","subtype":"init"}"#);
        live.set_now(20_000);
        tap.line(Pipe::Stdout, r#"{"type":"assistant"}"#);
        let writes = live.take().remove(&session()).unwrap();
        assert_eq!(writes.len(), 2, "{writes:?}");
        assert!(matches!(&writes[1], Write::Tap(row) if row.last_progress_at == 20_000));
        assert!(live.take().is_empty());
    }

    // Defect: the writes of a pass that rolled back are lost, or come back
    // after the ones recorded since, so an old turn end overrides a newer
    // start.
    #[test]
    fn a_take_put_back_goes_before_what_came_since() {
        let live = Live::default();
        live.set_now(1);
        live.turn("s-1", "t1", Edge::Ended);
        let taken = live.take();
        live.turn("s-1", "t2", Edge::Started);
        live.put_back(taken);
        let writes = live.take().remove(&session()).unwrap();
        assert_eq!(
            writes,
            vec![
                Write::TurnEnded {
                    turn_id: "t1".into(),
                    at: 1
                },
                Write::TurnStarted {
                    turn_id: "t2".into(),
                    at: 1
                },
            ]
        );
    }
}
