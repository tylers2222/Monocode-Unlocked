use std::cell::RefCell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

    fn share_every(&self, every: Timer) -> Share {
        share_every(&self.source, self.home.clone(), every).unwrap()
    }

    /// Codex's refresh at `at` o'clock, in the login (`private` false) or the home.
    fn refresh(&self, private: bool, at: &str) {
        write_keyring("Codex Auth", &self.auth_key(private), &refresh(at)).unwrap();
    }

    /// The `Codex Auth` entry in the login, then in the home.
    fn read_auth(&self) -> [Option<String>; 2] {
        [false, true].map(|private| read_keyring("Codex Auth", &self.auth_key(private)).unwrap())
    }

    /// The login's `Codex Auth` comes to be the refresh at `at` within 10 s.
    fn refresh_reaches_login(&self, at: &str) -> bool {
        soon(|| self.read_auth()[0] == Some(refresh(at)))
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

/// A timer that never fires within a test.
const NEVER: Timer = Timer {
    every: Duration::from_secs(3600),
    slow: Duration::from_secs(10),
};

/// A timer that fires every 100 ms, and never takes a keyutils read for slow.
const FAST: Timer = Timer {
    every: Duration::from_millis(100),
    slow: Duration::from_secs(10),
};

/// A `Codex Auth` value of account "one", refreshed at `at` o'clock.
fn refresh(at: &str) -> String {
    format!(r#"{{"tokens":{{"account_id":"one"}},"last_refresh":"2026-11-04T{at}:00:00Z"}}"#)
}

/// `check` comes true within 10 s.
fn soon(check: impl Fn() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

thread_local! {
    /// The homes of this thread's logins. A quit syncs only those, so it
    /// never syncs another test's home in the middle of that test.
    static MINE: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn mine(homes: Vec<(PathBuf, Arc<Home>)>) -> Vec<(PathBuf, Arc<Home>)> {
    let mine = MINE.with(|mine| mine.borrow().clone());
    homes
        .into_iter()
        .filter(|(home, _)| mine.contains(home))
        .collect()
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

/// Every key read so far, by any thread, with its count.
static READ_COUNT: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

fn read_counts(keys: &[String]) -> Vec<usize> {
    let counts = READ_COUNT.lock().unwrap_or_else(|e| e.into_inner());
    keys.iter()
        .map(|key| counts.get(key).copied().unwrap_or(0))
        .collect()
}

pub(super) fn count_read(key: &str) {
    *READ_COUNT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.to_owned())
        .or_default() += 1;
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
/// turn too) waits in the home until the child exits, while the login's own
/// Codex spends the revoked refresh token. Or the timer reads before its
/// tick: a busy loop.
#[test]
fn a_live_homes_timer_copies_a_refresh_no_line_follows() {
    let (slow, fast) = (Login::new(true), Login::new(true));
    for login in [&slow, &fast] {
        login.refresh(false, "10");
    }
    let a = slow.share();
    let b = fast.share_every(FAST);
    for login in [&slow, &fast] {
        login.refresh(true, "11");
    }
    let copied = fast.refresh_reaches_login("11");
    let waiting = slow.read_auth()[0].clone();
    drop((a, b));
    assert_eq!((copied, waiting), (true, Some(refresh("10"))));
}

/// Defect (ADE-243): the timer outlives the home's last store, or stops
/// while another store on the home is live.
#[test]
fn a_homes_timer_runs_until_its_last_store_drops() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share_every(FAST);
    let b = login.share_every(FAST);
    drop(a);
    login.refresh(true, "11");
    let joined = login.refresh_reaches_login("11");
    drop(b);
    login.refresh(true, "12");
    std::thread::sleep(FAST.every * 5);
    assert_eq!(
        (joined, login.read_auth()[0].clone()),
        (true, Some(refresh("11")))
    );
}

/// Defect (#529 r1 m4): the last drop does not wake the timer, which then
/// lives until its next tick; or a joining store starts a second timer; or
/// a home with no `Codex Auth` MonoCode wrote starts one.
#[test]
fn a_home_has_one_timer_and_it_ends_with_the_last_drop() {
    let (login, file_store) = (Login::new(true), Login::new(false));
    login.refresh(false, "10");
    let (mirror, other) = (Home::of(&login.home), Home::of(&file_store.home));
    // `HOMES`, the test's own, then one per store and one per timer thread.
    let count = || Arc::strong_count(&mirror);
    let a = login.share();
    let one = count();
    let b = login.share();
    let two = count();
    drop((a, b));
    let ended = soon(|| count() == 2);
    let c = file_store.share();
    let none = Arc::strong_count(&other);
    drop(c);
    assert_eq!((one, two, ended, none), (4, 5, true, 3));
}

/// Defect (#529 r1 M1): the timer reads the passphrase entries, which a
/// refresh never rewrites and Codex may have created: a macOS prompt every
/// tick. An idle tick reads the home's `Codex Auth` and nothing else; the
/// passphrase entries are the lines' to sync.
#[test]
fn the_timer_reads_only_the_homes_codex_auth() {
    let login = Login::new(true);
    login.refresh(false, "10");
    login.write(false, GATEWAY, "token-1");
    let a = login.share_every(FAST);
    let keys = [
        login.auth_key(true),
        login.key(true, ""),
        login.key(true, GATEWAY),
        login.auth_key(false),
        login.key(false, ""),
        login.key(false, GATEWAY),
    ];
    let before = read_counts(&keys);
    std::thread::sleep(FAST.every * 10);
    let idle: Vec<usize> = read_counts(&keys)
        .iter()
        .zip(&before)
        .map(|(after, before)| after - before)
        .collect();
    login.write(true, GATEWAY, "token-2");
    login.refresh(true, "11");
    let copied = login.refresh_reaches_login("11");
    let passphrase = login.read(GATEWAY)[0].clone();
    a.sync();
    let synced = login.read(GATEWAY)[0].clone();
    drop(a);
    assert!(idle[0] >= 3, "the timer read the home {} times", idle[0]);
    assert_eq!(idle[1..], [0; 5], "an idle tick read {idle:?}");
    assert_eq!(
        (copied, passphrase.as_deref(), synced.as_deref()),
        (true, Some("token-1"), Some("token-2"))
    );
}

/// Defect (#529 r1 M1): the timer reads a `Codex Auth` entry Codex created
/// (the login had none at prepare, as an "auto" login on auth.json), which
/// may prompt every tick. The lines still copy it.
#[test]
fn the_timer_skips_a_codex_auth_entry_monocode_did_not_write() {
    let login = Login::new(true);
    let a = login.share_every(FAST);
    login.refresh(true, "11");
    std::thread::sleep(FAST.every * 5);
    let ticked = login.read_auth()[0].clone();
    a.sync();
    let synced = login.read_auth()[0].clone();
    drop(a);
    assert_eq!((ticked, synced), (None, Some(refresh("11"))));
}

/// Defect (#529 r1 M1): a timer read that fails (a denied prompt) or is slow
/// (a prompt) runs again every tick. The timer must pause until a line sync
/// succeeds, then resume. A value that is not UTF-8 fails a keyutils read.
#[test]
fn a_failed_or_slow_timer_read_pauses_the_timer_until_a_line_sync() {
    let (failing, slow) = (Login::new(true), Login::new(true));
    for login in [&failing, &slow] {
        login.refresh(false, "10");
    }
    let a = failing.share_every(FAST);
    let b = slow.share_every(Timer {
        slow: Duration::ZERO,
        ..FAST
    });
    keyring::Entry::new("Codex Auth", &failing.auth_key(true))
        .and_then(|e| e.set_secret(&[0xff, 0xfe]))
        .unwrap();
    slow.refresh(true, "11");
    let paused = [&a, &b].map(|share| soon(|| share.mirror.lock().paused));
    failing.refresh(true, "11");
    std::thread::sleep(FAST.every * 5);
    let held = [&failing, &slow].map(|login| login.read_auth()[0].clone());
    a.sync();
    failing.refresh(true, "12");
    let resumed = failing.refresh_reaches_login("12");
    drop((a, b));
    assert_eq!(
        (paused, held, resumed),
        (
            [true, true],
            [Some(refresh("10")), Some(refresh("10"))],
            true
        )
    );
}

/// Defect (ADE-243): a refresh a child writes as the quit kills it stays in
/// the home: its exit sync races the app's own exit.
#[test]
fn a_quit_copies_a_refresh_written_during_the_kill() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share();
    let started = Instant::now();
    kill_synced(|| login.refresh(true, "11"));
    let took = started.elapsed();
    let copied = login.read_auth()[0].clone();
    drop(a);
    assert_eq!(copied, Some(refresh("11")));
    assert!(took < Duration::from_secs(1), "the quit took {took:?}");
}

/// Defect (#529 r1 M2): the quit waits with no limit on a home whose lock a
/// sync holds at a prompt, and so does every home after it.
#[test]
fn a_quit_skips_a_busy_home_within_its_budget() {
    let (busy, free) = (Login::new(true), Login::new(true));
    for login in [&busy, &free] {
        login.refresh(false, "10");
    }
    let (a, b) = (busy.share(), free.share());
    let held = Home::of(&busy.home);
    let (locked, lock) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _state = held.lock();
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(6));
    });
    lock.recv().unwrap();
    for login in [&busy, &free] {
        login.refresh(true, "11");
    }
    let started = Instant::now();
    kill_synced(|| ());
    let took = started.elapsed();
    let copied = [&busy, &free].map(|login| login.read_auth()[0].clone());
    holder.join().unwrap();
    drop((a, b));
    assert!(
        took < QUIT_BUDGET + Duration::from_secs(1),
        "the quit took {took:?}"
    );
    assert_eq!(copied, [Some(refresh("10")), Some(refresh("11"))]);
}

/// Defect (#529 r1 m1): closing the last window reaps, then the quit it
/// starts reaps again, while the homes are still live. The children are dead
/// after the first, so the second reads nothing; a spawn after a reap makes
/// the next one read again.
#[test]
fn a_home_is_reaped_once_until_it_spawns_again() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share();
    kill_synced(|| ());
    login.refresh(true, "11");
    kill_synced(|| ());
    let twice = login.read_auth()[0].clone();
    let b = login.share();
    kill_synced(|| ());
    let respawned = login.read_auth()[0].clone();
    drop((a, b));
    assert_eq!(
        (twice, respawned),
        (Some(refresh("10")), Some(refresh("11")))
    );
}

/// Defect: a quit reads an idle home, whose children are gone and whose exit
/// syncs ran.
#[test]
fn a_quit_skips_an_idle_home() {
    let login = Login::new(true);
    login.refresh(false, "10");
    drop(login.share());
    login.refresh(true, "11");
    kill_synced(|| ());
    assert_eq!(login.read_auth()[0], Some(refresh("10")));
}

/// Defect (ADE-243): the app's quit kills the children with no sync.
#[test]
fn the_app_quit_copies_a_pending_refresh() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share();
    login.refresh(true, "11");
    let app = tauri::test::mock_app();
    crate::reap_harness_children(app.handle());
    let copied = login.read_auth();
    drop(a);
    assert_eq!(copied, [Some(refresh("11")), Some(refresh("11"))]);
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
