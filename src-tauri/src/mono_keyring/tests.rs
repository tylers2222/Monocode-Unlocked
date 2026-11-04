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
        let root = std::env::temp_dir().join(format!("ade225-keyring-{}", uuid::Uuid::new_v4()));
        let (source, home) = (root.join("login"), root.join("home"));
        crate::codex_mono_store::prepare_files(&source, &home).unwrap();
        MINE.with(|mine| mine.borrow_mut().push(home.clone()));
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

    /// A share whose timer never fires within a test.
    fn share(&self) -> Share {
        self.share_every(NEVER)
    }

    fn share_every(&self, every: Duration) -> Share {
        share_every(&self.source, self.home.clone(), every).unwrap()
    }

    /// The login's entry with `suffix` comes to be `value` within 10 s.
    fn reaches_login(&self, suffix: &str, value: &str) -> bool {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < until {
            if self.read(suffix)[0].as_deref() == Some(value) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
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

const NEVER: Duration = Duration::from_secs(3600);
const TEST_TICK: Duration = Duration::from_millis(100);

thread_local! {
    /// The homes of this thread's logins. A quit syncs only those, so it
    /// never syncs another test's home in the middle of that test.
    static MINE: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn mine(home: &Path) -> bool {
    MINE.with(|mine| mine.borrow().iter().any(|own| own == home))
}

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

/// Defect (ADE-243): a refresh no line follows (Codex refreshes outside a
/// turn too) waits in the home until the child exits, while the login's
/// own Codex spends the revoked refresh token. Or the timer syncs before
/// its tick, so a busy loop.
#[test]
fn a_live_homes_timer_copies_a_write_no_line_follows() {
    let (slow, fast) = (Login::new(true), Login::new(true));
    for login in [&slow, &fast] {
        login.write(false, GATEWAY, "token-1");
    }
    let a = slow.share();
    let b = fast.share_every(TEST_TICK);
    for login in [&slow, &fast] {
        login.write(true, GATEWAY, "token-2");
    }
    let copied = fast.reaches_login(GATEWAY, "token-2");
    let waiting = slow.read(GATEWAY)[0].clone();
    drop((a, b));
    assert_eq!((copied, waiting.as_deref()), (true, Some("token-1")));
}

/// Defect (ADE-243): the timer outlives the home's last store, or stops
/// while another store on the home is live.
#[test]
fn a_homes_timer_runs_until_its_last_store_drops() {
    let login = Login::new(true);
    login.write(false, GATEWAY, "token-1");
    let a = login.share_every(TEST_TICK);
    let b = login.share_every(TEST_TICK);
    drop(a);
    login.write(true, GATEWAY, "token-2");
    let joined = login.reaches_login(GATEWAY, "token-2");
    drop(b);
    login.write(true, GATEWAY, "token-3");
    std::thread::sleep(TEST_TICK * 5);
    assert_eq!(
        (joined, login.read(GATEWAY)[0].as_deref()),
        (true, Some("token-2"))
    );
}

/// Defect (ADE-243): a quit kills the children before a refresh since the
/// last sync reaches the login, and the exit sync that would copy it races
/// the app's exit. Or a refresh a child writes as it dies stays in the home.
#[test]
fn a_quit_copies_a_refresh_before_and_after_the_kill() {
    let login = Login::new(true);
    login.write(false, GATEWAY, "token-1");
    let a = login.share();
    login.write(true, GATEWAY, "token-2");
    let mut killed = None;
    kill_synced(|| {
        killed = login.read(GATEWAY)[0].clone();
        login.write(true, GATEWAY, "token-3");
    });
    assert_eq!(
        (killed.as_deref(), login.read(GATEWAY)),
        (Some("token-2"), both("token-3"))
    );
    drop(a);
}

/// Defect (ADE-243): the app's quit kills the children with no sync.
#[test]
fn the_app_quit_copies_a_pending_refresh() {
    let login = Login::new(true);
    login.write(false, GATEWAY, "token-1");
    let a = login.share();
    login.write(true, GATEWAY, "token-2");
    let app = tauri::test::mock_app();
    crate::reap_harness_children(app.handle());
    assert_eq!(login.read(GATEWAY), both("token-2"));
    drop(a);
}

/// Defect: a quit reads an idle home, whose exit syncs ran, and for one
/// whose login has since encrypted a secret, the login's entries too: a
/// macOS prompt as the app quits.
#[test]
fn a_quit_reads_only_the_live_homes() {
    let (live, idle) = (Login::new(true), Login::new(false));
    drop(idle.share());
    idle.seal();
    let a = live.share();
    let read = reads(|| kill_synced(|| ()));
    drop(a);
    assert_eq!(read, [live.home_keys(), live.home_keys()].concat());
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
        format!(r#"{{"tokens":{{"account_id":"one"}},"last_refresh":"2026-11-04T{at}:00:00Z"}}"#)
    };
    write_keyring("Codex Auth", &login.auth_key(false), &auth("11")).unwrap();
    write_keyring("Codex Auth", &login.auth_key(true), &auth("12")).unwrap();
    let a = login.share();
    let found =
        [false, true].map(|private| read_keyring("Codex Auth", &login.auth_key(private)).unwrap());
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
            let _ =
                keyring::Entry::new("Codex Auth", &key(dir)).and_then(|e| e.delete_credential());
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
