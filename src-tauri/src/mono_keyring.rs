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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::codex_mono_store::{
    has_secrets, home_key, newer_same_account, prepare_keyring, read_keyring, write_keyring,
    KeyringMirror, MIRRORED,
};

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

/// The home's mirror entries, each with the login's value now as its baseline.
fn login_mirrors(source: &Path, home: &Path) -> Result<Vec<KeyringMirror>, String> {
    let (source_hash, private_hash) = (home_key(source)?, home_key(home)?);
    MIRRORED
        .into_iter()
        .map(|(service, prefix, suffix)| {
            let source_key = format!("{prefix}|{source_hash}{suffix}");
            Ok(KeyringMirror {
                service,
                source_value: read_keyring(service, &source_key)?,
                source_key,
                private_key: format!("{prefix}|{private_hash}{suffix}"),
            })
        })
        .collect()
}

impl KeyringMirror {
    fn sync(&mut self) -> Result<(), String> {
        let source = read_keyring(self.service, &self.source_key)?;
        let private = read_keyring(self.service, &self.private_key)?;
        if private == source {
            self.source_value = source;
            return Ok(());
        }
        if source != self.source_value && !newer_same_account(&private, &source) {
            return Ok(());
        }
        if let Some(value) = private {
            write_keyring(self.service, &self.source_key, &value)?;
            self.source_value = Some(value);
        }
        Ok(())
    }
}

/// On keyutils, never a real Keychain.
#[cfg(all(test, target_os = "linux"))]
mod tests {
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

        fn write(&self, private: bool, suffix: &str, value: &str) {
            write_keyring("codex", &self.key(private, suffix), value).unwrap();
        }

        /// The entry with `suffix` in the login, then in the home.
        fn read(&self, suffix: &str) -> [Option<String>; 2] {
            [false, true].map(|private| read_keyring("codex", &self.key(private, suffix)).unwrap())
        }
    }

    impl Drop for Login {
        fn drop(&mut self) {
            for private in [false, true] {
                for suffix in ["", GATEWAY] {
                    let _ = keyring::Entry::new("codex", &self.key(private, suffix))
                        .and_then(|entry| entry.delete_credential());
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const GATEWAY: &str = "|gateway-oauth";

    fn both(value: &str) -> [Option<String>; 2] {
        [Some(value.to_owned()), Some(value.to_owned())]
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
}
