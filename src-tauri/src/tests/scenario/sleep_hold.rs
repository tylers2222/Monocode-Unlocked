//! Alarms H: a held foreground wait reaches its provisioner as a wake,
//! the provisioner answers it with holds.allow / holds.deny, the hook
//! client gets the answer, and no answer in 2 min of active time is a
//! refusal the log records, even when the client is gone. Calls run as
//! `control.rs` runs them: on the store, with the caller's session.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::active_clock::Active;
use crate::alarms::act::Refusal;
use crate::alarms::Live;
use crate::ask_scheduler;
use crate::control::TestHoldHost;
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;
use crate::session_store::SessionStore;
use crate::sleep_hold::client::{self, Cause, Outcome};
use crate::sleep_hold::install::{HoldState, PING_GRACE_MS};
use crate::sleep_hold::service::{self, Banner, HoldCaller, HoldTime, Received};
use crate::sleep_hold::store::ANSWER_MS as ANSWER;
use crate::sleep_hold::{classify, Harness, HoldRequest, HoldWaitReply};

const P: &str = "/p/sleep-hold";
const SECOND: i64 = 1_000;
const GH_WATCH: &str = "gh pr checks 912 --watch";

/// A Giga, its Mono, the Mono's lead and the lead's worker.
struct Chain {
    mono: Mono,
    lead: SessionId,
    worker: SessionId,
}

fn chain(world: &World, name: &str) -> Chain {
    world.add_mono_in(&format!("{name}-giga"), MonoRole::Giga, &[P]);
    let mono = world.add_mono_in(&format!("{name}-mono"), MonoRole::Mono, &[P]);
    let start = |caller, id: &str| {
        let input = json!({
            "prompt": "Do this", "harness": "codex", "model": "codex:default", "due_minutes": 480,
        });
        world
            .start(caller, id, P, &format!("start-{id}"), &input)
            .unwrap();
        SessionId::parse(id).unwrap()
    };
    let mono_start = mono_caller(&mono, "start");
    let lead = start(&mono_start, &format!("{name}-lead"));
    let lead_start = session_caller(&lead, P);
    let worker = start(&lead_start, &format!("{name}-worker"));
    Chain { mono, lead, worker }
}

/// What the hook client sends for `command` (`sleep_hold::client`).
fn wire(tool_use_id: &str, command: &str) -> Value {
    serde_json::to_value(HoldRequest {
        harness: Harness::Claude,
        session_hint: Some("claude-session".into()),
        tool_use_id: tool_use_id.into(),
        command: command.into(),
        background: false,
        reason: Some("Wait for CI".into()),
        wait: classify(command, false).unwrap(),
    })
    .unwrap()
}

fn now(world: &World) -> (i64, i64) {
    (world.active_ms(), world.now())
}

/// Claude's PreToolUse payload for a foreground `gh pr checks --watch`.
fn hook_payload(tool_use_id: &str) -> String {
    json!({
        "session_id": "claude-session", "tool_use_id": tool_use_id, "tool_name": "Bash",
        "tool_input": {"command": GH_WATCH, "description": "Wait for CI"},
    })
    .to_string()
}

fn hold_of(world: &World, tool_use_id: &str) -> String {
    world
        .conn()
        .query_row(
            "SELECT id FROM sleep_holds WHERE tool_use_id = ?1",
            [tool_use_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn request(world: &World, session: &SessionId, tool_use_id: &str, command: &str) -> String {
    let (hold, _) = service::request(
        &world.conn(),
        session.as_str(),
        &wire(tool_use_id, command),
        now(world),
    )
    .unwrap();
    hold
}

fn status(world: &World, session: &SessionId, hold: &str) -> HoldWaitReply {
    service::status(&world.conn(), session.as_str(), hold, now(world)).unwrap()
}

fn decide(
    world: &World,
    caller: &SessionId,
    action: &str,
    input: Value,
    request_id: &str,
) -> Result<Value, Refusal> {
    service::decide(
        &world.conn(),
        caller.as_str(),
        action,
        &input,
        request_id,
        now(world).into(),
    )
}

fn code(decided: Result<Value, Refusal>) -> &'static str {
    decided.map_or_else(|refusal| refusal.code(), |_| "ok")
}

/// `(decision, decided_by, decision_reason)` of the hold's row.
fn logged(world: &World, hold: &str) -> (Option<String>, Option<String>, Option<String>) {
    world
        .conn()
        .query_row(
            "SELECT decision, decided_by, decision_reason FROM sleep_holds WHERE id = ?1",
            [hold],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn outbox(world: &World, hold: &str) -> Option<String> {
    world
        .conn()
        .query_row(
            "SELECT recipient FROM mono_outbox WHERE key = ?1",
            [format!("hold-{hold}")],
            |row| row.get(0),
        )
        .ok()
}

/// The recipient's window takes the hold's wake: its text, if it may.
fn take(world: &World, hold: &str) -> Option<String> {
    ask_scheduler::claim(
        &world.conn(),
        &format!("hold-{hold}"),
        "main",
        &HashMap::new(),
        true,
    )
    .unwrap()
    .map(|claimed| claimed.text)
}

fn set(world: &World, key: &str, value: &str) {
    world
        .conn()
        .execute(
            "INSERT INTO alarm_settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .unwrap();
}

/// One pass of Tyler's banners: what it showed and what it took down.
fn banners(world: &World) -> (Vec<Banner>, Vec<String>) {
    let (mut shown, mut withdrawn) = (Vec::new(), Vec::new());
    service::notify_tyler(
        world.store(),
        world.now(),
        |banner| {
            shown.push(banner.clone());
            Ok(())
        },
        |hold| withdrawn.push(hold.to_string()),
    )
    .unwrap();
    (shown, withdrawn)
}

// Defect: a held wait never reaches the lead, reaches it with no way to
// answer, or the allow does not reach the client.
#[test]
fn a_held_wait_wakes_its_lead_and_runs_once_the_lead_allows() {
    let world = World::new("hold-allow");
    let chain = chain(&world, "ha");
    let hold = request(&world, &chain.worker, "toolu_1", GH_WATCH);
    assert_eq!(
        outbox(&world, &hold),
        Some(format!("session:{}", chain.lead.as_str()))
    );
    let text = take(&world, &hold).expect("the lead's window takes the wake");
    assert!(
        text.starts_with(&format!(
            "<mono_sleep_hold hold=\"{hold}\" expires_in=\"2m\">\nha-worker (session:ha-worker) wants to wait"
        )),
        "{text}"
    );
    assert!(text.contains("Command: gh pr checks 912 --watch\nHow long: until checks finish (no bound)\nReason: \"Wait for CI\""), "{text}");
    assert!(text.contains(&format!("holds.allow {hold} | holds.deny {hold} --reason")));
    assert_eq!(status(&world, &chain.worker, &hold), HoldWaitReply::Pending);

    let allowed = decide(
        &world,
        &chain.lead,
        "holds.allow",
        json!({"hold": hold}),
        "r1",
    );
    assert_eq!(allowed.unwrap(), json!({"hold": hold, "decision": "allow"}));
    assert_eq!(status(&world, &chain.worker, &hold), HoldWaitReply::Allow);
    let lead = format!("session:{}", chain.lead.as_str());
    assert_eq!(
        logged(&world, &hold),
        (Some("allow".into()), Some(lead), None)
    );
}

// Defect: a deny loses its reason, or its wake stays in the outbox and
// wakes the provisioner for a hold that is over.
#[test]
fn a_deny_reaches_the_client_with_its_reason_and_takes_back_the_wake() {
    let world = World::new("hold-deny");
    let chain = chain(&world, "hd");
    let hold = request(&world, &chain.worker, "toolu_1", "sleep 600");
    let denied = decide(
        &world,
        &chain.mono.chat,
        "holds.deny",
        json!({"hold": hold, "reason": "Run it in the background"}),
        "r1",
    );
    assert_eq!(denied.unwrap()["decision"], "deny");
    assert_eq!(
        status(&world, &chain.worker, &hold),
        HoldWaitReply::Deny {
            reason: Some("Run it in the background".into())
        }
    );
    let mono = format!("mono:{}", chain.mono.id.as_str());
    assert_eq!(
        logged(&world, &hold),
        (
            Some("deny".into()),
            Some(mono),
            Some("Run it in the background".into())
        )
    );
    assert_eq!(outbox(&world, &hold), None);
    assert_eq!(take(&world, &hold), None);
}

// Defect: a client still polling sees an answer later than 2 min, or a
// late answer replaces the timeout the client was refused on.
#[test]
fn no_answer_in_two_minutes_is_a_timeout_and_a_late_answer_is_refused() {
    let mut world = World::new("hold-timeout");
    let chain = chain(&world, "ht");
    let hold = request(&world, &chain.worker, "toolu_1", GH_WATCH);
    world.advance(ANSWER - SECOND);
    assert_eq!(status(&world, &chain.worker, &hold), HoldWaitReply::Pending);
    assert_eq!(logged(&world, &hold).0, None);
    world.advance(SECOND);
    assert_eq!(status(&world, &chain.worker, &hold), HoldWaitReply::Timeout);
    assert_eq!(logged(&world, &hold).0.as_deref(), Some("timeout"));
    let late = decide(
        &world,
        &chain.lead,
        "holds.allow",
        json!({"hold": hold}),
        "r1",
    );
    assert_eq!(code(late), "hold_decided");
    assert_eq!(logged(&world, &hold).0.as_deref(), Some("timeout"));
    assert_eq!(outbox(&world, &hold), None);
}

// Defect: a hold whose client died stays open in the log forever, or the
// app's sleep or downtime counts toward its 2 min.
#[test]
fn the_tick_times_out_a_hold_whose_client_is_gone_in_active_time() {
    let mut world = World::new("hold-gone");
    let chain = chain(&world, "hg");
    let hold = request(&world, &chain.worker, "toolu_1", "sleep 300");
    world.sleep(10 * 60 * SECOND);
    world.advance(ANSWER - SECOND);
    assert_eq!(logged(&world, &hold).0, None, "sleep does not count");
    world.advance(SECOND);
    assert_eq!(
        logged(&world, &hold),
        (
            Some("timeout".into()),
            None,
            Some("no answer in 2 min".into())
        )
    );
    world.advance(ANSWER);
    assert_eq!(logged(&world, &hold).0.as_deref(), Some("timeout"));
}

// Defect: a retried request makes a second hold and a second wake, or two
// sessions that share a tool call id share a hold.
#[test]
fn a_retried_request_returns_the_same_hold() {
    let world = World::new("hold-retry");
    let chain = chain(&world, "hr");
    let first = request(&world, &chain.worker, "toolu_1", "sleep 60");
    assert_eq!(request(&world, &chain.worker, "toolu_1", "sleep 60"), first);
    let other = request(&world, &chain.lead, "toolu_1", "sleep 60");
    assert_ne!(other, first);
    let (holds, wakes): (i64, i64) = world
        .conn()
        .query_row(
            "SELECT (SELECT count(*) FROM sleep_holds),
                    (SELECT count(*) FROM mono_outbox WHERE stage = 'hold')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((holds, wakes), (2, 2));
}

// Defect: the held agent allows itself, a stranger answers, a second
// answer replaces the first, or a retried allow is refused.
#[test]
fn only_the_chain_above_answers_and_the_first_answer_stands() {
    let world = World::new("hold-first");
    let chain = chain(&world, "hf");
    let other = world.add_session("hf-stranger", P, None);
    let hold = request(&world, &chain.worker, "toolu_1", "sleep 60");
    let allow = json!({"hold": hold});
    assert_eq!(
        code(decide(
            &world,
            &chain.worker,
            "holds.allow",
            allow.clone(),
            "r0"
        )),
        "not_your_hold"
    );
    assert_eq!(
        code(decide(&world, &other, "holds.allow", allow.clone(), "r0")),
        "not_your_hold"
    );
    assert_eq!(
        code(decide(
            &world,
            &chain.lead,
            "holds.allow",
            allow.clone(),
            "r1"
        )),
        "ok"
    );
    assert_eq!(
        code(decide(
            &world,
            &chain.lead,
            "holds.allow",
            allow.clone(),
            "r1"
        )),
        "ok",
        "the same call again"
    );
    assert_eq!(
        code(decide(
            &world,
            &chain.lead,
            "holds.allow",
            allow.clone(),
            "r2"
        )),
        "hold_decided"
    );
    let deny = json!({"hold": hold, "reason": "no"});
    assert_eq!(
        code(decide(&world, &chain.mono.chat, "holds.deny", deny, "r3")),
        "hold_decided"
    );
    assert_eq!(logged(&world, &hold).0.as_deref(), Some("allow"));
    assert_eq!(
        code(decide(
            &world,
            &chain.lead,
            "holds.allow",
            json!({"hold": "sh_nothing"}),
            "r4"
        )),
        "unknown_hold"
    );
}

// Defect: with Tyler's holds off (until his card ships), his agents' sleeps
// wait 2 min for an answer no one can give, or a banner goes out.
#[test]
fn tyler_holds_are_skipped_while_his_setting_is_off() {
    let world = World::new("hold-tyler-off");
    let solo = world.add_session("solo", P, None);
    let hold = request(&world, &solo, "toolu_1", "sleep 60");
    assert_eq!(status(&world, &solo, &hold), HoldWaitReply::Allow);
    assert_eq!(
        logged(&world, &hold),
        (
            Some("skipped".into()),
            None,
            Some("Tyler's sleep holds are off".into())
        )
    );
    assert_eq!(outbox(&world, &hold), None);
    assert_eq!(banners(&world), (vec![], vec![]));
}

// Defect: Tyler gets no banner, two banners for one hold, a banner that
// stays up after the hold is decided or expired, or an agent answers for
// him.
#[test]
fn tyler_gets_one_banner_per_hold_and_it_goes_once_decided_or_expired() {
    let mut world = World::new("hold-tyler-on");
    let chain = chain(&world, "hy");
    set(&world, "tyler_sleep_hold", "on");
    let solo = world.add_session("solo", P, None);
    let first = request(&world, &solo, "toolu_1", GH_WATCH);
    let (shown, withdrawn) = banners(&world);
    assert_eq!(
        shown,
        vec![Banner {
            hold: first.clone(),
            subtitle: "solo wants to wait".into(),
            body:
                "gh pr checks 912 --watch · until checks finish · \"Wait for CI\" · 2 min to answer"
                    .into(),
        }]
    );
    assert!(withdrawn.is_empty());
    assert_eq!(banners(&world), (vec![], vec![]), "shown once");
    assert_eq!(outbox(&world, &first), None, "no agent wake");
    assert_eq!(
        code(decide(
            &world,
            &chain.mono.chat,
            "holds.allow",
            json!({"hold": first}),
            "r1"
        )),
        "not_your_hold"
    );

    let conn = world.conn();
    assert!(service::tyler_decides(&conn, &first, false, now(&world).into()).unwrap());
    assert!(!service::tyler_decides(&conn, &first, true, now(&world).into()).unwrap());
    drop(conn);
    assert_eq!(
        status(&world, &solo, &first),
        HoldWaitReply::Deny {
            reason: Some("denied from notification".into())
        }
    );
    assert_eq!(banners(&world), (vec![], vec![first.clone()]));
    assert_eq!(banners(&world), (vec![], vec![]), "withdrawn once");

    let second = request(&world, &solo, "toolu_2", "sleep 60");
    assert_eq!(banners(&world).0.len(), 1);
    world.advance(ANSWER);
    assert_eq!(banners(&world), (vec![], vec![second]));
}

// Defect: the hold's session-start ping is lost, so a session with the hook
// reads as missing it.
#[test]
fn a_ping_marks_the_session_hold_installed() {
    let mut world = World::new("hold-ping");
    let chain = chain(&world, "hp");
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(SECOND);
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );
}

const PASS: i64 = 5 * SECOND;

/// `agent_activity.hold_state` and `hold_missing_reason` of `session`.
fn hold_state_of(world: &World, session: &SessionId) -> (Option<String>, Option<String>) {
    world
        .conn()
        .query_row(
            "SELECT hold_state, hold_missing_reason FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn ping(world: &World, session: &SessionId) -> Value {
    service::call(
        world.store(),
        session.as_str(),
        "ping",
        &json!({}),
        &|_: &Connection| Ok((0, 0).into()),
        &Received {
            at: Instant::now(),
            peer: None,
            may_block: true,
        },
    )
    .0
}

/// A `provider` child of `session` spawned now with sleep hold `hold`, read
/// by the tap `harness_spawn` gives it.
fn spawn(world: &World, session: &SessionId, provider: &str, hold: Option<HoldState>) {
    let alarms = &world.store().alarms;
    let tap = alarms.tap(session.as_str(), Some(provider));
    alarms.spawned(session.as_str(), tap.as_ref(), hold);
}

const SILENT: &str = "the omp hold extension did not report in";

// Defect (H risk 3): a respawn keeps the last child's ping, so a new omp
// whose extension never loaded reads as installed; or a ping that comes
// before the spawn's pass is wiped by the spawn's reset.
#[test]
fn an_omp_respawn_waits_for_its_own_ping() {
    let mut world = World::new("hold-respawn");
    let chain = chain(&world, "hr");
    let awaiting = || Some(HoldState::AwaitingPing);
    spawn(&world, &chain.worker, "omp", awaiting());
    world.alarm_turn_started(&chain.worker, "turn-1");
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(PING_GRACE_MS + PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );

    spawn(&world, &chain.worker, "omp", awaiting());
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "still in its grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: an omp session idle before its first turn reads as missing,
// though the app takes a ping only inside a turn.
#[test]
fn an_omp_session_has_its_grace_from_its_first_turn() {
    let mut world = World::new("hold-first-turn");
    let chain = chain(&world, "hf");
    spawn(&world, &chain.worker, "omp", Some(HoldState::AwaitingPing));
    world.advance(5 * 60 * SECOND);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "idle, never asked"
    );
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "in the turn's grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: a Claude or Codex spawn's install result is not what the row
// shows, so a dropped hook never reaches the provisioner.
#[test]
fn a_spawn_records_how_its_hold_went_in() {
    let mut world = World::new("hold-spawn-state");
    let chain = chain(&world, "hs");
    let missing = HoldState::Missing("hooks disabled".into());
    spawn(&world, &chain.worker, "claude", Some(missing));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some("hooks disabled".into()))
    );
    spawn(&world, &chain.worker, "codex", Some(HoldState::Installed));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );
    spawn(&world, &chain.worker, "cursor", None);
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "no hold for this harness"
    );
}

/// The hook client's clock: no real time passes.
#[derive(Default)]
struct ClientClock(Cell<Duration>);

impl client::Clock for ClientClock {
    fn elapsed(&self) -> Duration {
        self.0.get()
    }

    fn sleep(&self, duration: Duration) {
        self.0.set(self.0.get() + duration);
    }
}

/// The hook client against the app's own control socket on loopback: the
/// worker runs a turn with its app token and no app access. `decide` runs
/// once the request is in, on the World, before the app reads the wait.
fn hook_run(
    world: &mut World,
    worker: &SessionId,
    decide: impl FnOnce(&mut World, &str),
) -> Outcome {
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open_second(world.path().to_path_buf()).unwrap())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let time = Arc::new((
        AtomicI64::new(world.active_ms()),
        AtomicI64::new(world.now()),
    ));
    let clock = time.clone();
    let host = TestHoldHost::running(
        worker.as_str(),
        Arc::new(move |_: &SessionStore, _: &Connection| {
            Ok((
                clock.0.load(Ordering::SeqCst),
                clock.1.load(Ordering::SeqCst),
            )
                .into())
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let (requested, go) = (mpsc::channel(), mpsc::channel());
    let token = host.token.clone();
    let payload = hook_payload("toolu_e2e");
    std::thread::scope(|scope| {
        let (host, listener, handle) = (&host, &listener, app.handle());
        let (requested_tx, go_rx) = (requested.0, go.1);
        scope.spawn(move || {
            host.serve_next(listener, handle);
            requested_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            host.serve_next(listener, handle);
        });
        let hook = scope.spawn(|| {
            let var = |key: &str| match key {
                "MONOCODE_APP_ENDPOINT" => Some(endpoint.clone()),
                "MONOCODE_APP_TOKEN" => Some(token.clone()),
                _ => None,
            };
            client::hold(
                Harness::Claude,
                &payload,
                &var,
                &ClientClock::default(),
                || client::Tcp::from_env(&var),
            )
        });
        requested.1.recv().unwrap();
        let hold = hold_of(world, "toolu_e2e");
        decide(world, &hold);
        time.0.store(world.active_ms(), Ordering::SeqCst);
        time.1.store(world.now(), Ordering::SeqCst);
        go.0.send(()).unwrap();
        hook.join().unwrap()
    })
}

// Defect: the client and the app disagree on the wire (fields, statuses,
// the namespace), so an allowed wait is refused.
#[test]
fn the_hook_client_runs_a_wait_its_lead_allows_over_the_hold_namespace() {
    let mut world = World::new("hold-e2e-allow");
    let chain = chain(&world, "he");
    let lead = chain.lead.clone();
    let outcome = hook_run(&mut world, &chain.worker, |world, hold| {
        let allowed = decide(world, &lead, "holds.allow", json!({"hold": hold}), "r1");
        assert_eq!(code(allowed), "ok");
    });
    assert_eq!(outcome, Outcome::Run);
}

// Defect: the client is never told of the timeout, or hears it as an
// allow, so an unanswered wait runs.
#[test]
fn the_hook_client_refuses_a_wait_no_one_answers_in_two_minutes() {
    let mut world = World::new("hold-e2e-timeout");
    let chain = chain(&world, "hz");
    let outcome = hook_run(&mut world, &chain.worker, |world, _| world.advance(ANSWER));
    assert_eq!(
        outcome,
        Outcome::Refused {
            command: Some(GH_WATCH.into()),
            cause: Cause::NoAnswer,
        }
    );
    let decision: String = world
        .conn()
        .query_row("SELECT decision FROM sleep_holds", [], |row| row.get(0))
        .unwrap();
    assert_eq!(decision, "timeout");
}

// Defect: an alarms outbox key holds a character the front end's request
// id rule drops (`hold:<id>`, `alarm:<ep>:<level>`), so a window loses
// the wake on a reload.
#[test]
fn every_alarms_wake_key_is_one_the_front_end_keeps() {
    let world = World::new("hold-key");
    let chain = chain(&world, "hk");
    let hold = request(&world, &chain.worker, "toolu_1", GH_WATCH);
    let key: String = world
        .conn()
        .query_row(
            "SELECT key FROM mono_outbox WHERE stage = 'hold'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(key, format!("hold-{hold}"));
    assert!(take(&world, &hold).is_some(), "the window still takes it");
    let episode = "al_0123456789ab";
    let kept = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    for key in [
        key,
        crate::alarms::test_taps::outbox_key(episode, 2),
        crate::alarms::deliver::nudge_key(episode, 3),
    ] {
        assert!(!key.is_empty() && key.chars().all(kept), "{key}");
    }
}

/// What a test does at a moment of a `SharedTime`, with the app's time then.
type Due<'a> = Option<(Duration, Box<dyn FnOnce(HoldTime) + 'a>)>;

/// One fake time for the hook client and the app, so a test runs both
/// sides of a hold on the same clock. `at` runs once, when time gets there.
struct SharedTime<'a> {
    world: &'a World,
    base: (i64, i64),
    /// The app's active clock, ticked as the clock thread ticks it.
    live: Live,
    started: Instant,
    tick: Duration,
    ticked: Cell<Duration>,
    passed: Cell<Duration>,
    /// When the app took the call it serves now.
    accepted: Cell<Duration>,
    /// The client process dies here.
    killed_at: Option<Duration>,
    at: RefCell<Due<'a>>,
}

fn ms(time: Duration) -> i64 {
    i64::try_from(time.as_millis()).unwrap()
}

impl SharedTime<'_> {
    /// The app's time, as `service::now` reads it in the lease holder.
    fn app_now(&self) -> HoldTime {
        let passed = self.passed.get();
        HoldTime {
            active: Active::Running(self.live.now_at(self.started + passed).unwrap()),
            wall: self.base.1 + ms(passed),
        }
    }

    fn killed(&self) -> bool {
        self.killed_at.is_some_and(|at| self.passed.get() >= at)
    }

    /// The clock thread's tick: it catches up, then sweeps the holds.
    fn run_tick(&self, at: Duration) {
        self.passed.set(at);
        self.ticked.set(at);
        let active = self.base.0 + ms(at);
        self.live.set_now_at(active, self.started + at);
        service::sweep(&self.world.conn(), (active, self.base.1 + ms(at))).unwrap();
    }

    /// Moves time on by `by`, ticking on the way, or only up to `at` when
    /// it falls inside, and runs it there.
    fn pass(&self, by: Duration) {
        let to = self.passed.get() + by;
        loop {
            let tick = self.ticked.get() + self.tick;
            let answer = self.at.borrow().as_ref().map(|(at, _)| *at);
            if answer.is_some_and(|at| at <= to && at < tick) {
                let (at, run) = self.at.borrow_mut().take().unwrap();
                self.passed.set(at.max(self.passed.get()));
                run(self.app_now());
                return;
            }
            if tick > to {
                break;
            }
            self.run_tick(tick);
        }
        self.passed.set(to);
    }
}

impl client::Clock for SharedTime<'_> {
    fn elapsed(&self) -> Duration {
        self.passed.get()
    }

    fn sleep(&self, duration: Duration) {
        let to = self.passed.get() + duration;
        self.pass(duration);
        self.passed.set(to.max(self.passed.get()));
    }
}

impl HoldCaller for SharedTime<'_> {
    fn elapsed(&self) -> Duration {
        self.passed.get() - self.accepted.get()
    }

    /// A decision wakes a blocked wait at once, as the app's signal does.
    fn block(&self, _seen: u64, timeout: Duration) {
        self.pass(timeout);
    }

    fn hung_up(&self) -> bool {
        self.killed()
    }

    fn may_block(&self) -> bool {
        true
    }
}

/// Each way of the line between the hook client and the app.
const ONE_WAY: Duration = Duration::from_millis(10);
/// What the client does before it sends its request.
const BEFORE_REQUEST: Duration = Duration::from_millis(300);

/// How a run of the hook client goes, apart from the lead's answer.
#[derive(Clone, Copy)]
struct Plan {
    /// Call `n` (0 is the request) waits this long for a free reader.
    queued: Option<(usize, Duration)>,
    /// The client crashes once the reply to call `n` is in.
    panics_after: Option<usize>,
    killed_at: Option<Duration>,
    /// How often the app's clock ticks: 1 s, or slower while stalled.
    tick: Duration,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            queued: None,
            panics_after: None,
            killed_at: None,
            tick: Duration::from_secs(1),
        }
    }
}

/// The hook client's calls run straight on the app's `hold` namespace on
/// one fake time, as `serve` runs them.
struct Direct<'a, 'at> {
    store: &'a SessionStore,
    session: &'a str,
    time: &'a SharedTime<'at>,
    plan: Plan,
    calls: usize,
}

impl client::Transport for Direct<'_, '_> {
    fn call(
        &mut self,
        action: &str,
        input: Value,
        _request_id: &str,
        read_timeout: Duration,
    ) -> Result<Value, String> {
        let (n, time) = (self.calls, self.time);
        self.calls += 1;
        if n == 0 {
            time.pass(BEFORE_REQUEST);
        }
        let sent = time.passed.get();
        time.pass(ONE_WAY);
        time.accepted.set(time.passed.get());
        if let Some((_, queued)) = self.plan.queued.filter(|(call, _)| *call == n) {
            time.pass(queued);
        }
        let now = |_: &Connection| -> rusqlite::Result<HoldTime> { Ok(time.app_now()) };
        let (reply, _) = service::call(self.store, self.session, action, &input, &now, time);
        time.pass(ONE_WAY);
        if time.killed() {
            return Err("the client was killed".into());
        }
        if self.plan.panics_after == Some(n) {
            panic!("the hook client crashed");
        }
        if time.passed.get() - sent > read_timeout {
            return Err(format!("no {action} reply in {read_timeout:?}"));
        }
        Ok(reply)
    }
}

struct Answered {
    /// `None` when the client crashed.
    outcome: Option<Outcome>,
    /// The code the lead's `holds.allow` got.
    answer: &'static str,
    hold: String,
}

/// The worker's hook client asks to wait as `plan` says; the lead allows
/// `answer_at` after the client started, while it waits or after it
/// stopped. The app's clock ticks as `plan` says, and once more at the end.
fn allowed_at(
    world: &World,
    chain: &Chain,
    tool_use_id: &str,
    plan: Plan,
    answer_at: Duration,
) -> Answered {
    let answer = Cell::new("not answered");
    let base = now(world);
    let (live, started) = (Live::default(), Instant::now());
    live.set_now_at(base.0, started);
    let time = SharedTime {
        world,
        base,
        live,
        started,
        tick: plan.tick,
        ticked: Cell::new(Duration::ZERO),
        passed: Cell::new(Duration::ZERO),
        accepted: Cell::new(Duration::ZERO),
        killed_at: plan.killed_at,
        at: RefCell::new(Some((
            answer_at,
            Box::new(|at| {
                let input = json!({"hold": hold_of(world, tool_use_id)});
                answer.set(code(service::decide(
                    &world.conn(),
                    chain.lead.as_str(),
                    "holds.allow",
                    &input,
                    "r1",
                    at,
                )));
            }),
        ))),
    };
    let payload = hook_payload(tool_use_id);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client::hold(Harness::Claude, &payload, &|_| None, &time, || {
            Ok(Direct {
                store: world.store(),
                session: chain.worker.as_str(),
                time: &time,
                plan,
                calls: 0,
            })
        })
    }))
    .ok();
    time.pass(answer_at.saturating_sub(time.passed.get()));
    service::sweep(&world.conn(), time.app_now().closing()).unwrap();
    Answered {
        outcome,
        answer: answer.get(),
        hold: hold_of(world, tool_use_id),
    }
}

/// A decision the log has is one the client heard; else the hold timed out
/// and the late answer was refused. `true` when the client heard it.
fn heard(world: &World, run: &Answered, case: &str) -> bool {
    match logged(world, &run.hold).0.as_deref() {
        Some("allow") => {
            assert_eq!(run.outcome, Some(Outcome::Run), "{case}");
            assert_eq!(run.answer, "ok", "{case}");
            true
        }
        Some("timeout") => {
            assert_ne!(run.outcome, Some(Outcome::Run), "{case}");
            assert_eq!(run.answer, "hold_decided", "{case}");
            false
        }
        other => panic!("{case}: the hold reads {other:?}"),
    }
}

// Defect: the app takes an answer after the client stopped waiting, so the
// log says allow while the agent was refused; or a request that waited for
// a free reader moves the app's cut past the client's. Time runs as in the
// app: round trips, 300 ms before the request, a clock that moves by ticks.
#[test]
fn the_client_hears_every_answer_the_app_logs_up_to_the_cut() {
    let world = World::new("hold-cut");
    let chain = chain(&world, "hc");
    for queued in [Duration::ZERO, Duration::from_secs(20)] {
        let (mut yes, mut no) = (0, 0);
        for quarter in 448..=468 {
            let answer_at = Duration::from_millis(quarter * 250);
            let case = format!("queued {queued:?}, allowed at {answer_at:?}");
            let plan = Plan {
                queued: Some((0, queued)),
                ..Plan::default()
            };
            let id = format!("toolu_{}_{quarter}", queued.as_secs());
            if heard(
                &world,
                &allowed_at(&world, &chain, &id, plan, answer_at),
                &case,
            ) {
                yes += 1;
            } else {
                no += 1;
            }
        }
        assert!(
            yes > 0 && no > 0,
            "queued {queued:?}: {yes} heard, {no} refused"
        );
    }
}

// Defect: the hook client stops (its last wait ends, a reply comes too
// late, it crashes or is killed) and the app keeps the hold open, so a
// later allow is logged that no client heard; or with the app's tick
// stalled the hold stays open for as long as the stall.
#[test]
fn every_way_the_client_stops_closes_the_hold_before_a_later_answer() {
    let world = World::new("hold-stops");
    let chain = chain(&world, "hs");
    let second = Duration::from_secs(1);
    let idle = Duration::from_millis(u64::try_from(crate::sleep_hold::store::IDLE_MS).unwrap());
    let stops = |tick| {
        [
            (
                "last wait",
                Plan::default(),
                crate::sleep_hold::ANSWER_CUT + second,
            ),
            (
                "read timeout",
                Plan {
                    queued: Some((2, 40 * second)),
                    ..Plan::default()
                },
                50 * second,
            ),
            (
                "crash",
                Plan {
                    panics_after: Some(2),
                    ..Plan::default()
                },
                61 * second + idle,
            ),
            (
                "killed",
                Plan {
                    killed_at: Some(45 * second),
                    ..Plan::default()
                },
                47 * second,
            ),
        ]
        .map(|(stop, plan, after)| (stop, Plan { tick, ..plan }, after))
    };
    // On Windows the hold reading stays at a stalled tick (live.rs, #547).
    let ticks: &[Duration] = if cfg!(windows) {
        &[second]
    } else {
        &[second, 5 * second]
    };
    for &tick in ticks {
        for (n, (stop, plan, after)) in stops(tick).into_iter().enumerate() {
            let id = |when: &str| format!("toolu_{}_{n}_{when}", tick.as_secs());
            let case = |what: &str| format!("{stop}, ticks every {tick:?}: allowed {what}");
            let before = allowed_at(&world, &chain, &id("b"), plan, 10 * second);
            assert!(heard(&world, &before, &case("while it waits")));
            let late = allowed_at(&world, &chain, &id("a"), plan, after);
            assert!(!heard(&world, &late, &case("after it stopped")));
        }
    }
}

// Defect: the app times a hold by the clock its last tick left, so with
// the tick stalled for seconds (App Nap, a busy store) a live client looks
// idle and its hold times out; or a wait that waited for a free reader
// finds its hold idle although the client sent it on time.
#[test]
#[cfg_attr(
    windows,
    ignore = "the hold reading stays at a stalled tick there (live.rs, #547)"
)]
fn a_live_client_keeps_its_hold_through_a_stalled_tick() {
    let world = World::new("hold-stalled-tick");
    let chain = chain(&world, "ht");
    // Ticks 12 s apart: the 2nd wait starts 6.35 s after the tick at 24 s.
    // Ticks 5 s apart: the 2nd wait queues 4 s between the ticks at 30 s
    // and 35 s.
    let stalls = [(12, None), (5, Some((2, Duration::from_secs(4))))];
    for (n, (tick, queued)) in stalls.into_iter().enumerate() {
        let plan = Plan {
            queued,
            tick: Duration::from_secs(tick),
            ..Plan::default()
        };
        let run = allowed_at(
            &world,
            &chain,
            &format!("toolu_{n}"),
            plan,
            Duration::from_secs(100),
        );
        let case = format!("ticks every {tick} s, queued {queued:?}");
        assert!(heard(&world, &run, &case), "{case}");
    }
}

// Defect: the launch or a tick stamps its reading after its store
// writes, so a write that stalls (another process holds the database)
// holds the hold clock back by the stall until the next tick, and a
// client's idle budget shrinks by as much.
#[test]
#[cfg_attr(
    windows,
    ignore = "the hold reading stays at the tick there (live.rs, #547)"
)]
fn the_hold_clock_runs_on_from_when_the_tick_read_the_clocks() {
    let mut world = World::new("hold-clock-read");
    let runs_on_from_the_read = |world: &World, case: &str| {
        let (read, active) = (world.clock_read_at(), world.active_ms());
        let reading = world.store().alarms.now_at(read + Duration::from_secs(1));
        assert_eq!(reading, Some(active + 1_000), "{case}");
    };
    runs_on_from_the_read(&world, "the launch writes the clock row");
    world.advance(5_000);
    runs_on_from_the_read(&world, "the 5th tick writes the clock row");
}

/// A call the app took `0` ago, with no tick since.
struct Queued(Duration);

impl HoldCaller for Queued {
    fn elapsed(&self) -> Duration {
        self.0
    }

    fn block(&self, _seen: u64, _timeout: Duration) {}

    fn hung_up(&self) -> bool {
        false
    }

    fn may_block(&self) -> bool {
        true
    }
}

// Defect: a wait the app serves after its client's read timeout leaves the
// hold open, as the client sent it on time, so a later answer is logged
// that no client hears.
#[test]
fn a_wait_served_after_its_client_stopped_reading_closes_its_hold() {
    let world = World::new("hold-read-late");
    let chain = chain(&world, "hr");
    let at = now(&world);
    let app_now = |_: &Connection| -> rusqlite::Result<HoldTime> { Ok(at.into()) };
    let wait = |hold: &str, queued: u64| {
        let input = json!({"hold": hold, "timeoutSeconds": 30, "last": false});
        let caller = Queued(Duration::from_secs(queued));
        let (reply, _) = service::call(
            world.store(),
            chain.worker.as_str(),
            "wait",
            &input,
            &app_now,
            &caller,
        );
        reply["status"].clone()
    };
    let read = request(&world, &chain.worker, "toolu_read", "sleep 60");
    assert_eq!(wait(&read, 34), "pending", "the client still reads");
    let gone = request(&world, &chain.worker, "toolu_gone", "sleep 60");
    assert_eq!(wait(&gone, 35), "timeout");
    assert_eq!(
        logged(&world, &gone).2.as_deref(),
        Some("the hold client stopped waiting")
    );
}

/// One banner pass of `store`: how many it showed and took down.
/// `meanwhile` runs while each banner goes up or down.
fn counted_pass(store: &SessionStore, wall: i64, meanwhile: &dyn Fn()) -> (usize, usize) {
    let (shown, withdrawn) = (Cell::new(0), Cell::new(0));
    service::notify_tyler(
        store,
        wall,
        |_| {
            meanwhile();
            shown.set(shown.get() + 1);
            Ok(())
        },
        |_| {
            meanwhile();
            withdrawn.set(withdrawn.get() + 1);
        },
    )
    .unwrap();
    (shown.get(), withdrawn.get())
}

// Defect: two MonoCode processes on one data folder both put up Tyler's
// banner for a hold, or both take it down.
#[test]
fn two_processes_show_a_tyler_hold_once_and_take_it_down_once() {
    let world = World::new("hold-two-banners");
    set(&world, "tyler_sleep_hold", "on");
    let solo = world.add_session("solo", P, None);
    let hold = request(&world, &solo, "toolu_1", "sleep 60");
    let other = SessionStore::open_second(world.path().to_path_buf()).unwrap();
    let wall = world.now();
    let theirs = Cell::new((0, 0));
    let meanwhile = || theirs.set(counted_pass(&other, wall, &|| {}));
    assert_eq!(counted_pass(world.store(), wall, &meanwhile), (1, 0));
    assert_eq!(theirs.get(), (0, 0), "the other process shows nothing");
    assert_eq!(counted_pass(&other, wall, &|| {}), (0, 0));

    assert!(service::tyler_decides(&world.conn(), &hold, true, now(&world).into()).unwrap());
    assert_eq!(counted_pass(world.store(), wall, &meanwhile), (0, 1));
    assert_eq!(theirs.get(), (0, 0), "the other process takes nothing down");
    assert_eq!(counted_pass(&other, wall, &|| {}), (0, 0));
}

// Defect: a process without the clock lease reads the clock its holder
// stored up to 5 s ago, so it takes an answer after the cut.
#[test]
fn a_process_without_the_lease_cuts_a_hold_on_the_live_clock() {
    let world = World::new("hold-no-lease");
    let chain = chain(&world, "hn");
    let hold = request(&world, &chain.worker, "toolu_1", "sleep 60");
    let (requested, wall) = now(&world);
    world
        .conn()
        .execute(
            "UPDATE mono_clock SET active_ms = ?1, last_wall_ms = ?2 WHERE id = 1",
            [requested + ANSWER - 4 * SECOND, wall],
        )
        .unwrap();
    let later = wall + 6 * SECOND;
    let live = crate::active_clock::active_at(None, &world.conn(), later).unwrap();
    assert_eq!(
        live,
        Active::Running(requested + ANSWER + 2 * SECOND),
        "4 s short + 6 s on"
    );
    let late = service::decide(
        &world.conn(),
        chain.lead.as_str(),
        "holds.allow",
        &json!({"hold": hold}),
        "r1",
        (requested + ANSWER + 2 * SECOND, later).into(),
    );
    assert_eq!(code(late), "hold_decided");
    assert_eq!(logged(&world, &hold).0.as_deref(), Some("timeout"));
}

// Defect: the process that ran the active clock quit, so a second process
// reads a clock that never moves on: its holds never time out and an
// answer long after the client gave up is logged as allow.
#[test]
fn with_the_active_clock_stopped_a_hold_fails_closed() {
    let world = World::new("hold-clock-stopped");
    let chain = chain(&world, "hx");
    let other = SessionStore::open_second(world.path().to_path_buf()).unwrap();
    let held = |tool_use_id: &str, written_ago: i64| {
        let hold = request(&world, &chain.worker, tool_use_id, "sleep 60");
        let wall = crate::session_store::now_millis() - written_ago;
        world
            .conn()
            .execute(
                "UPDATE mono_clock SET last_wall_ms = ?1 WHERE id = 1",
                [wall],
            )
            .unwrap();
        hold
    };
    let other_now = |conn: &Connection| service::now(&other, conn);
    let allow_hold = |hold: &str| {
        let input = json!({"hold": hold});
        service::decide_call(
            &other,
            chain.lead.as_str(),
            "holds.allow",
            &input,
            "r1",
            &other_now,
        )
    };
    let fresh = held("toolu_fresh", 4 * SECOND);
    assert_eq!(
        allow_hold(&fresh)["ok"],
        true,
        "a clock written 4 s ago runs"
    );

    let stopped = held("toolu_stopped", 60 * SECOND);
    assert_eq!(allow_hold(&stopped)["code"], "clock_stale");
    assert_eq!(
        logged(&world, &stopped),
        (
            Some("timeout".into()),
            None,
            Some("the active clock stopped".into())
        )
    );
    let waited = held("toolu_waited", 60 * SECOND);
    let caller = Received {
        at: Instant::now(),
        peer: None,
        may_block: true,
    };
    let wait = json!({"hold": waited, "timeoutSeconds": 30, "last": false});
    let (reply, _) = service::call(
        &other,
        chain.worker.as_str(),
        "wait",
        &wait,
        &other_now,
        &caller,
    );
    assert_eq!(reply, json!({"ok": true, "status": "timeout"}));
    let (reply, _) = service::call(
        &other,
        chain.worker.as_str(),
        "request",
        &wire("toolu_new", "sleep 60"),
        &other_now,
        &caller,
    );
    assert_eq!(reply["code"], "clock_stale");

    set(&world, "tyler_sleep_hold", "on");
    let solo = world.add_session("solo", P, None);
    let tyler = request(&world, &solo, "toolu_tyler", "sleep 60");
    let (active, wall) = now(&world);
    let stopped = HoldTime {
        active: Active::Stopped(active),
        wall,
    };
    assert!(!service::tyler_decides(&world.conn(), &tyler, true, stopped).unwrap());
    assert_eq!(logged(&world, &tyler).0.as_deref(), Some("timeout"));
}
