//! The active clock (Change R plan v2, R2; research A). One stored row,
//! `mono_clock.active_ms`, counts the time the app runs while the computer
//! is awake. Each ask is timed against it (`due_active_ms`), so app
//! downtime and sleep never make a deadline pass. A 1 s tick adds the
//! uptime that passed, read straight from a clock that stops during sleep,
//! never from `Instant` or a Tokio timer (Rust leaves their sleep behaviour
//! unspecified). One exception: between ticks the lease holder's hold
//! reading moves the last tick on by `Instant`, at most one step
//! ([`crate::alarms::Live::now_at`]). Raw uptime is never stored: it
//! restarts at every boot.
//! Downtime and sleep are kept in `mono_downtime` only to explain delays.

use std::fs::{File, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use tauri::{AppHandle, Manager};

use crate::mono_messages::Notices;
use crate::session_store::SessionStore;

/// The clocks the active clock reads. `uptime_ns` stops while the computer
/// sleeps and `monotonic_ns` does not; both restart at boot. `boot_wall_ms`
/// is the wall time the computer booted.
pub trait Clock {
    /// All four clocks at one instant, so a launch or a tick never mixes
    /// values from two readings.
    fn now(&mut self) -> Reading;
}

/// One reading of the computer's clocks.
/// `fake-clock.json` is one reading as JSON.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(any(feature = "fake-clock", test), derive(serde::Deserialize))]
pub struct Reading {
    pub uptime_ns: u64,
    pub monotonic_ns: u64,
    pub wall_ms: i64,
    pub boot_wall_ms: i64,
}

/// How often the app's thread ticks.
const TICK: Duration = Duration::from_secs(1);
/// The most one tick adds, so a tick that stalled (App Nap, a busy store)
/// cannot add a long gap at once.
pub(crate) const MAX_STEP: Duration = Duration::from_secs(30);
/// The active time a crash may lose: the row is written this often.
const PERSIST_EVERY_NS: u64 = 5_000_000_000;
/// A monotonic minus uptime gap below this is scheduling noise, not sleep.
const SLEEP_MIN_NS: u64 = 2_000_000_000;

const NS_PER_MS: u64 = 1_000_000;

/// The active time as a process reads it, with whether the clock runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Active {
    Running(i64),
    /// No process ticks the clock: the time it stopped at.
    Stopped(i64),
}

/// The active time at wall time `wall` as any process reads it: `live`,
/// this process's clock when it holds the lease, else the stored clock
/// moved on by the wall time since it was written. The lease holder writes
/// it every 5 s, so a longer gap, or no clock at all, is a clock that
/// stopped; wall time never stands in for it, as the computer may sleep.
pub(crate) fn active_at(
    live: Option<i64>,
    conn: &Connection,
    wall: i64,
) -> rusqlite::Result<Active> {
    if let Some(active) = live {
        return Ok(Active::Running(active));
    }
    let stored: Option<(i64, i64)> = conn
        .query_row(
            "SELECT active_ms, last_wall_ms FROM mono_clock WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    // One persist interval plus one tick: the tightest bound that never
    // reads a holder that keeps pace as stopped. A holder tick late by more
    // than a second reads as stopped here, which fails closed.
    let stale_ms = ms(PERSIST_EVERY_NS) + ms(TICK.as_nanos() as u64);
    Ok(match stored {
        None => Active::Stopped(0),
        Some((active, last_wall)) if wall - last_wall > stale_ms => Active::Stopped(active),
        Some((active, last_wall)) => Active::Running(active + (wall - last_wall).max(0)),
    })
}

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_clock (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           active_ms INTEGER NOT NULL CHECK (active_ms >= 0),
           last_wall_ms INTEGER NOT NULL,
           boot_wall_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS mono_downtime (
           id INTEGER PRIMARY KEY,
           start_wall_ms INTEGER NOT NULL,
           end_wall_ms INTEGER NOT NULL,
           cause TEXT NOT NULL CHECK (cause IN ('app_down', 'reboot', 'sleep')),
           active_ms INTEGER NOT NULL
         );",
    )
}

/// The wall time at which the clock read `active_ms`, when it reads
/// `now_active` at `now_wall`: every sleep and app downtime logged since is
/// taken out, so a time before an 8 h sleep stays before it. A downtime
/// logged at `active_ms` itself came after it.
pub(crate) fn wall_at(
    conn: &Connection,
    active_ms: i64,
    (now_active, now_wall): (i64, i64),
) -> rusqlite::Result<i64> {
    let down: i64 = conn.query_row(
        "SELECT COALESCE(SUM(end_wall_ms - start_wall_ms), 0) FROM mono_downtime
          WHERE active_ms >= ?1",
        [active_ms],
        |row| row.get(0),
    )?;
    Ok(now_wall - (now_active - active_ms) - down)
}

/// The running active clock of the one process that holds the lease. The
/// lease is part of it, so a clock that ticks always holds its lease.
pub(crate) struct ActiveClock<C: Clock> {
    _lease: Lease,
    clock: C,
    active_ns: u64,
    persisted_ns: u64,
    uptime_ns: u64,
    monotonic_ns: u64,
    /// The `alarms::PASS_MS` slice the detector last ran in.
    alarm_slice: Option<i64>,
}

impl<C: Clock> ActiveClock<C> {
    /// The app starts: the stored clock goes on from its row, the gap since
    /// the row's last write adds nothing and is logged as `reboot` (the
    /// computer booted after that write) or `app_down`.
    fn launch(lease: Lease, conn: &Connection, mut clock: C) -> rusqlite::Result<Self> {
        let now = clock.now();
        let (wall, boot) = (now.wall_ms, now.boot_wall_ms);
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let stored: Option<(i64, i64)> = tx
            .query_row(
                "SELECT active_ms, last_wall_ms FROM mono_clock WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let active_ms = match stored {
            Some((active_ms, last_wall_ms)) => {
                let cause = if boot > last_wall_ms {
                    "reboot"
                } else {
                    "app_down"
                };
                tx.execute(
                    "INSERT INTO mono_downtime (start_wall_ms, end_wall_ms, cause, active_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![last_wall_ms, wall, cause, active_ms],
                )?;
                active_ms
            }
            None => 0,
        };
        crate::alarms::end_all_running(&tx, active_ms)?;
        tx.execute(
            "INSERT INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET last_wall_ms = ?2, boot_wall_ms = ?3",
            params![active_ms, wall, boot],
        )?;
        tx.commit()?;
        let active_ns = u64::try_from(active_ms).unwrap_or(0) * NS_PER_MS;
        Ok(Self {
            _lease: lease,
            uptime_ns: now.uptime_ns,
            monotonic_ns: now.monotonic_ns,
            clock,
            active_ns,
            persisted_ns: active_ns,
            alarm_slice: None,
        })
    }

    /// Adds the uptime since the last tick, at most 30 s; logs a sleep when
    /// the monotonic clock moved further than uptime; writes the row every
    /// 5 s of active time and after a sleep. Then it enqueues the reminders
    /// that fell due (`ask_scheduler::run`), runs the stall detector once
    /// per 5 s of active time on `alarms`, and names the chats of both.
    pub(crate) fn tick(
        &mut self,
        conn: &Connection,
        alarms: &crate::alarms::Sources,
    ) -> rusqlite::Result<Notices> {
        let read_at = Instant::now();
        let now = self.clock.now();
        let (uptime, monotonic) = (now.uptime_ns, now.monotonic_ns);
        let awake = uptime.saturating_sub(self.uptime_ns);
        let slept = monotonic
            .saturating_sub(self.monotonic_ns)
            .saturating_sub(awake);
        self.uptime_ns = uptime;
        self.monotonic_ns = monotonic;
        self.active_ns += awake.min(MAX_STEP.as_nanos() as u64);
        if slept >= SLEEP_MIN_NS {
            let wall = now.wall_ms;
            conn.execute(
                "INSERT INTO mono_downtime (start_wall_ms, end_wall_ms, cause, active_ms)
                 VALUES (?1, ?2, 'sleep', ?3)",
                params![wall - ms(slept), wall, self.active_ms()],
            )?;
        }
        if slept >= SLEEP_MIN_NS || self.active_ns - self.persisted_ns >= PERSIST_EVERY_NS {
            conn.execute(
                "UPDATE mono_clock SET active_ms = ?1, last_wall_ms = ?2 WHERE id = 1",
                params![self.active_ms(), now.wall_ms],
            )?;
            self.persisted_ns = self.active_ns;
        }
        let active = self.active_ms();
        alarms.live.set_now_at(active, read_at);
        let mut notices = crate::ask_scheduler::run(conn, active, now.wall_ms)?;
        // A hold fault must not stop the reminders or the clock either.
        if let Err(error) = crate::sleep_hold::service::sweep(conn, (active, now.wall_ms)) {
            eprintln!("Could not time out the sleep holds: {error}");
        }
        let slice = active / crate::alarms::PASS_MS;
        if self.alarm_slice != Some(slice) {
            self.alarm_slice = Some(slice);
            // A detector fault must not stop the reminders or the clock.
            match crate::alarms::run(conn, active, now.wall_ms, alarms) {
                Ok(alarms) => notices.merge(alarms),
                Err(error) => {
                    let error = error.to_string();
                    if alarms.live.new_fault(Some(error.clone())) {
                        eprintln!("Could not run the stall detector: {error}");
                    }
                }
            }
        }
        Ok(notices)
    }

    pub(crate) fn active_ms(&self) -> i64 {
        ms(self.active_ns)
    }
}

fn ms(ns: u64) -> i64 {
    i64::try_from(ns / NS_PER_MS).unwrap_or(i64::MAX)
}

/// The scheduler lease: an exclusive lock on `scheduler.lock` in the data
/// folder. Only the process that holds it runs the clock, so two processes
/// on one database never add the same time twice. The OS drops the lock
/// when the process dies.
pub(crate) struct Lease {
    _file: File,
}

/// The launched clock, holding the lease, or `None` when another process
/// holds the lease; that process ticks and this one writes nothing.
pub(crate) fn start<C: Clock>(
    lock: &Path,
    conn: &Connection,
    clock: C,
) -> Result<Option<ActiveClock<C>>, String> {
    let file = File::create(lock).map_err(|error| error.to_string())?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error.to_string()),
    }
    ActiveClock::launch(Lease { _file: file }, conn, clock)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Starts the 1 s clock thread, beside `reminders::init`.
pub(crate) fn init<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    #[cfg(not(feature = "fake-clock"))]
    let clock = SystemClock;
    #[cfg(feature = "fake-clock")]
    let clock = fake::FileClock::open(data.join("fake-clock.json"))?;
    let started = start(
        &data.join("scheduler.lock"),
        &*app.state::<SessionStore>().lock_conn()?,
        clock,
    )?;
    let Some(mut clock) = started else {
        eprintln!("Another MonoCode process runs the active clock for this data folder");
        return Ok(());
    };
    // Taps record from launch, not from the first tick.
    app.state::<SessionStore>()
        .alarms
        .set_now(clock.active_ms());
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        // Before the store lock (design 5.3 lock order).
        let running = app
            .try_state::<crate::control::ControlHost>()
            .and_then(|host| host.active_turns());
        let store = app.state::<SessionStore>();
        let alarms = crate::alarms::Sources {
            live: &store.alarms,
            running: running.as_ref(),
        };
        let ticked = store.lock_conn().and_then(|conn| {
            clock
                .tick(&conn, &alarms)
                .map_err(|error| error.to_string())
        });
        // After the store lock is let go, as every command emits (design 5.3).
        match ticked {
            Ok(notices) => notices.emit(&app),
            Err(error) => eprintln!("Could not tick the active clock: {error}"),
        }
    });
    Ok(())
}

/// The computer's clocks. macOS: `CLOCK_UPTIME_RAW` stops during sleep and
/// `CLOCK_MONOTONIC` does not (`clock_gettime(3)`). Linux: `CLOCK_MONOTONIC`
/// does not count suspend and `CLOCK_BOOTTIME` does (`clock_gettime(2)`).
/// Windows: `QueryUnbiasedInterruptTime` leaves out sleep and hibernation,
/// and `GetTickCount64` counts them.
#[cfg(not(feature = "fake-clock"))]
pub(crate) struct SystemClock;

#[cfg(all(target_os = "macos", not(feature = "fake-clock")))]
mod system {
    extern "C" {
        // libSystem; the `libc` crate does not bind it.
        fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    }

    pub(super) fn uptime_ns() -> u64 {
        unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) }
    }

    pub(super) fn monotonic_ns() -> u64 {
        unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC) }
    }

    /// `sysctl kern.boottime`.
    pub(super) fn boot_wall_ms() -> i64 {
        let mut boot = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        let mut name = [libc::CTL_KERN, libc::KERN_BOOTTIME];
        let read = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                2,
                (&raw mut boot).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return 0;
        }
        boot.tv_sec * 1000 + i64::from(boot.tv_usec) / 1000
    }
}

#[cfg(all(unix, not(target_os = "macos"), not(feature = "fake-clock")))]
mod system {
    fn read(clock: libc::clockid_t) -> u64 {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(clock, &mut now) };
        u64::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000
            + u64::try_from(now.tv_nsec).unwrap_or(0)
    }

    pub(super) fn uptime_ns() -> u64 {
        read(libc::CLOCK_MONOTONIC)
    }

    pub(super) fn monotonic_ns() -> u64 {
        read(libc::CLOCK_BOOTTIME)
    }

    pub(super) fn boot_wall_ms() -> i64 {
        crate::mono_messages::now_ms() - super::ms(monotonic_ns())
    }
}

#[cfg(all(windows, not(feature = "fake-clock")))]
mod system {
    #[link(name = "kernel32")]
    extern "system" {
        fn QueryUnbiasedInterruptTime(time: *mut u64) -> i32;
        fn GetTickCount64() -> u64;
    }

    pub(super) fn uptime_ns() -> u64 {
        let mut time = 0u64;
        unsafe { QueryUnbiasedInterruptTime(&mut time) };
        time * 100
    }

    pub(super) fn monotonic_ns() -> u64 {
        let ms = unsafe { GetTickCount64() };
        ms * 1_000_000
    }

    pub(super) fn boot_wall_ms() -> i64 {
        crate::mono_messages::now_ms() - super::ms(monotonic_ns())
    }
}

#[cfg(not(feature = "fake-clock"))]
impl Clock for SystemClock {
    fn now(&mut self) -> Reading {
        Reading {
            uptime_ns: system::uptime_ns(),
            monotonic_ns: system::monotonic_ns(),
            wall_ms: crate::mono_messages::now_ms(),
            boot_wall_ms: system::boot_wall_ms(),
        }
    }
}

/// GigaEval builds only (`instance.sh build`): the clocks come from
/// `fake-clock.json` in the data folder, which `scripts/scenarios/app.sh`
/// writes, so a run moves the active clock without waiting. Test builds
/// compile it too, so CI's default-feature runs check it.
#[cfg(any(feature = "fake-clock", test))]
mod fake {
    use std::path::PathBuf;

    use super::Reading;

    pub(crate) struct FileClock {
        path: PathBuf,
        last: Reading,
    }

    impl FileClock {
        /// Fails when the file cannot be read, so a run with no clock file
        /// stops at launch instead of ticking on a default.
        pub(crate) fn open(path: PathBuf) -> Result<Self, String> {
            Ok(Self {
                last: read(&path)?,
                path,
            })
        }
    }

    fn read(path: &PathBuf) -> Result<Reading, String> {
        std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
            .map_err(|error| format!("fake clock {}: {error}", path.display()))
    }

    impl super::Clock for FileClock {
        /// The file once; a bad read keeps the last good reading.
        fn now(&mut self) -> Reading {
            match read(&self.path) {
                Ok(now) => self.last = now,
                Err(error) => eprintln!("{error}"),
            }
            self.last
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::Clock;
        use super::FileClock;

        // Defect: the clock file is read in the wrong unit, a launch with no
        // file ticks on a made-up reading, or a bad write mid-run stops the
        // clock instead of keeping the last good reading.
        #[test]
        fn a_file_clock_reads_the_driver_file_and_keeps_the_last_good_reading() {
            let dir = std::env::temp_dir().join(format!("file-clock-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("fake-clock.json");
            let _ = std::fs::remove_file(&path);
            assert!(FileClock::open(path.clone()).is_err(), "no file");
            let write = |up: u64, wall: i64| {
                std::fs::write(
                    &path,
                    format!(
                        r#"{{"uptime_ns":{up},"monotonic_ns":{},"wall_ms":{wall},"boot_wall_ms":7}}"#,
                        up + 1_000_000
                    ),
                )
                .unwrap();
            };
            write(1_000_000_000, 50);
            let mut clock = FileClock::open(path.clone()).unwrap();
            write(2_500_000_000, 60);
            let now = clock.now();
            assert_eq!(
                (
                    now.uptime_ns,
                    now.monotonic_ns,
                    now.wall_ms,
                    now.boot_wall_ms
                ),
                (2_500_000_000, 2_501_000_000, 60, 7)
            );
            std::fs::write(&path, "{").unwrap();
            assert_eq!(
                clock.now().wall_ms,
                60,
                "a bad write keeps the last reading"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR_MS: i64 = 3_600_000;

    fn log(conn: &Connection, (start, end): (i64, i64), cause: &str, active_ms: i64) {
        conn.execute(
            "INSERT INTO mono_downtime (start_wall_ms, end_wall_ms, cause, active_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![start, end, cause, active_ms],
        )
        .unwrap();
    }

    // Defect (lead, S1 r5 item 6): an active time before a logged gap maps
    // to a wall time late by that gap; or a gap logged at the active time
    // itself (the next tick after it) counts as before it; or a second
    // gap, or an app downtime with no sleep, is missed.
    #[test]
    fn a_wall_time_takes_out_every_gap_logged_from_its_active_time_on() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        // Active 0 at wall 1000. 8 h sleep logged at active 60 s, the app
        // down 1 h logged at active 120 s; now active 180 s.
        log(&conn, (61_000, 61_000 + 8 * HOUR_MS), "sleep", 60_000);
        let up = 121_000 + 8 * HOUR_MS;
        log(&conn, (up, up + HOUR_MS), "app_down", 120_000);
        let now = (180_000, 181_000 + 9 * HOUR_MS);
        let wall = |active| wall_at(&conn, active, now).unwrap();
        assert_eq!(wall(30_000), 31_000, "before both gaps");
        assert_eq!(wall(60_000), 61_000, "a gap logged at it came after it");
        assert_eq!(wall(90_000), 91_000 + 8 * HOUR_MS, "after the sleep only");
        assert_eq!(wall(150_000), 151_000 + 9 * HOUR_MS, "after both");
        assert_eq!(wall(180_000), now.1);
    }

    // Defect: a process without the lease reads the clock its holder
    // stored up to 5 s ago, so it takes a hold's answer after the cut; or
    // a holder that stopped leaves the clock frozen, so holds never time
    // out, or makes it run on with the wall clock.
    #[test]
    fn a_process_without_the_lease_reads_the_stored_clock_moved_on_by_wall_time() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        let at = |wall| active_at(None, &conn, wall).unwrap();
        assert_eq!(at(9_000), Active::Stopped(0), "no clock yet");
        conn.execute(
            "INSERT INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms)
             VALUES (1, 100000, 50000, 0)",
            [],
        )
        .unwrap();
        assert_eq!(at(50_000), Active::Running(100_000));
        assert_eq!(
            at(54_000),
            Active::Running(104_000),
            "4 s since the holder wrote it"
        );
        assert_eq!(
            at(49_000),
            Active::Running(100_000),
            "a wall clock set back adds nothing"
        );
        assert_eq!(at(56_000), Active::Running(106_000));
        assert_eq!(at(56_001), Active::Stopped(100_000), "the holder stopped");
        assert_eq!(at(500_000), Active::Stopped(100_000));
        assert_eq!(
            active_at(Some(7), &conn, 500_000).unwrap(),
            Active::Running(7),
            "the lease holder's own"
        );
    }
}
