use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use rusqlite::Connection;

use crate::mono_messages::{self, SessionId, Targeted};
use crate::session_store::SessionStore;

/// Names a `.backup` copy of a real `monocode.db` under `/tmp` that a World
/// starts from instead of an empty database.
pub(crate) const SCENARIO_DB: &str = "MONOCODE_SCENARIO_DB";

/// The fake wall time a World starts at (2025-10-09), so stored times look
/// like real ones next to a real database's rows.
const START_MS: i64 = 1_760_000_000_000;

/// The window label every hand-off slot names; a World is one window.
const WINDOW: &str = "main";

/// The app on a file database at a fake wall time. `running` is the turn
/// each session runs now, as `ControlHost` reports it to the store.
pub(crate) struct World {
    dir: PathBuf,
    path: PathBuf,
    store: Option<SessionStore>,
    running: HashMap<String, String>,
    /// The fake wall time every call is made at; only `advance` moves it.
    pub(crate) now: i64,
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
            // Never the live database: `startup` would change it.
            let source = Path::new(&source);
            assert!(
                source.starts_with("/tmp") || source.starts_with("/private/tmp"),
                "{SCENARIO_DB} must name a `sqlite3 .backup` copy under /tmp"
            );
            std::fs::copy(source, &path).unwrap();
        }
        let mut world = Self {
            dir,
            path,
            store: None,
            running: HashMap::new(),
            now: START_MS,
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

    pub(crate) fn advance(&mut self, ms: i64) {
        self.now += ms;
    }

    /// The app stops at once: the store is dropped and no app shutdown path
    /// runs, so only what the store committed survives. No turn outlives it.
    pub(crate) fn kill(&mut self) {
        assert!(self.store.take().is_some(), "the app is not running");
        self.running.clear();
    }

    /// The app starts again on the same file, through the app's own open
    /// path (migrate, then `startup` at the World's wall time).
    pub(crate) fn restart(&mut self) {
        assert!(self.store.is_none(), "the app is already running");
        self.store = Some(SessionStore::open_at(self.path.clone(), self.now).unwrap());
    }

    pub(crate) fn store(&self) -> &SessionStore {
        self.store.as_ref().expect("the app is not running")
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
    /// provider has not taken the text yet; see [`World::provider_took`].
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
                    self.now,
                )
                .unwrap();
                slotted
            })
            .collect()
    }

    /// The provider runs turn `turn` of `session`, so it took the text that
    /// opened it (`mono_message_confirm_openers`).
    pub(crate) fn provider_took(&self, session: &SessionId, turn: &str) {
        mono_messages::confirm_openers(&self.conn(), session, turn, self.now).unwrap();
    }

    /// Turn `turn` of `session` ends (`turn_ended`).
    pub(crate) fn end_turn(&mut self, session: &SessionId, turn: &str) {
        self.running.remove(session.as_str());
        mono_messages::turn_ended(&self.conn(), session, turn, self.now).unwrap();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Close the database before its folder goes (Windows keeps open
        // files).
        self.store = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
