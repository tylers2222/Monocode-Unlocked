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
//! limit read, MCP or connector setup, a session start), and no line follows
//! that. So a live home also syncs every `TICK` on its own timer (ADE-243),
//! and an app quit syncs each live home before and after it kills the
//! children (`kill_synced`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::codex_mono_store::home_key;

/// Every home's mirror, by home. Held only to find one, never across a
/// keyring call.
static HOMES: Mutex<BTreeMap<PathBuf, Arc<Home>>> = Mutex::new(BTreeMap::new());

/// How often a live home's timer syncs it. Codex refreshes a token at its
/// next `auth()` in the 5 minutes before expiry, the same 5 minutes in the
/// Mono and in the login's own Codex (the CLI, an IDE, the usage probe), and
/// an idle app-server lists models every 4 min 30 s. So each side refreshes
/// at a random moment in the window, and the login's side spends the revoked
/// refresh token when it refreshes within a tick after the Mono: about
/// TICK / 270 s, 0.7% at 2 s. An idle tick is 3 reads of the home.
const TICK: Duration = Duration::from_secs(2);

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

    /// Syncs the home every `every` while `timer` is its timer. The lock is
    /// let go while it waits.
    fn tick(&self, home: &Path, timer: u64, every: Duration) {
        let mut state = self.lock();
        loop {
            state = self
                .stopped
                .wait_timeout_while(state, every, |state| state.timer == timer)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            if state.timer != timer {
                return;
            }
            state.sync_logged(home);
        }
    }
}

/// A new store's share of `home`'s mirror. Only a spawn on an idle home copies
/// the login over the private entries: one on a home with a live store joins
/// it, as those entries are the live children's newest, and so does one on a
/// home whose last sync failed.
pub(crate) fn share(source: &Path, home: PathBuf) -> Result<Share, String> {
    share_every(source, home, TICK)
}

/// `share`, whose first store starts the home's timer, every `every`.
fn share_every(source: &Path, home: PathBuf, every: Duration) -> Result<Share, String> {
    let mirror = Home::of(&home);
    let mut state = mirror.lock();
    if state.live == 0 && !state.unsynced {
        #[cfg(all(test, target_os = "linux"))]
        tests::inside_prepare(&home);
        state.mirrors = prepare_keyring(source, &home)?;
        state.source = source.to_owned();
    }
    state.live += 1;
    let timer = (state.live == 1).then_some(state.timer);
    drop(state);
    if let Some(timer) = timer {
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

/// Syncs every live home, runs `kill` (the app's quit kills the children),
/// then syncs them again. The first copies a refresh since the last sync while
/// the app surely still runs; the second, one a child wrote as it died, which
/// its exit sync would race the app's own exit for.
pub(crate) fn kill_synced(kill: impl FnOnce()) {
    sync_live_homes();
    kill();
    sync_live_homes();
}

/// Never holds `HOMES` across a home's lock or a keyring call. A home with no
/// live store has no child left to write it, and its exit syncs ran.
fn sync_live_homes() {
    let homes: Vec<(PathBuf, Arc<Home>)> = HOMES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(home, mirror)| (home.clone(), Arc::clone(mirror)))
        .collect();
    for (home, mirror) in homes {
        #[cfg(test)]
        #[cfg(target_os = "linux")]
        if !tests::mine(&home) {
            continue;
        }
        let mut state = mirror.lock();
        if state.live > 0 {
            state.sync_logged(&home);
        }
    }
}

impl State {
    /// `sync`, with a failure logged and kept in `unsynced`.
    fn sync_logged(&mut self, home: &Path) {
        let synced = self.sync(home);
        if let Err(error) = &synced {
            eprintln!(
                "Could not copy the Mono Codex login back from {}: {error}",
                home.display()
            );
        }
        self.unsynced = synced.is_err();
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
