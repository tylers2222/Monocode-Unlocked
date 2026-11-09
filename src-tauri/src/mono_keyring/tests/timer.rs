//! The live-home timer and the quit sync (ADE-243, #529).

use super::*;

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
/// a home with no `Codex Auth` MonoCode wrote starts one. Counts this home's
/// timer threads only, and waits on conditions, never on time.
#[test]
fn a_home_has_one_timer_and_it_ends_with_the_last_drop() {
    let (login, file_store) = (Login::new(true), Login::new(false));
    login.refresh(false, "10");
    let a = login.share();
    let b = login.share();
    let waiting = soon(|| parked(&login.home));
    // Free only once the timer is inside its wait, which let the lock go.
    drop(Home::of(&login.home).lock());
    let running = timers(&login.home);
    drop((a, b));
    let ended = soon(|| timers(&login.home) == 0);
    let c = file_store.share();
    let none = timers(&file_store.home);
    drop(c);
    let starts = [started(&login.home), started(&file_store.home)];
    assert_eq!(
        (waiting, running, ended, none, starts),
        (true, 1, true, 0, [1, 0])
    );
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

/// Defect (#529 r1 M1): a timer read that fails (a denied prompt) runs
/// again every tick. The timer must pause until a line sync succeeds, then
/// resume. A value that is not UTF-8 fails a keyutils read.
#[test]
fn a_failed_timer_read_pauses_the_timer_until_a_line_sync() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share_every(FAST);
    login.unreadable();
    let paused = soon(|| a.mirror.lock().paused);
    login.refresh(true, "11");
    std::thread::sleep(FAST.every * 5);
    let held = login.read_auth()[0].clone();
    a.sync();
    login.refresh(true, "12");
    let resumed = login.refresh_reaches_login("12");
    drop(a);
    assert_eq!((paused, held, resumed), (true, Some(refresh("10")), true));
}

/// Defect (#529 r2 m1, m2; r3 m2): a slow timer read (a prompt) drops the
/// refresh it read, or reads the entry again to copy it (a second prompt,
/// under the lock), or the timer reads that entry again after the next line
/// sync: one more prompt per line. It reads it again only after a prepare.
#[test]
fn a_slow_timer_read_copies_its_value_then_stops_until_the_next_prepare() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let slow = Timer {
        every: Duration::from_millis(500),
        slow: Duration::ZERO,
    };
    let key = [login.auth_key(true)];
    let a = login.share_every(slow);
    let before = read_counts(&key)[0];
    login.refresh(true, "11");
    let copied = login.refresh_reaches_login("11");
    std::thread::sleep(slow.every * 3);
    let reads = read_counts(&key)[0] - before;
    login.refresh(true, "12");
    a.sync();
    login.refresh(true, "13");
    std::thread::sleep(slow.every * 3);
    let stopped = login.read_auth()[0].clone();
    a.sync();
    drop(a);
    let b = login.share_every(slow);
    login.refresh(true, "14");
    let again = login.refresh_reaches_login("14");
    drop(b);
    assert_eq!(
        (copied, reads, stopped, again),
        (true, 1, Some(refresh("12")), true)
    );
}

/// Defect (#529 r3 m2): the timer copies the value it read with no second
/// read, but a line sync acted between that read and the copy, so the login
/// already holds a newer refresh, which the older read must not overwrite.
#[test]
fn a_timer_read_older_than_the_last_sync_is_not_copied() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share();
    login.refresh(true, "12");
    a.sync();
    let key = login.auth_key(true);
    a.mirror.lock().take_auth(
        &login.home,
        &key,
        &Some(refresh("10")),
        Ok(Some(refresh("11"))),
        false,
    );
    let kept = login.read_auth()[0].clone();
    drop(a);
    assert_eq!(kept, Some(refresh("12")));
}

/// Defect (#529 r2 m2): a paused timer stays paused into the next live
/// period, though a prepare built the home anew.
#[test]
fn a_prepare_resumes_a_paused_timer() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share_every(FAST);
    login.unreadable();
    let paused = soon(|| a.mirror.lock().paused);
    login.refresh(true, "10");
    drop(a);
    let b = login.share_every(FAST);
    login.refresh(true, "11");
    let resumed = login.refresh_reaches_login("11");
    drop(b);
    assert_eq!((paused, resumed), (true, true));
}

/// Defect (#529 r3 M1): a prepare deletes the home `Codex Auth` and creates
/// it anew, so the new item trusts only MonoCode, and the Mono's Codex
/// prompts at every spawn. It writes in place. Defect (#529 r2 m1): a timer
/// read that finds no entry (Codex deleted it, and will create the next one,
/// which may prompt MonoCode) does not stop the timer reading it.
#[test]
fn a_prepare_writes_codex_auth_in_place_and_a_missing_entry_stops_the_timer() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let key = login.auth_key(true);
    drop(login.share());
    let before = deletes(&key);
    let a = login.share_every(FAST);
    let deleted = deletes(&key) - before;
    login.refresh(true, "11");
    let read = login.refresh_reaches_login("11");
    keyring::Entry::new("Codex Auth", &key)
        .and_then(|e| e.delete_credential())
        .unwrap();
    let gone = soon(|| a.mirror.lock().auth_key().is_none());
    let after = read_counts(std::slice::from_ref(&key))[0];
    login.refresh(true, "12");
    std::thread::sleep(FAST.every * 5);
    let later = read_counts(std::slice::from_ref(&key))[0] - after;
    drop(a);
    assert_eq!((deleted, read, gone, later), (0, true, true, 0));
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

/// Defect (#529 r2 m2): the quit skips a home whose timer paused on a failed
/// read, so a refresh pending there waits for the next launch.
#[test]
fn a_quit_copies_a_refresh_on_a_paused_home() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share_every(FAST);
    login.unreadable();
    let paused = soon(|| a.mirror.lock().paused);
    login.refresh(true, "11");
    kill_synced(|| ());
    let copied = login.read_auth()[0].clone();
    drop(a);
    assert_eq!((paused, copied), (true, Some(refresh("11"))));
}

/// Defect (#529 mutation): the quit gives up on a home that is busy at its
/// first try, though its lock frees well within the budget.
#[test]
fn a_quit_syncs_a_home_that_frees_within_its_budget() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share();
    let held = Home::of(&login.home);
    let (locked, lock) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _state = held.lock();
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
    lock.recv().unwrap();
    login.refresh(true, "11");
    kill_synced(|| ());
    holder.join().unwrap();
    let copied = login.read_auth()[0].clone();
    drop(a);
    assert_eq!(copied, Some(refresh("11")));
}

/// Defect (#529 r1 m2): a sync that keeps failing logs at every try, or
/// never logs the first failure of a streak.
#[test]
fn a_failing_sync_logs_once_a_streak() {
    let login = Login::new(true);
    login.write(false, GATEWAY, "token-1");
    let a = login.share();
    let copy = "Could not copy the Mono Codex login back";
    let unreadable = || {
        keyring::Entry::new("codex", &login.key(false, GATEWAY))
            .and_then(|e| e.set_secret(&[0xff, 0xfe]))
            .unwrap()
    };
    login.write(true, GATEWAY, "token-2");
    unreadable();
    a.sync();
    let first = warnings(&login.home, copy);
    a.sync();
    let streak = warnings(&login.home, copy);
    login.write(false, GATEWAY, "token-1");
    a.sync();
    login.write(true, GATEWAY, "token-3");
    unreadable();
    a.sync();
    let next = warnings(&login.home, copy);
    drop(a);
    assert_eq!((first, streak, next), (1, 1, 2));
}

/// Defect (#529 r2 m3): a failed timer read logs at every later failed read
/// of the same pause, or never logs the first.
#[test]
fn a_failed_timer_read_logs_once_a_pause() {
    let login = Login::new(true);
    login.refresh(false, "10");
    let a = login.share_every(FAST);
    login.unreadable();
    let paused = soon(|| a.mirror.lock().paused);
    let pause = "Paused the Mono Codex login timer";
    let first = warnings(&login.home, pause);
    kill_synced(|| ());
    let again = warnings(&login.home, pause);
    drop(a);
    assert_eq!((paused, first, again), (true, 1, 1));
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
