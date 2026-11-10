//! `ci.watch` on the real store: a watch is a row, the poll reads each
//! watched PR, and an ended watch wakes its recipient through the outbox
//! that the window delivers (R9), across restarts.

use std::cell::Cell;
use std::collections::HashMap;

use serde_json::{json, Value};

use super::world::World;
use crate::ask_scheduler;
use crate::ci_watch::{self, Snapshot};
use crate::mono_messages::SessionId;

const SHA: &str = "0f4432e0d46670e4afed982a0d2a8d0ca13dbe34";
const NEW_SHA: &str = "2222222222222222222222222222222222222222";
const REPO: &str = "acme/web";
const MINUTE: i64 = 60_000;

/// One job of a `statusCheckRollup`, as `gh pr view --json` gives it.
fn job(name: &str, status: &str, conclusion: &str, run: u32) -> Value {
    json!({"__typename": "CheckRun", "name": name, "workflowName": "CI",
           "status": status, "conclusion": conclusion,
           "detailsUrl": format!("https://github.com/acme/web/actions/runs/{run}/job/1"),
           "startedAt": "2026-11-10T01:00:00Z"})
}

fn running(run: u32) -> Value {
    job("ci", "IN_PROGRESS", "", run)
}

/// A read of the PR in `state` with head `head`.
fn snapshot(state: &str, head: &str, checks: Vec<Value>) -> Result<Snapshot, String> {
    let json = json!({"state": state, "headRefOid": head, "statusCheckRollup": checks});
    Ok(ci_watch::parse_snapshot(&json.to_string()).unwrap())
}

fn finished() -> Result<Snapshot, String> {
    snapshot(
        "OPEN",
        SHA,
        vec![
            job("gate/lint", "COMPLETED", "SUCCESS", 10),
            job("ci", "COMPLETED", "FAILURE", 20),
        ],
    )
}

/// One `ci.*` call from `session`, as the control route runs it.
fn call(world: &World, session: &SessionId, action: &str, input: Value, request_id: &str) -> Value {
    ci_watch::call(
        &world.conn(),
        session.as_str(),
        action,
        &input,
        request_id,
        world.now(),
    )
}

fn register(world: &World, session: &SessionId, request_id: &str) -> String {
    let response = call(
        world,
        session,
        "ci.watch",
        json!({"repo": REPO, "pr": 7, "sha": SHA}),
        request_id,
    );
    assert_eq!(response["ok"], true, "{response}");
    response["result"]["id"].as_str().unwrap().to_string()
}

/// One poll pass at the World's clocks; `read` answers every PR, and the
/// number of reads it made is returned.
fn poll(world: &World, read: &Result<Snapshot, String>) -> usize {
    let reads = Cell::new(0);
    ci_watch::poll(
        world.store(),
        |_: &rusqlite::Connection| Ok((world.active_ms(), world.now())),
        |repo, pr| {
            assert_eq!((repo, pr), (REPO, 7));
            reads.set(reads.get() + 1);
            read.clone()
        },
    )
    .unwrap();
    reads.get()
}

/// The wakes that wait for `session`'s chat, claimed as an idle window
/// claims them: each one's text, and whether it starts a turn.
fn wakes(world: &World, session: &SessionId) -> Vec<(String, bool)> {
    let conn = world.conn();
    ask_scheduler::pending(&conn, session)
        .unwrap()
        .into_iter()
        .map(|key| {
            let claimed = ask_scheduler::claim(&conn, &key, "main", &HashMap::new(), false)
                .unwrap()
                .unwrap();
            (claimed.text, claimed.wake)
        })
        .collect()
}

fn outbox_rows(world: &World) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_outbox WHERE stage = 'ci'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_watch_survives_a_restart_and_still_wakes() {
    let mut world = World::new("ci-watch-restart");
    let lead = world.add_session("ciw-lead", "/p", None);
    let id = register(&world, &lead, "w1");
    world.down(10 * MINUTE);
    let listed = call(&world, &lead, "ci.list", json!({}), "l1");
    assert_eq!(listed["result"]["watches"][0]["id"], id.as_str());
    assert_eq!(poll(&world, &finished()), 1);
    assert!(
        wakes(&world, &lead).is_empty(),
        "the first read only holds the set"
    );
    world.down(MINUTE);
    assert_eq!(poll(&world, &finished()), 1);
    world.down(MINUTE);
    let got = wakes(&world, &lead);
    assert_eq!(got.len(), 1, "{got:?}");
    let (text, wake) = &got[0];
    assert!(*wake, "an idle lead gets a new turn");
    assert!(
        text.starts_with(&format!(
            "<ci_watch id=\"{id}\" repo=\"{REPO}\" pr=\"7\" sha=\"{SHA}\" outcome=\"finished\">\nPR #7 at {SHA}:"
        )),
        "{text}"
    );
    assert!(text.contains("  ci: failure, run 20"), "{text}");
    assert!(text.contains("Failed:\n  CI / ci (run 20)"), "{text}");
}

#[test]
fn one_finished_set_wakes_once() {
    let mut world = World::new("ci-watch-once");
    let lead = world.add_session("ciw-lead", "/p", None);
    register(&world, &lead, "w1");
    let running = snapshot("OPEN", SHA, vec![running(20)]);
    assert_eq!(poll(&world, &running), 1);
    assert_eq!(
        poll(&world, &running),
        0,
        "a PR is read at most once a minute"
    );
    world.jump_wall(MINUTE);
    assert_eq!(poll(&world, &finished()), 1);
    assert_eq!(
        outbox_rows(&world),
        0,
        "a set that just finished waits a read"
    );
    for _ in 0..3 {
        world.jump_wall(MINUTE);
        poll(&world, &finished());
    }
    assert_eq!(outbox_rows(&world), 1);
    assert_eq!(
        poll(&world, &finished()),
        0,
        "an ended watch is not read again"
    );
    assert_eq!(
        call(&world, &lead, "ci.list", json!({}), "l1")["result"]["watches"],
        json!([])
    );
}

#[test]
fn a_moved_head_wakes_superseded() {
    let world = World::new("ci-watch-superseded");
    let lead = world.add_session("ciw-lead", "/p", None);
    register(&world, &lead, "w1");
    poll(&world, &snapshot("OPEN", NEW_SHA, vec![running(30)]));
    let got = wakes(&world, &lead);
    assert_eq!(got.len(), 1);
    assert!(got[0].0.contains("outcome=\"superseded\""), "{}", got[0].0);
    assert!(
        got[0].0.contains(&format!("Superseded by {NEW_SHA}")),
        "{}",
        got[0].0
    );
}

#[test]
fn a_merge_or_close_ends_the_watch() {
    for (state, outcome) in [("MERGED", "merged"), ("CLOSED", "closed")] {
        let world = World::new(&format!("ci-watch-{outcome}"));
        let lead = world.add_session("ciw-lead", "/p", None);
        register(&world, &lead, "w1");
        poll(&world, &snapshot(state, SHA, vec![running(20)]));
        let got = wakes(&world, &lead);
        assert_eq!(got.len(), 1);
        assert!(
            got[0].0.contains(&format!("outcome=\"{outcome}\"")),
            "{}",
            got[0].0
        );
    }
}

#[test]
fn six_hours_gives_up() {
    let mut world = World::new("ci-watch-give-up");
    let lead = world.add_session("ciw-lead", "/p", None);
    register(&world, &lead, "w1");
    let running = snapshot("OPEN", SHA, vec![running(20)]);
    world.jump_wall(6 * 60 * MINUTE - 1);
    poll(&world, &running);
    assert!(wakes(&world, &lead).is_empty());
    world.jump_wall(MINUTE);
    poll(&world, &running);
    let got = wakes(&world, &lead);
    assert_eq!(got.len(), 1);
    assert!(got[0].0.contains("outcome=\"gave_up\""), "{}", got[0].0);
}

#[test]
fn watches_of_one_pr_share_one_read() {
    let world = World::new("ci-watch-share");
    let lead = world.add_session("ciw-lead", "/p", None);
    let worker = world.add_session("ciw-worker", "/p", Some("ciw-lead"));
    register(&world, &lead, "w1");
    register(&world, &worker, "w2");
    assert_eq!(poll(&world, &snapshot("OPEN", NEW_SHA, vec![])), 1);
    assert_eq!(wakes(&world, &lead).len(), 1);
    assert_eq!(wakes(&world, &worker).len(), 1);
}

#[test]
fn notify_is_you_or_an_agent_you_provisioned() {
    let world = World::new("ci-watch-rights");
    let lead = world.add_session("ciw-lead", "/p", None);
    let worker = world.add_session("ciw-worker", "/p", Some("ciw-lead"));
    let input = |notify: &str| json!({"repo": REPO, "pr": 7, "sha": SHA, "notify": notify});
    let up = call(&world, &worker, "ci.watch", input("ciw-lead"), "w1");
    assert_eq!(up["code"], "notify_forbidden", "{up}");
    let down = call(&world, &lead, "ci.watch", input("ciw-worker"), "w2");
    assert_eq!(down["result"]["notify"], "session:ciw-worker", "{down}");
    let mine = call(&world, &lead, "ci.watch", input("ciw-lead"), "w3");
    assert_eq!(mine["result"]["notify"], "session:ciw-lead", "{mine}");
    let stranger = SessionId::parse("ciw-nobody").unwrap();
    let unknown = call(&world, &stranger, "ci.watch", input("ciw-lead"), "w4");
    assert_eq!(unknown["code"], "notify_forbidden", "{unknown}");
}

#[test]
fn a_watch_again_is_the_same_watch() {
    let world = World::new("ci-watch-again");
    let lead = world.add_session("ciw-lead", "/p", None);
    let id = register(&world, &lead, "w1");
    let again = call(
        &world,
        &lead,
        "ci.watch",
        json!({"repo": REPO, "pr": 7, "sha": SHA.to_uppercase()}),
        "w2",
    );
    assert_eq!(again["result"]["id"], id.as_str(), "{again}");
    assert_eq!(again["result"]["duplicate"], true);
    let retry = call(
        &world,
        &lead,
        "ci.watch",
        json!({"repo": REPO, "pr": 8, "sha": SHA}),
        "w1",
    );
    assert_eq!(
        retry["result"]["id"],
        id.as_str(),
        "a reused request id is that call"
    );
}

#[test]
fn bad_input_is_refused() {
    let world = World::new("ci-watch-input");
    let lead = world.add_session("ciw-lead", "/p", None);
    for (input, needle) in [
        (json!({"repo": "web", "pr": 7, "sha": SHA}), "owner/name"),
        (json!({"repo": REPO, "pr": 0, "sha": SHA}), "positive"),
        (
            json!({"repo": REPO, "pr": 7, "sha": "0f4432e0"}),
            "40-character",
        ),
        (
            json!({"repo": REPO, "pr": 7, "sha": SHA, "extra": 1}),
            "unknown field",
        ),
        (
            json!({"repo": REPO, "pr": 7, "sha": SHA, "required": [""]}),
            "up to 20",
        ),
    ] {
        let refused = call(&world, &lead, "ci.watch", input, "w1");
        assert_eq!(refused["code"], "invalid_input", "{refused}");
        assert!(
            refused["hint"].as_str().unwrap().contains(needle),
            "{refused}"
        );
    }
    let named = call(
        &world,
        &lead,
        "ci.watch",
        json!({"repo": REPO, "pr": 7, "sha": SHA, "required": ["build"]}),
        "w2",
    );
    assert_eq!(named["ok"], true, "{named}");
    let other = call(&world, &lead, "ci.stop", json!({}), "w3");
    assert_eq!(other["code"], "invalid_input");
    let listed = call(&world, &lead, "ci.list", json!({"x": 1}), "w4");
    assert_eq!(listed["code"], "invalid_input");
}

#[test]
fn a_named_required_list_replaces_the_default() {
    let mut world = World::new("ci-watch-required");
    let lead = world.add_session("ciw-lead", "/p", None);
    let named = call(
        &world,
        &lead,
        "ci.watch",
        json!({"repo": REPO, "pr": 7, "sha": SHA, "required": ["ci"]}),
        "w1",
    );
    assert_eq!(named["ok"], true, "{named}");
    poll(&world, &finished());
    world.jump_wall(MINUTE);
    poll(&world, &finished());
    let text = &wakes(&world, &lead)[0].0;
    assert!(
        text.contains("Required:\n  ci: failure, run 20\nFailed:"),
        "{text}"
    );
}

#[test]
fn cancel_ends_a_watch_without_a_wake() {
    let mut world = World::new("ci-watch-cancel");
    let lead = world.add_session("ciw-lead", "/p", None);
    let other = world.add_session("ciw-other", "/p", None);
    let id = register(&world, &lead, "w1");
    let theirs = call(&world, &other, "ci.cancel", json!({"id": id}), "c1");
    assert_eq!(theirs["code"], "not_your_watch", "{theirs}");
    let unknown = call(&world, &lead, "ci.cancel", json!({"id": "cw_none"}), "c2");
    assert_eq!(unknown["code"], "unknown_watch");
    let done = call(&world, &lead, "ci.cancel", json!({"id": id}), "c3");
    assert_eq!(done["result"]["cancelled"], true, "{done}");
    let again = call(&world, &lead, "ci.cancel", json!({"id": id}), "c4");
    assert_eq!(again["code"], "watch_ended", "{again}");
    assert!(again["hint"].as_str().unwrap().contains("cancelled"));
    world.jump_wall(MINUTE);
    assert_eq!(poll(&world, &finished()), 0);
    assert_eq!(outbox_rows(&world), 0);
}

#[test]
fn the_woken_one_may_cancel_and_list() {
    let world = World::new("ci-watch-woken");
    let lead = world.add_session("ciw-lead", "/p", None);
    let worker = world.add_session("ciw-worker", "/p", Some("ciw-lead"));
    let made = call(
        &world,
        &lead,
        "ci.watch",
        json!({"repo": REPO, "pr": 7, "sha": SHA, "notify": "ciw-worker"}),
        "w1",
    );
    let id = made["result"]["id"].as_str().unwrap();
    let listed = call(&world, &worker, "ci.list", json!({}), "l1");
    assert_eq!(
        listed["result"]["watches"][0]["registeredBy"],
        "session:ciw-lead"
    );
    let done = call(&world, &worker, "ci.cancel", json!({"id": id}), "c1");
    assert_eq!(done["ok"], true, "{done}");
}

#[test]
fn an_outbox_from_the_alarms_build_takes_ci_rows() {
    let dir = std::env::temp_dir().join(format!(
        "monocode-ci-outbox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("monocode.db");
    drop(crate::session_store::SessionStore::open_at(path.clone(), 0).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE mono_outbox;
             CREATE TABLE mono_outbox (
               key          TEXT PRIMARY KEY,
               ask_id       TEXT REFERENCES mono_asks(id),
               stage        TEXT NOT NULL CHECK (stage IN
                 ('t5', 'deadline', 'why', 'blocked', 'resume', 'alarm', 'hold', 'nudge')),
               recipient    TEXT NOT NULL,
               at_active_ms INTEGER NOT NULL,
               claimed_by   TEXT,
               sent_at      INTEGER
             );
             INSERT INTO mono_outbox (key, stage, recipient, at_active_ms)
               VALUES ('nudge-a-1', 'nudge', 'session:s', 7);",
        )
        .unwrap();
    let store = crate::session_store::SessionStore::open_at(path, 0).unwrap();
    let conn = store.lock_conn().unwrap();
    let kept: i64 = conn
        .query_row(
            "SELECT at_active_ms FROM mono_outbox WHERE key = 'nudge-a-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 7);
    conn.execute(
        "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms)
         VALUES ('ci-x', 'ci', 'session:s', 1)",
        [],
    )
    .unwrap();
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The wall time of watch `id`'s last read, if any.
fn polled(world: &World, id: &str) -> Option<i64> {
    let sql = "SELECT polled_wall_ms FROM ci_watches WHERE id = ?1";
    let conn = world.conn();
    conn.query_row(sql, [id], |row| row.get(0)).unwrap()
}

// Defect: the app never starts the thread that reads the watched PRs, so a
// watch never wakes anyone.
#[test]
fn the_app_thread_reads_each_watched_pr() {
    use std::time::{Duration, Instant};

    let world = World::new("ci-watch-thread");
    let lead = world.add_session("ciw-thread", "/p", None);
    let id = register(&world, &lead, "w1");
    let store = crate::session_store::SessionStore::open(world.path().to_path_buf()).unwrap();
    let app = tauri::test::mock_builder()
        .manage(store)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    ci_watch::init(app.handle());
    let deadline = Instant::now() + Duration::from_secs(10);
    while polled(&world, &id).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(polled(&world, &id).is_some(), "the thread read the PR");
}

/// A window that answers every control event: no ci.* call may reach it.
fn no_window(_: &Value) -> Value {
    json!({"ok": false, "code": "window"})
}

// Defect: the control service sends a ci.* call to the window, or answers
// it for another namespace, instead of the watch store.
#[test]
fn a_lead_registers_and_lists_a_watch_over_the_app_endpoint() {
    use super::s5_scheduler::P;
    use super::wake_turn::{app_call, app_with_window, provider_env};
    use crate::mono_messages::tests::invoke_command;
    use crate::mono_roster::MonoRole;

    let world = World::new("ci-watch-route");
    let mono = world.add_mono_in("ciw-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "ciw-route-lead", P);
    let (app, main, events) = app_with_window(&world);
    let cwd = world.path().parent().unwrap().to_str().unwrap().to_string();
    let turn = json!({"sessionId": lead.as_str(), "cwd": cwd, "appAccess": true, "turnId": "t1"});
    invoke_command(&main, "control_authorize_turn", &turn).unwrap();
    let token = provider_env(app.handle(), lead.as_str());
    let input = json!({"repo": REPO, "pr": 7, "sha": SHA});
    let (watched, seen) = app_call(&token, "ci.watch", input, "r1", &main, &events, no_window);
    assert!(seen.is_empty(), "the window got {seen:?}");
    let id = watched["result"]["id"].as_str().unwrap_or_default();
    assert!(id.starts_with("cw_"), "{watched}");
    let none = json!({});
    let (listed, seen) = app_call(&token, "ci.list", none, "r2", &main, &events, no_window);
    assert!(seen.is_empty(), "the window got {seen:?}");
    assert_eq!(listed["result"]["watches"][0]["id"], id, "{listed}");
}
