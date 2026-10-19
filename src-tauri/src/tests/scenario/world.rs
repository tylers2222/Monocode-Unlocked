use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::MutexGuard;

use rusqlite::Connection;

use crate::active_clock::{self, ActiveClock, Clock};
use crate::mono_messages::{self, SessionId, Targeted};
use crate::session_store::SessionStore;

/// Names a `.backup` copy of a real `monocode.db` under `/tmp` that a World
/// starts from instead of an empty database.
pub(crate) const SCENARIO_DB: &str = "MONOCODE_SCENARIO_DB";

/// The fake wall time a World starts at (2025-10-09), so stored times look
/// like real ones next to a real database's rows.
const START_MS: i64 = 1_760_000_000_000;

/// The app's tick period (`active_clock`'s thread).
const TICK_MS: i64 = 1_000;

/// The computer's clocks as a World moves them. One value, shared by the
/// World and the app's active clock.
#[derive(Clone)]
pub(crate) struct FakeClock(Rc<FakeTime>);

struct FakeTime {
    uptime_ns: Cell<u64>,
    monotonic_ns: Cell<u64>,
    wall_ms: Cell<i64>,
    boot_wall_ms: Cell<i64>,
}

impl FakeClock {
    fn new() -> Self {
        Self(Rc::new(FakeTime {
            uptime_ns: Cell::new(0),
            monotonic_ns: Cell::new(0),
            wall_ms: Cell::new(START_MS),
            boot_wall_ms: Cell::new(START_MS),
        }))
    }

    /// Awake time passes: every clock moves.
    fn run(&self, ms: i64) {
        self.slept(ms);
        self.0.uptime_ns.set(self.0.uptime_ns.get() + ns(ms));
    }

    /// The computer sleeps: the wall and monotonic clocks move, uptime not.
    fn slept(&self, ms: i64) {
        self.0.monotonic_ns.set(self.0.monotonic_ns.get() + ns(ms));
        self.jump_wall(ms);
    }

    fn jump_wall(&self, ms: i64) {
        self.0.wall_ms.set(self.0.wall_ms.get() + ms);
    }

    fn boot(&self) {
        self.0.uptime_ns.set(0);
        self.0.monotonic_ns.set(0);
        self.0.boot_wall_ms.set(self.0.wall_ms.get());
    }
}

fn ns(ms: i64) -> u64 {
    u64::try_from(ms).unwrap() * 1_000_000
}

impl Clock for FakeClock {
    fn uptime_ns(&self) -> u64 {
        self.0.uptime_ns.get()
    }

    fn monotonic_ns(&self) -> u64 {
        self.0.monotonic_ns.get()
    }

    fn wall_ms(&self) -> i64 {
        self.0.wall_ms.get()
    }

    fn boot_wall_ms(&self) -> i64 {
        self.0.boot_wall_ms.get()
    }
}

/// What dies with the app process: the store and the active clock, which
/// holds the clock lease.
struct App {
    store: SessionStore,
    active: ActiveClock<FakeClock>,
}

/// The window label every hand-off slot names; a World is one window.
const WINDOW: &str = "main";

/// The app on a file database, on a [`FakeClock`]. `running` is the turn
/// each session runs now, as `ControlHost` reports it to the store.
pub(crate) struct World {
    dir: PathBuf,
    path: PathBuf,
    app: Option<App>,
    running: HashMap<String, String>,
    clock: FakeClock,
}

impl World {
    /// A World on a new file database, or on a copy of the database
    /// `MONOCODE_SCENARIO_DB` names. The app is running.
    pub(crate) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "monocode-scenario-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("monocode.db");
        if let Ok(source) = std::env::var(SCENARIO_DB) {
            // Only a `sqlite3 .backup` copy is a consistent snapshot: a plain
            // copy of the live database leaves out its `-wal` file, so it
            // can miss recent commits. Resolved first, so a symlink or `..`
            // cannot lead out of /tmp (macOS /tmp is /private/tmp).
            let source = std::fs::canonicalize(&source)
                .unwrap_or_else(|error| panic!("{SCENARIO_DB}={source}: {error}"));
            assert!(
                source.starts_with("/tmp") || source.starts_with("/private/tmp"),
                "{SCENARIO_DB} must name a `sqlite3 .backup` copy under /tmp, not {}",
                source.display()
            );
            std::fs::copy(&source, &path).unwrap();
        }
        let mut world = Self {
            dir,
            path,
            app: None,
            running: HashMap::new(),
            clock: FakeClock::new(),
        };
        world.restart();
        if let Ok(source) = std::env::var(SCENARIO_DB) {
            let sessions: i64 = world
                .conn()
                .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
                .unwrap();
            eprintln!("scenario {name}: started from {source}, {sessions} sessions");
        }
        world
    }

    /// The wall time every call is made at.
    pub(crate) fn now(&self) -> i64 {
        self.clock.wall_ms()
    }

    /// `ms` of awake time pass while the app runs; its clock thread ticks
    /// every second.
    pub(crate) fn advance(&mut self, ms: i64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(TICK_MS);
            self.clock.run(step);
            self.tick();
            left -= step;
        }
    }

    /// `ms` of awake time pass with no tick (the thread stalled), then one.
    pub(crate) fn stall(&mut self, ms: i64) {
        self.clock.run(ms);
        self.tick();
    }

    /// The computer sleeps `ms`; the app's first tick comes after the wake.
    pub(crate) fn sleep(&mut self, ms: i64) {
        self.clock.slept(ms);
        self.tick();
    }

    /// The wall clock is set `ms` forward (or back, when negative) while the
    /// app runs; the next tick sees it.
    pub(crate) fn jump_wall(&mut self, ms: i64) {
        self.clock.jump_wall(ms);
        self.tick();
    }

    /// The app is down for `ms`: it is killed, only the wall clock moves,
    /// and it starts again.
    pub(crate) fn down(&mut self, ms: i64) {
        self.kill();
        self.clock.jump_wall(ms);
        self.restart();
    }

    /// The app is down for `ms` across a reboot of the computer.
    pub(crate) fn reboot(&mut self, ms: i64) {
        self.kill();
        self.clock.jump_wall(ms);
        self.clock.boot();
        self.restart();
    }

    fn tick(&mut self) {
        let app = self.app.as_mut().expect("the app is not running");
        app.active.tick(&app.store.lock_conn().unwrap()).unwrap();
    }

    /// The app's active clock now (what it holds, not what it stored).
    pub(crate) fn active_ms(&self) -> i64 {
        self.app
            .as_ref()
            .expect("the app is not running")
            .active
            .active_ms()
    }

    /// The app process stops at once: the store, the clock lease and the
    /// active clock go with no app shutdown path, so only what the store
    /// committed survives. No turn outlives it.
    pub(crate) fn kill(&mut self) {
        assert!(self.app.take().is_some(), "the app is not running");
        self.running.clear();
    }

    /// The app starts again on the same file, through the app's own open
    /// path (migrate, then `startup` at the World's wall time), then takes
    /// the clock lease and launches the active clock (`active_clock::start`).
    pub(crate) fn restart(&mut self) {
        assert!(self.app.is_none(), "the app is already running");
        let store = SessionStore::open_at(self.path.clone(), self.now()).unwrap();
        // The lease is free once the killed app's file is closed, but a child
        // that another test thread is spawning (s0 runs `git`) holds a copy of
        // that file between its fork and exec. So a retake waits up to 2 s;
        // the app itself takes the lease once and never retakes it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let active = loop {
            let started = active_clock::start(
                &self.lock_path(),
                &store.lock_conn().unwrap(),
                self.clock.clone(),
            );
            if let Some(active) = started.unwrap() {
                break active;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "another process holds the lease"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        self.app = Some(App { store, active });
    }

    /// A second app process on the same data folder: it opens the store
    /// and tries to start the active clock. True when it got the lease.
    pub(crate) fn second_launch(&self) -> bool {
        let store = SessionStore::open_at(self.path.clone(), self.now()).unwrap();
        let started = active_clock::start(
            &self.lock_path(),
            &store.lock_conn().unwrap(),
            self.clock.clone(),
        );
        started.unwrap().is_some()
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("scheduler.lock")
    }

    pub(crate) fn store(&self) -> &SessionStore {
        &self.app.as_ref().expect("the app is not running").store
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.store().lock_conn().unwrap()
    }

    pub(crate) fn running(&self) -> HashMap<String, String> {
        self.running.clone()
    }

    /// What the window does when `session` is idle and a message waits for
    /// it: it starts turn `turn` with every unqueued row (`unqueued`, then
    /// `assign_slot` as a submit), and returns the rows it handed off. The
    /// provider has not taken the text yet; see [`World::end_turn`].
    pub(crate) fn start_turn(&mut self, session: &SessionId, turn: &str) -> Vec<Targeted> {
        self.running
            .insert(session.as_str().to_string(), turn.to_string());
        let waiting = mono_messages::unqueued(&self.conn(), session).unwrap();
        waiting
            .into_iter()
            .filter_map(|row| {
                let (slotted, _) = mono_messages::assign_slot(
                    &self.conn(),
                    &row.message.id,
                    turn,
                    WINDOW,
                    mono_messages::Delivery::Submit,
                    self.now(),
                )
                .unwrap();
                slotted
            })
            .collect()
    }

    /// Turn `turn` of `session` ends. As the window does when its send
    /// resolves (`registry.ts`, `sendHarnessTurn`'s `finally`): when the
    /// provider took the turn's text (`sent`) it confirms the openers
    /// (`mono_message_confirm_openers`), then the turn ends (`turn_ended`).
    /// A messaging call bound to the turn also confirms them (`submit`).
    pub(crate) fn end_turn(&mut self, session: &SessionId, turn: &str, sent: bool) {
        self.running.remove(session.as_str());
        if sent {
            mono_messages::confirm_openers(&self.conn(), session, turn, self.now()).unwrap();
        }
        mono_messages::turn_ended(&self.conn(), session, turn, self.now()).unwrap();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Close the database before its folder goes (Windows keeps open
        // files).
        self.app = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
