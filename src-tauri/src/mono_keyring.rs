//! A Mono home's keyring mirror (ADE-225 item 5). A Mono home is one per
//! login, not per session, so every live Codex on a login shares the home's
//! private keyring entries, and Codex rewrites them itself (a refreshed token,
//! a new secrets key) before a sync copies them to the login. So the mirror is
//! kept per home and shared by every live store on it:
//! - a spawn on a home with a live store joins it and never copies the login
//!   over the live entries, nor deletes one;
//! - a sync by any store moves the one baseline all of them check;
//! - an entry a child makes after the prepare (its first secrets key) joins.
//!
//! Left: a login changed outside the app (`codex login`) while a Mono on that
//! home is live. The live Mono keeps its own entries, and no sync copies them
//! over the new login, which wins from the next spawn on the idle home.
//!
//! A sync reads the home's 3 entries, and a login entry only after a child
//! changed its home entry (ADE-241). macOS may prompt MonoCode for an entry
//! Codex created: each login entry, and a home entry the child made itself
//! because the login had none at prepare. A home entry MonoCode created stays
//! MonoCode's when Codex updates it in place, so it does not prompt.
//!
//! A sync runs on every `turn/completed`, `item/started` and `account/updated`
//! line of a child, and at its exit (`harness.rs`). Codex refreshes a token
//! before each model request, so inside a turn, but also outside one (a rate
//! limit read, the model list an idle app-server fetches every 4 min 30 s, MCP
//! or connector setup), and no line follows that. A refresh rewrites only
//! `Codex Auth`, never the two passphrase entries. So while a home is live, a
//! timer reads its `Codex Auth` entry every `TIMER.every` (ADE-243), and an app
//! quit reads it once more after it kills the children (`kill_synced`). Both
//! read only an entry MonoCode last wrote at prepare (`owned`), and stop at
//! the first slow read (a prompt), so an entry that prompts costs one prompt
//! a live period at most. A changed entry is copied as a line sync copies it:
//! that reads the login's entry, which Codex created, under the home lock,
//! and that read can prompt, as on the lines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use crate::codex_mono_store::home_key;

/// Every home's mirror, by home. Held only to find one, never across a
/// keyring call.
static HOMES: Mutex<BTreeMap<PathBuf, Arc<Home>>> = Mutex::new(BTreeMap::new());

/// A live home's timer: how often it reads the home's `Codex Auth`, and how
/// long a read may take before the timer takes it for a prompt and stops.
#[derive(Clone, Copy)]
struct Timer {
    every: Duration,
    slow: Duration,
}

/// Codex refreshes a token at its next `auth()` in the 5 minutes before
/// expiry, the same 5 minutes in the Mono and in the login's own Codex (the
/// CLI, an IDE, the usage probe), and an idle app-server calls it every
/// 4 min 30 s. When the Mono refreshes first, the login's side spends the
/// revoked refresh token if it refreshes before the copy: within a tick, about
/// `every` / 270 s, 0.7% at 2 s. When the login's side refreshes first, the
/// live Mono spends it whatever the tick (module doc, "Left"). An idle tick
/// reads 1 home entry, which MonoCode wrote.
const TIMER: Timer = Timer {
    every: Duration::from_secs(2),
    slow: Duration::from_secs(1),
};

/// How long an app quit waits for the homes' last sync, in all.
const QUIT_BUDGET: Duration = Duration::from_secs(2);

/// One home's mirror. Its lock is held across the keyring calls of a spawn's
/// preparation and of the children's syncs (stdout, timer and exit), so those
/// never interleave on that home; spawns on other homes never wait on it.
/// `prepare_files` and the `codex_mono_store_prepare` command touch no keyring
/// entry, so they run outside it.
#[derive(Default)]
struct Home {
    state: Mutex<State>,
    /// Wakes the home's timer when its last store drops.
    stopped: Condvar,
}

#[derive(Default)]
struct State {
    /// Stores alive on the home.
    live: usize,
    /// Moves when the last store drops, which stops the timer started with
    /// the old value: one timer per home, even across a quick drop and spawn.
    timer: u64,
    /// A timer read failed, so the timer skips the home until a line or exit
    /// sync succeeds, or a prepare.
    paused: bool,
    /// A quit synced the home after its children were killed, and nothing
    /// spawned on it since: a second reap of the same quit skips it.
    reaped: bool,
    /// The last sync failed, so the private entries may be newer than the login.
    unsynced: bool,
    /// The login the home mirrors.
    source: PathBuf,
    mirrors: Vec<KeyringMirror>,
}

/// One live store's share of its home's mirror.
pub(crate) struct Share {
    home: PathBuf,
    mirror: Arc<Home>,
}

impl Home {
    fn of(home: &Path) -> Arc<Self> {
        let mut homes = HOMES.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(homes.entry(home.to_owned()).or_default())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reads the home's `Codex Auth` every `every` while `timer` is its
    /// timer, with no lock held across the read, so a slow one (a prompt)
    /// never stalls the lines' syncs. The lock is taken after it, to compare,
    /// and to copy the value read, which reads the login's entry under it.
    /// The check before each wait holds the lock until the wait lets it go,
    /// so a drop's wake is never missed; a spurious wake is one more read.
    fn tick(&self, home: &Path, timer: u64, every: Timer) {
        #[cfg(test)]
        #[cfg(target_os = "linux")]
        let _alive = tests::Alive::new(home);
        loop {
            let state = self.lock();
            if state.timer != timer {
                return;
            }
            #[cfg(test)]
            #[cfg(target_os = "linux")]
            tests::parking(home);
            let state = self
                .stopped
                .wait_timeout(state, every.every)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            if state.timer != timer {
                return;
            }
            let entry = state.auth_key().filter(|_| !state.paused);
            drop(state);
            if let Some((key, seen)) = entry {
                let started = Instant::now();
                let read = read_keyring("Codex Auth", &key);
                let slow = started.elapsed() >= every.slow;
                let mut state = self.lock();
                if state.timer == timer {
                    state.take_auth(home, &key, &seen, read, slow);
                }
            }
        }
    }
}

/// A new store's share of `home`'s mirror. Only a spawn on an idle home copies
/// the login over the private entries: one on a home with a live store joins
/// it, as those entries are the live children's newest, and so does one on a
/// home whose last sync failed.
pub(crate) fn share(source: &Path, home: PathBuf) -> Result<Share, String> {
    share_every(source, home, TIMER)
}

/// `share`, whose first store starts the home's timer when it has a
/// `Codex Auth` entry MonoCode wrote.
fn share_every(source: &Path, home: PathBuf, every: Timer) -> Result<Share, String> {
    let mirror = Home::of(&home);
    let mut state = mirror.lock();
    if state.live == 0 && !state.unsynced {
        #[cfg(all(test, target_os = "linux"))]
        tests::inside_prepare(&home);
        state.mirrors = prepare_keyring(source, &home)?;
        state.source = source.to_owned();
        state.paused = false;
    }
    state.live += 1;
    state.reaped = false;
    let timer = (state.live == 1 && state.auth_key().is_some()).then_some(state.timer);
    drop(state);
    if let Some(timer) = timer {
        #[cfg(test)]
        #[cfg(target_os = "linux")]
        tests::starting(&home);
        let (ticking, path) = (Arc::clone(&mirror), home.clone());
        let started = std::thread::Builder::new()
            .name("mono-keyring-sync".into())
            .spawn(move || ticking.tick(&path, timer, every));
        // The lines and the exit still sync the home.
        if let Err(error) = started {
            eprintln!(
                "Could not start the Mono Codex login sync for {}: {error}",
                home.display()
            );
        }
    }
    Ok(Share { home, mirror })
}

impl Share {
    /// Copies what the home's children wrote to the login, unless the login
    /// changed on its own since the last sync. A failure is logged, and keeps
    /// the home joined, not prepared again, until a sync succeeds.
    pub(crate) fn sync(&self) {
        self.mirror.lock().sync_logged(&self.home);
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        let mut state = self.mirror.lock();
        state.live -= 1;
        if state.live == 0 {
            state.timer += 1;
            self.mirror.stopped.notify_all();
        }
    }
}

/// Runs `kill` (the app's quit kills the children), then reads the
/// `Codex Auth` of each home with a live store, no reap since its last spawn,
/// and an owned entry, once, as a tick does, paused or not: a refresh a child
/// wrote since its last sync or as it died, as its exit sync races the app's
/// own exit. Other homes and the passphrase entries have only that exit sync.
/// The kill never waits on the keyring. The sync runs on a worker, and the
/// quit waits for it `QUIT_BUDGET` at most: a home whose lock stays busy (a
/// sync at a prompt) is skipped, and so is a read past the budget.
pub(crate) fn kill_synced(kill: impl FnOnce()) {
    kill();
    let deadline = Instant::now() + QUIT_BUDGET;
    let homes = live_homes();
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    let homes = tests::mine(homes);
    let (done, finished) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("mono-keyring-quit".into())
        .spawn(move || {
            reap(homes, deadline);
            let _ = done.send(());
        });
    let waited = match worker {
        Ok(_) => finished.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        Err(_) => Err(mpsc::RecvTimeoutError::Disconnected),
    };
    if waited.is_err() {
        eprintln!("Could not copy every Mono Codex login back before the quit");
    }
}

/// Every home. Never holds `HOMES` across a home's lock.
fn live_homes() -> Vec<(PathBuf, Arc<Home>)> {
    HOMES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(home, mirror)| (home.clone(), Arc::clone(mirror)))
        .collect()
}

/// Syncs each live home's `Codex Auth`, skipping a busy home until it is free
/// or `deadline` passes, so one busy home never holds up the others. A home
/// with no live store has no child left to write it, and its exit syncs ran;
/// a reaped one was synced by this quit already.
fn reap(mut homes: Vec<(PathBuf, Arc<Home>)>, deadline: Instant) {
    loop {
        homes.retain(|(home, mirror)| {
            let mut state = match mirror.state.try_lock() {
                Ok(state) => state,
                Err(TryLockError::Poisoned(e)) => e.into_inner(),
                Err(TryLockError::WouldBlock) => return true,
            };
            if state.live > 0 && !state.reaped {
                if let Some((key, seen)) = state.auth_key() {
                    let read = read_keyring("Codex Auth", &key);
                    state.take_auth(home, &key, &seen, read, false);
                }
                state.reaped = true;
            }
            false
        });
        if homes.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for (home, _) in homes {
        eprintln!(
            "Could not copy the Mono Codex login back from {} before the quit: busy",
            home.display()
        );
    }
}

impl State {
    /// `sync`, with a failure logged once a streak and kept in `unsynced`. A
    /// success resumes a paused timer.
    fn sync_logged(&mut self, home: &Path) {
        match self.sync(home) {
            Ok(()) => {
                self.unsynced = false;
                self.paused = false;
            }
            Err(error) => self.failed(home, &error),
        }
    }

    fn failed(&mut self, home: &Path, error: &str) {
        if !self.unsynced {
            warn(format!(
                "Could not copy the Mono Codex login back from {}: {error}",
                home.display()
            ));
        }
        self.unsynced = true;
    }

    /// The home's `Codex Auth` key, and the value the last sync acted on,
    /// when it is owned: MonoCode wrote the entry at prepare, and no timer
    /// read since was slow or found none. One Codex created may prompt, so
    /// only the lines read it.
    fn auth_key(&self) -> Option<(String, Option<String>)> {
        self.mirrors
            .iter()
            .find(|mirror| mirror.owned && mirror.service == "Codex Auth")
            .map(|mirror| (mirror.private_key.clone(), mirror.private_value.clone()))
    }

    /// Acts on a timer's or the quit's `read` of `key`, begun when the last
    /// sync had acted on `seen`: copies the value read, if it changed, as a
    /// sync does, with no second read. If another sync acted since, the next
    /// tick reads again: the value read may be older than the login's now.
    /// A failed read or copy pauses the timer, and a failed copy is also
    /// unsynced. A `slow` read was a prompt and a missing entry is one Codex
    /// will create, so neither is owned until the next prepare. A copy never
    /// clears `unsynced`: the passphrase entries are the lines' to sync.
    fn take_auth(
        &mut self,
        home: &Path,
        key: &str,
        seen: &Option<String>,
        read: Result<Option<String>, String>,
        slow: bool,
    ) {
        let Some(mirror) = self.mirrors.iter_mut().find(|m| m.private_key == key) else {
            return;
        };
        let value = match read {
            Ok(value) => value,
            Err(error) => {
                if !self.paused {
                    warn(format!(
                        "Paused the Mono Codex login timer for {}: {error}",
                        home.display()
                    ));
                }
                self.paused = true;
                return;
            }
        };
        if slow || value.is_none() {
            mirror.owned = false;
        }
        if mirror.private_value != *seen {
            return;
        }
        if let Err(error) = mirror.take(value) {
            self.paused = true;
            self.failed(home, &error);
        }
    }

    fn sync(&mut self, home: &Path) -> Result<(), String> {
        if self.mirrors.is_empty() && has_secrets(&self.source) {
            self.mirrors = login_mirrors(&self.source, home)?;
        }
        let mut synced = Ok(());
        for mirror in &mut self.mirrors {
            if let Err(error) = mirror.sync() {
                synced = Err(error);
            }
        }
        synced
    }
}

/// Logs a sync problem once a streak. Never logs a credential.
fn warn(message: String) {
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    tests::count_warning(&message);
    eprintln!("{message}");
}

/// The keyring entries a Mono home mirrors from its login: service, key prefix
/// and suffix, around each folder's `home_key`.
pub(crate) const MIRRORED: [(&str, &str, &str); 3] = [
    ("Codex Auth", "cli", ""),
    ("codex", "secrets", ""),
    ("codex", "secrets", "|gateway-oauth"),
];

pub(crate) struct KeyringMirror {
    pub(crate) service: &'static str,
    pub(crate) source_key: String,
    pub(crate) private_key: String,
    // Never serialize or log credential contents.
    pub(crate) source_value: Option<String>,
    /// The home's value the last sync acted on. Never logged either.
    pub(crate) private_value: Option<String>,
    /// MonoCode last wrote the home's entry, at prepare: a best guess that
    /// reading it does not prompt. A write in place keeps the item's creator,
    /// and one Codex created may prompt, so a slow timer read, or one that
    /// finds no entry, means it is not MonoCode's to poll until the next
    /// prepare. Recreating the item instead would drop codex's own grant.
    pub(crate) owned: bool,
}

/// The home's mirror entries, each with the login's value now as its baseline.
/// A home value equal to it has nothing to copy, as a sync of an equal pair
/// only keeps the baseline.
fn login_mirrors(source: &Path, home: &Path) -> Result<Vec<KeyringMirror>, String> {
    let (source_hash, private_hash) = (home_key(source)?, home_key(home)?);
    MIRRORED
        .into_iter()
        .map(|(service, prefix, suffix)| {
            let source_key = format!("{prefix}|{source_hash}{suffix}");
            let source_value = read_keyring(service, &source_key)?;
            Ok(KeyringMirror {
                service,
                owned: false,
                private_value: source_value.clone(),
                source_value,
                source_key,
                private_key: format!("{prefix}|{private_hash}{suffix}"),
            })
        })
        .collect()
}

impl KeyringMirror {
    /// Reads the home's entry, and the login's only when the home's changed
    /// since the last sync acted on it: until then no child wrote it, and a
    /// login changed on its own is never copied over.
    fn sync(&mut self) -> Result<(), String> {
        let private = read_keyring(self.service, &self.private_key)?;
        self.take(private)
    }

    /// `sync` of the home's value `private`, read already.
    fn take(&mut self, private: Option<String>) -> Result<(), String> {
        if private == self.private_value {
            return Ok(());
        }
        let source = read_keyring(self.service, &self.source_key)?;
        if private == source {
            self.source_value = source;
        } else if source == self.source_value || newer_same_account(&private, &source) {
            if let Some(value) = &private {
                write_keyring(self.service, &self.source_key, value)?;
                self.source_value = private.clone();
            }
        }
        self.private_value = private;
        Ok(())
    }
}

pub(crate) fn read_keyring(service: &str, key: &str) -> Result<Option<String>, String> {
    #[cfg(all(test, target_os = "linux"))]
    tests::count_read(key);
    let entry =
        keyring::Entry::new(service, key).map_err(|_| "Could not access Codex credentials")?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("Could not read Codex credentials from the system credential store".into()),
    }
}

pub(crate) fn write_keyring(service: &str, key: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new(service, key)
        .and_then(|e| e.set_password(value))
        .map_err(|_| "Could not share Codex credentials with Mono storage".into())
}

/// Deletes the home's own entry. A missing one is already gone.
fn delete_private(service: &str, private_key: &str) -> Result<(), String> {
    #[cfg(all(test, target_os = "linux"))]
    tests::count_delete(private_key);
    if let Ok(entry) = keyring::Entry::new(service, private_key) {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(_) => return Err("Could not clear the previous Mono Codex login".into()),
        }
    }
    Ok(())
}

pub(crate) fn prepare_keyring(source: &Path, home: &Path) -> Result<Vec<KeyringMirror>, String> {
    // The default file store shares auth.json directly, with no Keychain prompt.
    let store = credentials_store(source);
    let explicit_keyring = store
        .as_deref()
        .is_some_and(|v| v == "keyring" || v == "auto");
    let encrypted = has_secrets(source);
    if !explicit_keyring && !encrypted {
        return Ok(Vec::new());
    }
    // Codex's "auto" store reads auth.json, shared already, when the keyring
    // cannot be read; "keyring" and an encrypted secret's key have no fallback.
    // Codex in the Mono reads the home's own entry first, so the copy an
    // earlier spawn left there goes, or it wins over auth.json.
    let fallback = !encrypted && store.as_deref() == Some("auto");
    let source_hash = home_key(source)?;
    let private_hash = home_key(home)?;
    let mut mirrors = Vec::new();
    for (service, prefix, suffix) in MIRRORED {
        let source_key = format!("{prefix}|{source_hash}{suffix}");
        let private_key = format!("{prefix}|{private_hash}{suffix}");
        let value = match read_keyring(service, &source_key) {
            Ok(value) => value,
            Err(_) if fallback => {
                delete_private(service, &private_key)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let value = if service == "Codex Auth" {
            let private = read_keyring(service, &private_key)?;
            if newer_same_account(&private, &value) {
                let recovered = private.as_ref().unwrap();
                write_keyring(service, &source_key, recovered)?;
                private
            } else {
                value
            }
        } else {
            value
        };
        if let Some(value) = &value {
            write_keyring(service, &private_key, value)?;
        } else {
            delete_private(service, &private_key)?;
        }
        mirrors.push(KeyringMirror {
            service,
            source_key,
            private_key,
            owned: value.is_some(),
            private_value: value.clone(),
            source_value: value,
        });
    }
    Ok(mirrors)
}

/// The login's `cli_auth_credentials_store` in config.toml, which is a whole
/// TOML document: `toml::Value` parses one value, never a file.
fn credentials_store(source: &Path) -> Option<String> {
    let config = std::fs::read_to_string(source.join("config.toml")).ok()?;
    let config = config.parse::<toml::Table>().ok()?;
    config
        .get("cli_auth_credentials_store")?
        .as_str()
        .map(str::to_owned)
}

/// Codex has encrypted a secret for the login, under a key in the keyring.
pub(crate) fn has_secrets(source: &Path) -> bool {
    std::fs::read_dir(source.join("secrets"))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

pub(crate) fn newer_same_account(private: &Option<String>, source: &Option<String>) -> bool {
    let Some((private, source)) = private.as_ref().zip(source.as_ref()) else {
        return false;
    };
    let Ok(private) = serde_json::from_str::<serde_json::Value>(private) else {
        return false;
    };
    let Ok(source) = serde_json::from_str::<serde_json::Value>(source) else {
        return false;
    };
    let account = |v: &serde_json::Value| {
        v.get("tokens")
            .and_then(|v| v.get("account_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let Some(private_account) = account(&private) else {
        return false;
    };
    if Some(private_account) != account(&source) {
        return false;
    }
    private
        .get("last_refresh")
        .and_then(|v| v.as_str())
        .zip(source.get("last_refresh").and_then(|v| v.as_str()))
        .is_some_and(|(private, source)| private > source)
}

/// On keyutils, never a real Keychain. Two attributes, not `all(test, ..)`:
/// cargo-mutants skips only a `cfg` that names `test` at its top level.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
