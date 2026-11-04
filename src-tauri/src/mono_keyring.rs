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
//! limit read, MCP or connector setup, a session start). No line follows such
//! a refresh, so it waits for the child's next trigger line or its exit
//! (follow-up ADE-243: a timer sync while a home is live).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::codex_mono_store::home_key;

/// Every home's mirror, by home. Held only to find one, never across a
/// keyring call.
static HOMES: Mutex<BTreeMap<PathBuf, Arc<Home>>> = Mutex::new(BTreeMap::new());

/// One home's mirror. Its lock is held across the keyring calls of a spawn's
/// preparation and of the children's syncs (stdout and exit), so those never
/// interleave on that home; spawns on other homes never wait on it.
/// `prepare_files` and the `codex_mono_store_prepare` command touch no keyring
/// entry, so they run outside it.
#[derive(Default)]
struct Home(Mutex<State>);

#[derive(Default)]
struct State {
    /// Stores alive on the home.
    live: usize,
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
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A new store's share of `home`'s mirror. Only a spawn on an idle home copies
/// the login over the private entries: one on a home with a live store joins
/// it, as those entries are the live children's newest, and so does one on a
/// home whose last sync failed.
pub(crate) fn share(source: &Path, home: PathBuf) -> Result<Share, String> {
    let mirror = Home::of(&home);
    let mut state = mirror.lock();
    if state.live == 0 && !state.unsynced {
        #[cfg(all(test, target_os = "linux"))]
        tests::inside_prepare(&home);
        state.mirrors = prepare_keyring(source, &home)?;
        state.source = source.to_owned();
    }
    state.live += 1;
    drop(state);
    Ok(Share { home, mirror })
}

impl Share {
    /// Copies what the home's children wrote to the login, unless the login
    /// changed on its own since the last sync. A failure is logged, and keeps
    /// the home joined, not prepared again, until a sync succeeds.
    pub(crate) fn sync(&self) {
        let mut state = self.mirror.lock();
        let synced = state.sync(&self.home);
        if let Err(error) = &synced {
            eprintln!(
                "Could not copy the Mono Codex login back from {}: {error}",
                self.home.display()
            );
        }
        state.unsynced = synced.is_err();
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.mirror.lock().live -= 1;
    }
}

impl State {
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
mod tests {
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A login with its Mono home, and the keyring entries of both.
    struct Login {
        root: PathBuf,
        source: PathBuf,
        home: PathBuf,
    }

    impl Login {
        fn new(secret: bool) -> Self {
            keyring::set_default_credential_builder(keyring::keyutils::default_credential_builder());
            let root =
                std::env::temp_dir().join(format!("ade225-keyring-{}", uuid::Uuid::new_v4()));
            let (source, home) = (root.join("login"), root.join("home"));
            crate::codex_mono_store::prepare_files(&source, &home).unwrap();
            let login = Login { root, source, home };
            if secret {
                login.seal();
            }
            login
        }

        /// Codex encrypts a secret, which turns the mirror on.
        fn seal(&self) {
            std::fs::write(self.source.join("secrets/blob"), "sealed").unwrap();
        }

        fn share(&self) -> Share {
            share(&self.source, self.home.clone()).unwrap()
        }

        /// The `codex` entry with `suffix` in the login (`private` false) or the home.
        fn key(&self, private: bool, suffix: &str) -> String {
            let dir = if private { &self.home } else { &self.source };
            format!("secrets|{}{suffix}", home_key(dir).unwrap())
        }

        /// The `Codex Auth` entry in the login (`private` false) or the home.
        fn auth_key(&self, private: bool) -> String {
            let dir = if private { &self.home } else { &self.source };
            format!("cli|{}", home_key(dir).unwrap())
        }

        fn write(&self, private: bool, suffix: &str, value: &str) {
            write_keyring("codex", &self.key(private, suffix), value).unwrap();
        }

        /// The entry with `suffix` in the login, then in the home.
        fn read(&self, suffix: &str) -> [Option<String>; 2] {
            [false, true].map(|private| read_keyring("codex", &self.key(private, suffix)).unwrap())
        }

        /// The home's 3 entries, in the order a sync reads them.
        fn home_keys(&self) -> Vec<String> {
            vec![
                self.auth_key(true),
                self.key(true, ""),
                self.key(true, GATEWAY),
            ]
        }
    }

    impl Drop for Login {
        fn drop(&mut self) {
            for private in [false, true] {
                for suffix in ["", GATEWAY] {
                    let _ = keyring::Entry::new("codex", &self.key(private, suffix))
                        .and_then(|entry| entry.delete_credential());
                }
                let _ = keyring::Entry::new("Codex Auth", &self.auth_key(private))
                    .and_then(|entry| entry.delete_credential());
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const GATEWAY: &str = "|gateway-oauth";

    type Held = (mpsc::Sender<()>, mpsc::Receiver<()>);

    /// Homes whose next prepare says it started, then waits to be let go.
    static HELD: Mutex<BTreeMap<PathBuf, Held>> = Mutex::new(BTreeMap::new());

    pub(super) fn inside_prepare(home: &Path) {
        let held = HELD.lock().unwrap().remove(home);
        if let Some((started, go)) = held {
            started.send(()).unwrap();
            go.recv().unwrap();
        }
    }

    fn both(value: &str) -> [Option<String>; 2] {
        [Some(value.to_owned()), Some(value.to_owned())]
    }

    thread_local! {
        /// The keys this thread read while `reads` records them.
        static READS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    pub(super) fn count_read(key: &str) {
        READS.with(|reads| {
            if let Some(reads) = reads.borrow_mut().as_mut() {
                reads.push(key.to_owned());
            }
        });
    }

    /// The keyring keys `run` reads on this thread, in order.
    fn reads(run: impl FnOnce()) -> Vec<String> {
        READS.with(|reads| *reads.borrow_mut() = Some(Vec::new()));
        run();
        READS.with(|reads| reads.borrow_mut().take().unwrap_or_default())
    }

    /// Defect (ADE-241): every sync read the login's 3 entries and the home's
    /// 3, though only a child's write changes anything; macOS may prompt for
    /// each read of a login entry Codex wrote. Or the sync skips the home's
    /// changed entry, and a child's refresh never reaches the login.
    #[test]
    fn a_sync_reads_the_login_only_after_a_child_wrote() {
        let login = Login::new(true);
        login.write(false, GATEWAY, "token-1");
        let a = login.share();
        let idle = reads(|| a.sync());
        login.write(true, GATEWAY, "token-2");
        let written = reads(|| a.sync());
        let after = reads(|| a.sync());
        let mut expected = login.home_keys();
        expected.push(login.key(false, GATEWAY));
        assert_eq!(
            (idle, written, after, login.read(GATEWAY)),
            (
                login.home_keys(),
                expected,
                login.home_keys(),
                both("token-2")
            )
        );
        drop(a);
    }

    /// Defect (#526 r1 m1): a sync that fails after reading the home still
    /// records the home's value, so the next sync finds it unchanged, reports
    /// success, and the child's refresh never reaches the login. A value that
    /// is not UTF-8 makes the login read fail on keyutils.
    #[test]
    fn a_failed_sync_retries_the_copy_with_the_home_unchanged() {
        let login = Login::new(true);
        login.write(false, GATEWAY, "token-1");
        let a = login.share();
        login.write(true, GATEWAY, "token-2");
        keyring::Entry::new("codex", &login.key(false, GATEWAY))
            .and_then(|e| e.set_secret(&[0xff, 0xfe]))
            .unwrap();
        a.sync();
        let failed = a.mirror.lock().unsynced;
        login.write(false, GATEWAY, "token-1");
        a.sync();
        let retried = a.mirror.lock().unsynced;
        assert_eq!(
            (failed, retried, login.read(GATEWAY)),
            (true, false, both("token-2"))
        );
        drop(a);
    }

    /// Defect: a sync copies a child's refresh over a login that changed on
    /// its own, or skips one that is a newer refresh of the same account.
    #[test]
    fn a_sync_copies_over_a_changed_login_only_a_newer_same_account_refresh() {
        let auth = |account: &str, at: &str| {
            format!(
                r#"{{"tokens":{{"account_id":"{account}"}},"last_refresh":"2026-11-04T{at}:00:00Z"}}"#
            )
        };
        let mut found = Vec::new();
        for (login_value, home_value) in [
            (auth("one", "12"), auth("one", "13")),
            (auth("two", "12"), auth("one", "13")),
            (auth("one", "12"), auth("one", "11")),
        ] {
            let login = Login::new(true);
            write_keyring("Codex Auth", &login.auth_key(false), &auth("one", "10")).unwrap();
            let a = login.share();
            write_keyring("Codex Auth", &login.auth_key(false), &login_value).unwrap();
            write_keyring("Codex Auth", &login.auth_key(true), &home_value).unwrap();
            a.sync();
            found.push(read_keyring("Codex Auth", &login.auth_key(false)).unwrap());
            drop(a);
        }
        assert_eq!(
            found,
            [
                Some(auth("one", "13")),
                Some(auth("two", "12")),
                Some(auth("one", "12")),
            ]
        );
    }

    /// Defect (#521 r1 B1): each store kept its own baseline of the login, so
    /// after store B synced, live store A's newer write read as a login change
    /// and was skipped, and the next spawn copied B's older token over it.
    #[test]
    fn a_write_survives_after_another_store_on_the_home_synced() {
        let login = Login::new(true);
        login.write(false, GATEWAY, "token-1");
        let a = login.share();
        let b = login.share();
        login.write(true, GATEWAY, "token-2");
        b.sync();
        drop(b);
        login.write(true, GATEWAY, "token-3");
        let c = login.share();
        c.sync();
        assert_eq!(login.read(GATEWAY), both("token-3"));
        drop((a, c));
    }

    /// Defect (#521 r1 B2): a home prepared while the login had no secret
    /// mirrored nothing, so the next spawn deleted the first secrets key a live
    /// child made, and its encrypted secret had no key anywhere.
    #[test]
    fn a_first_secrets_key_survives_the_next_spawn() {
        let login = Login::new(false);
        let a = login.share();
        login.write(true, "", "key-a");
        login.seal();
        let b = login.share();
        b.sync();
        assert_eq!(login.read(""), both("key-a"));
        drop((a, b));
    }

    /// Defect: a spawn on a home with a live store copies the login over the
    /// live child's entries, or one on an idle home keeps the old ones. Also the
    /// login changed outside the app while a Mono is live: it keeps the user's
    /// new login, the Mono its own, until the home is idle.
    #[test]
    fn a_joining_spawn_keeps_the_live_entries_and_an_idle_one_takes_the_login() {
        let login = Login::new(true);
        login.write(false, GATEWAY, "token-1");
        let a = login.share();
        login.write(true, GATEWAY, "token-2");
        login.write(false, GATEWAY, "token-x");
        let b = login.share();
        b.sync();
        let live = login.read(GATEWAY);
        drop((a, b));
        let c = login.share();
        assert_eq!(
            [live, login.read(GATEWAY)],
            [
                [Some("token-x".to_owned()), Some("token-2".to_owned())],
                both("token-x")
            ]
        );
        drop(c);
    }

    /// Defect (#521 r1 M1): one lock for every home makes a spawn wait on
    /// another login's Keychain prompt; or a login on the file store touches
    /// its keyring entries.
    #[test]
    fn a_spawn_waits_only_on_its_own_home() {
        let (busy, file_store) = (Login::new(true), Login::new(false));
        file_store.write(true, GATEWAY, "kept");
        let held = Home::of(&busy.home);
        let guard = held.lock();
        let (source, home) = (busy.source.clone(), busy.home.clone());
        let waiting = std::thread::spawn(move || drop(share(&source, home).unwrap()));
        // Let it reach the held lock first, as a spawn waiting on a prompt has.
        std::thread::sleep(Duration::from_millis(500));
        let (source, home) = (file_store.source.clone(), file_store.home.clone());
        let (send, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(share(&source, home).map(drop)));
        let other = done.recv_timeout(Duration::from_secs(20));
        drop(guard);
        waiting.join().unwrap();
        assert!(
            matches!(other, Ok(Ok(()))),
            "a spawn on another home waited"
        );
        assert_eq!(file_store.read(GATEWAY)[1].as_deref(), Some("kept"));
    }

    /// Defect (#521 r2 m1): a failed sync must keep the home joined, or the
    /// next spawn on the idle home copies the login over the live child's key.
    /// Hiding the home makes `home_key` fail inside the sync's first mirror build.
    #[test]
    fn a_failed_sync_keeps_the_live_key_on_the_idle_home() {
        let login = Login::new(false);
        let a = login.share();
        login.write(true, "", "key-a");
        login.seal();
        let hidden = login.root.join("hidden");
        std::fs::rename(&login.home, &hidden).unwrap();
        a.sync();
        std::fs::rename(&hidden, &login.home).unwrap();
        drop(a);
        let b = login.share();
        assert_eq!(login.read("")[1].as_deref(), Some("key-a"));
        drop(b);
    }

    /// Defect (#521 r2 m2): the home's lock is let go across the prepare, so a
    /// second spawn on the idle home also sees no live store and prepares,
    /// copying the login over what the first spawn's child writes.
    #[test]
    fn a_second_spawn_waits_for_the_first_ones_prepare() {
        let login = Login::new(false);
        let (started, start) = mpsc::channel();
        let (go, wait) = mpsc::channel();
        HELD.lock()
            .unwrap()
            .insert(login.home.clone(), (started, wait));
        let (source, home) = (login.source.clone(), login.home.clone());
        let first = std::thread::spawn(move || share(&source, home).unwrap());
        start.recv_timeout(Duration::from_secs(20)).unwrap();
        let (source, home) = (login.source.clone(), login.home.clone());
        let (send, done) = mpsc::channel();
        std::thread::spawn(move || send.send(share(&source, home).map(drop)));
        let early = done.recv_timeout(Duration::from_millis(500));
        go.send(()).unwrap();
        let a = first.join().unwrap();
        assert!(
            early.is_err(),
            "a second spawn ran while the first prepared"
        );
        let later = done.recv_timeout(Duration::from_secs(20));
        assert!(matches!(later, Ok(Ok(()))), "the second spawn never ran");
        drop(a);
    }

    /// Defect: a spawn on an idle home copies an older `Codex Auth` login over
    /// a newer refresh of the same account in the home, which the last child
    /// wrote and never synced; that newer one must win, in the login too.
    #[test]
    fn an_idle_spawn_keeps_a_newer_codex_auth_refresh_of_the_same_account() {
        let login = Login::new(true);
        let auth = |at: &str| {
            format!(
                r#"{{"tokens":{{"account_id":"one"}},"last_refresh":"2026-11-04T{at}:00:00Z"}}"#
            )
        };
        write_keyring("Codex Auth", &login.auth_key(false), &auth("11")).unwrap();
        write_keyring("Codex Auth", &login.auth_key(true), &auth("12")).unwrap();
        let a = login.share();
        let found = [false, true]
            .map(|private| read_keyring("Codex Auth", &login.auth_key(private)).unwrap());
        drop(a);
        assert_eq!(found, [Some(auth("12")), Some(auth("12"))]);
    }

    /// A login whose config.toml sets `cli_auth_credentials_store` to `store`
    /// (`None`: no such key), among other keys, with no secret encrypted.
    fn configured(login: &Login, store: Option<&str>) -> (PathBuf, PathBuf) {
        let (source, home) = (login.source.clone(), login.home.clone());
        let store = store
            .map(|s| format!("cli_auth_credentials_store = \"{s}\"\n"))
            .unwrap_or_default();
        let config = format!(
            "model = \"gpt-5.5\"\napproval_policy = \"on-request\"\n{store}\n[projects.\"/tmp/work\"]\ntrust_level = \"trusted\"\n"
        );
        std::fs::write(source.join("config.toml"), config).unwrap();
        (source, home)
    }

    /// Defect (ADE-238): config.toml was parsed as one TOML value, so a login
    /// on the "keyring" or "auto" store with no secret encrypted mirrored
    /// nothing, and its Mono started without the login. On keyutils, never a
    /// real Keychain.
    #[test]
    fn a_keyring_or_auto_login_with_no_secret_is_mirrored() {
        for store in [Some("keyring"), Some("auto"), Some("file"), None] {
            let login = Login::new(false);
            let (source, home) = configured(&login, store);
            let key = |dir: &Path| format!("cli|{}", home_key(dir).unwrap());
            write_keyring("Codex Auth", &key(&source), "login").unwrap();
            let mirrors = prepare_keyring(&source, &home).unwrap().len();
            let private = read_keyring("Codex Auth", &key(&home)).unwrap();
            for dir in [&source, &home] {
                let _ = keyring::Entry::new("Codex Auth", &key(dir))
                    .and_then(|e| e.delete_credential());
            }
            let expected = match store {
                Some("keyring" | "auto") => (3, Some("login")),
                _ => (0, None),
            };
            assert_eq!((mirrors, private.as_deref()), expected, "{store:?}");
        }
    }

    /// Defect: a login entry the keyring cannot read fails a Mono on the
    /// "auto" store, which Codex itself reads past to auth.json; or it is
    /// skipped on the "keyring" store or with an encrypted secret, which have
    /// no fallback. A value that is not UTF-8 is a read error on keyutils.
    #[test]
    fn only_an_auto_login_with_no_secret_skips_an_unreadable_entry() {
        for (store, sealed, readable) in [
            ("auto", false, true),
            ("auto", true, false),
            ("keyring", false, false),
        ] {
            let login = Login::new(false);
            let (source, home) = configured(&login, Some(store));
            if sealed {
                std::fs::write(source.join("secrets/blob"), "sealed").unwrap();
            }
            let key = format!("secrets|{}", home_key(&source).unwrap());
            keyring::Entry::new("codex", &key)
                .and_then(|e| e.set_secret(&[0xff, 0xfe]))
                .unwrap();
            let prepared = prepare_keyring(&source, &home).map(|m| m.len());
            let _ = keyring::Entry::new("codex", &key).and_then(|e| e.delete_credential());
            let expected = if readable { Ok(2) } else { Err(()) };
            assert_eq!(prepared.map_err(drop), expected, "{store} sealed={sealed}");
        }
    }

    /// Defect (#522 r1 M1): an "auto" login whose `Codex Auth` entry the
    /// keyring cannot read kept the copy an earlier spawn left in the home.
    /// Codex in the Mono reads that before auth.json, so the Mono ran on an
    /// old login: maybe another account, or a refresh token already used.
    #[test]
    fn an_auto_read_error_clears_the_homes_stale_copy() {
        let login = Login::new(false);
        let (source, home) = configured(&login, Some("auto"));
        write_keyring("Codex Auth", &login.auth_key(true), "stale-login").unwrap();
        keyring::Entry::new("Codex Auth", &login.auth_key(false))
            .and_then(|e| e.set_secret(&[0xff, 0xfe]))
            .unwrap();
        let prepared = prepare_keyring(&source, &home).map(|m| m.len());
        let private = read_keyring("Codex Auth", &login.auth_key(true));
        assert_eq!((prepared, private), (Ok(2), Ok(None)));
    }
}
