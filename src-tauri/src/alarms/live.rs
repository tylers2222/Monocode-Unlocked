//! What the taps saw, kept in memory (lead decision 2026-11-08, S1 review
//! M3/F3). The stdout reader, `harness_write` and the turn hooks record
//! here behind one leaf mutex and never take the store lock. The detector
//! pass on the clock thread is the only writer of `agent_activity`: it
//! takes the records and writes them in its own transaction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::taps::{Harness, Pipe, Snapshot, Tap};
use crate::mono_messages::SessionId;
use crate::sleep_hold::install::HoldState;

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
    /// A new child; `harness` names the tap that reads it (`None`: none),
    /// `hold` its sleep hold (`None`: its harness has none).
    Spawned {
        harness: Option<Harness>,
        hold: Option<HoldState>,
        at: i64,
    },
    /// The child's sleep hold reported in (`hold ping`).
    HoldPinged,
    Tap(Snapshot),
    /// The agent's provisioner answered its plan (lead ruling, S2 r2 m1).
    PlanAnswered,
}

/// Which edge of a turn a hook saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Started,
    Ended,
}

pub(crate) type Taken = HashMap<SessionId, Vec<Write>>;

#[derive(Default)]
struct Pending {
    writes: Taken,
    /// The tap of each session's current child; none while that child is
    /// untapped. A replaced child's reader writes nothing more (S1 r5 m1).
    current: HashMap<SessionId, u64>,
}

impl Pending {
    fn push(&mut self, session: &SessionId, write: Write) {
        let writes = self.writes.entry(session.clone()).or_default();
        // A newer snapshot of the tap holds everything the older one did.
        if let (Write::Tap(snapshot), Some(Write::Tap(last))) = (&write, writes.last_mut()) {
            last.clone_from(snapshot);
            return;
        }
        writes.push(write);
    }
}

struct Hub {
    /// The active time of this process's clock at its last tick, and when
    /// that tick ran; `None` while no clock runs here (another process
    /// holds the lease and the rows).
    ticked: Mutex<Option<(i64, Instant)>>,
    /// The id of the last tap made.
    taps: AtomicU64,
    pending: Mutex<Pending>,
    /// The detector fault logged last, so a lasting one logs once.
    fault: Mutex<Option<String>>,
}

/// The store's handle on the hub; a clone shares it.
#[derive(Clone)]
pub(crate) struct Live(Arc<Hub>);

impl Default for Live {
    fn default() -> Self {
        Self(Arc::new(Hub {
            ticked: Mutex::default(),
            taps: AtomicU64::new(0),
            pending: Mutex::default(),
            fault: Mutex::default(),
        }))
    }
}

/// Whether a hold reading moves the last tick on by `Instant`. On macOS
/// and Linux `Instant` is the clock the tick adds, which stops during
/// sleep. On Windows it counts sleep, so a reading in the second after a
/// wake would run up to one step ahead: a hold stamped then would stay open
/// past its client's cut (fails open), and one checked then would time out
/// early, which is harmless as the client's own `Instant` ran ahead too.
/// There the reading stays at the last tick instead: it lags by up to a
/// tick, so a hold stamped then times out early (fails closed), but while
/// the tick stalls a hold whose client stopped closes up to the stall late
/// (#547).
const EXTRAPOLATE: bool = !cfg!(windows);

/// The last tick `(active, when it read its clock)` at `at`: moved on by
/// the time since, at most the step the next tick may add, so a stalled
/// tick (App Nap, a busy store) does not hold the clock back.
fn reading((active, ticked): (i64, Instant), at: Instant, extrapolate: bool) -> i64 {
    if !extrapolate {
        return active;
    }
    let since = at
        .saturating_duration_since(ticked)
        .min(crate::active_clock::MAX_STEP);
    active.saturating_add(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Live {
    #[cfg(test)]
    pub(crate) fn set_now(&self, active: i64) {
        self.set_now_at(active, Instant::now());
    }

    /// The clock's stamp: taps measure against `active` from now on, and
    /// the hold reading moves on from `at`, when the clock read it.
    pub(crate) fn set_now_at(&self, active: i64, at: Instant) {
        *lock(&self.0.ticked) = Some((active, at));
    }

    pub(crate) fn now(&self) -> Option<i64> {
        lock(&self.0.ticked).map(|(active, _)| active)
    }

    /// The active time at `at`, with no tick lag where [`EXTRAPOLATE`]
    /// holds, else the last tick's.
    pub(crate) fn now_at(&self, at: Instant) -> Option<i64> {
        let ticked = (*lock(&self.0.ticked))?;
        Some(reading(ticked, at, EXTRAPOLATE))
    }

    fn record(&self, session: &SessionId, write: Write) {
        lock(&self.0.pending).push(session, write);
    }

    /// Records `snapshot` of tap `tap` when it reads `session`'s current
    /// child; whether it did.
    fn record_tap(&self, session: &SessionId, tap: u64, snapshot: Snapshot) -> bool {
        let mut pending = lock(&self.0.pending);
        let current = pending.current.get(session) == Some(&tap);
        if current {
            pending.push(session, Write::Tap(snapshot));
        }
        current
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

    /// `session`'s plan was answered: it waits on no dialog now.
    pub(crate) fn plan_answered(&self, session: &SessionId) {
        if self.now().is_some() {
            self.record(session, Write::PlanAnswered);
        }
    }

    /// `session`'s child is now the one `tap` reads (`None`: one no tap
    /// reads), spawned with sleep hold `hold` (`None`: its harness has none),
    /// and only `tap` writes its row from now on. Recorded for every
    /// harness, so a session that moves to one no tap reads is not judged
    /// by the last tap's row.
    pub(crate) fn spawned(&self, session: &str, tap: Option<&ChildTap>, hold: Option<HoldState>) {
        let Some(session) = SessionId::parse(session) else {
            return;
        };
        let mut pending = lock(&self.0.pending);
        match tap {
            Some(tap) => pending.current.insert(session.clone(), tap.id),
            None => pending.current.remove(&session),
        };
        if let Some(at) = self.now() {
            let harness = tap.map(|tap| tap.harness);
            pending.push(&session, Write::Spawned { harness, hold, at });
        }
    }

    /// The sleep hold of `session` reported in. Queued in order with the
    /// spawn's reset, which `harness_spawn` records as the child starts, long
    /// before omp boots far enough to ping. `false` when not recorded.
    pub(crate) fn hold_pinged(&self, session: &str) -> bool {
        let (Some(session), Some(_)) = (SessionId::parse(session), self.now()) else {
            return false;
        };
        self.record(&session, Write::HoldPinged);
        true
    }

    /// The tap for a child of `session` by `provider`, when this build
    /// reads that harness. It writes once `spawned` names it.
    pub(crate) fn tap(&self, session: &str, provider: Option<&str>) -> Option<ChildTap> {
        let harness = provider.and_then(Harness::parse)?;
        Some(ChildTap {
            live: self.clone(),
            session: SessionId::parse(session)?,
            id: self.0.taps.fetch_add(1, Ordering::Relaxed) + 1,
            harness,
            tap: Mutex::new(Tap::new(harness)),
        })
    }

    /// Everything recorded since the last take.
    pub(crate) fn take(&self) -> Taken {
        std::mem::take(&mut lock(&self.0.pending).writes)
    }

    /// What `session` recorded since the last take, left for the next take.
    pub(crate) fn pending(&self, session: &SessionId) -> Vec<Write> {
        lock(&self.0.pending)
            .writes
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    /// A take whose writes did not commit goes back before what came since.
    pub(crate) fn put_back(&self, taken: Taken) {
        let pending = &mut lock(&self.0.pending).writes;
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
    id: u64,
    harness: Harness,
    tap: Mutex<Tap>,
}

impl ChildTap {
    pub(crate) fn line(&self, pipe: Pipe, line: &str) {
        self.read(|tap, now| tap.line(pipe, line, now));
    }

    /// The child's stdout ended: it exited.
    pub(crate) fn exited(&self) {
        self.read(Tap::exited);
    }

    fn read(&self, read: impl FnOnce(&mut Tap, i64) -> Option<Snapshot>) {
        let Some(now) = self.live.now() else {
            return;
        };
        let mut tap = lock(&self.tap);
        if let Some(row) = read(&mut tap, now) {
            if self.live.record_tap(&self.session, self.id, row.clone()) {
                tap.wrote(row, now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
        let tap = live.tap("s-1", Some("claude")).unwrap();
        live.spawned("s-1", Some(&tap), None);
        live.turn("s-1", "t1", Edge::Started);
        assert!(live.take().is_empty(), "no clock runs here");
        live.set_now(7);
        live.turn("s-1", "t1", Edge::Started);
        tap.line(Pipe::Stdout, r#"{"type":"system","subtype":"init"}"#);
        live.set_now(20_000);
        tap.line(Pipe::Stdout, r#"{"type":"assistant"}"#);
        let writes = live.take().remove(&session()).unwrap();
        assert_eq!(writes.len(), 2, "{writes:?}");
        assert!(matches!(&writes[1], Write::Tap(row) if row.last_progress_at == 20_000));
        assert!(live.take().is_empty());
    }

    // Defect: a hold reads the clock its last tick left, so a tick that
    // stalled for seconds makes a live hold client look idle; or the
    // reading runs past what the next tick adds; or, where `Instant`
    // counts sleep, it runs on past the tick and a hold stamped after a
    // wake stays open past its client's cut.
    #[test]
    fn the_live_reading_moves_on_from_the_last_tick_up_to_one_step() {
        let ticked = Instant::now();
        let at = |ms: u64, extrapolate| {
            reading(
                (40_000, ticked),
                ticked + Duration::from_millis(ms),
                extrapolate,
            )
        };
        assert_eq!(
            [0, 4_900, 90_000].map(|ms| at(ms, true)),
            [40_000, 44_900, 70_000]
        );
        assert_eq!(
            [0, 4_900, 90_000].map(|ms| at(ms, false)),
            [40_000; 3],
            "stays at the tick"
        );
        let live = Live::default();
        assert_eq!(live.now_at(ticked), None, "no clock runs here");
        live.set_now_at(40_000, ticked);
        let later = live.now_at(ticked + Duration::from_millis(4_900));
        #[cfg(not(windows))]
        assert_eq!(later, Some(44_900));
        #[cfg(windows)]
        assert_eq!(later, Some(40_000), "Instant counts sleep here");
        assert_eq!(live.now(), Some(40_000), "taps still read the tick");
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
