//! Alarms H: a held foreground wait reaches its provisioner as a wake,
//! the provisioner answers it with holds.allow / holds.deny, the hook
//! client gets the answer, and no answer in 2 min of active time is a
//! refusal the log records, even when the client is gone. Calls run as
//! `control.rs` runs them: on the store, with the caller's session.

use std::cell::Cell;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::ask_scheduler;
use crate::control::TestHoldHost;
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;
use crate::session_store::SessionStore;
use crate::sleep_hold::client::{self, Cause, Outcome};
use crate::sleep_hold::service::{self, Banner, Refusal};
use crate::sleep_hold::{classify, Harness, HoldRequest, HoldWaitReply};

const P: &str = "/p/sleep-hold";
const SECOND: i64 = 1_000;
const ANSWER: i64 = 120 * SECOND;
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
        now(world),
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
            [format!("hold:{hold}")],
            |row| row.get(0),
        )
        .ok()
}

/// The recipient's window takes the hold's wake: its text, if it may.
fn take(world: &World, hold: &str) -> Option<String> {
    ask_scheduler::claim(
        &world.conn(),
        &format!("hold:{hold}"),
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
    assert!(service::tyler_decides(&conn, &first, false, now(&world)).unwrap());
    assert!(!service::tyler_decides(&conn, &first, true, now(&world)).unwrap());
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
    let (reply, _) = service::call(
        world.store(),
        chain.worker.as_str(),
        "ping",
        &json!({}),
        &|_: &Connection| Ok((0, 0)),
    );
    assert_eq!(reply, json!({"ok": true, "recorded": true}));
    let state: Option<String> = world
        .conn()
        .query_row(
            "SELECT hold_state FROM agent_activity WHERE session_id = ?1",
            [chain.worker.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state.as_deref(), Some("installed"));
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

/// The hook client against the real `hold` namespace on loopback: the
/// worker runs a turn with its app token and no app access. `decide`
/// runs once the request is in, on the World, before the client's answer
/// is read.
fn hook_run(
    world: &mut World,
    worker: &SessionId,
    decide: impl FnOnce(&mut World, &str),
) -> Outcome {
    let server = SessionStore::open_second(world.path().to_path_buf()).unwrap();
    let host = TestHoldHost::running(worker.as_str());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let (active, wall) = (
        AtomicI64::new(world.active_ms()),
        AtomicI64::new(world.now()),
    );
    let clock = |_: &Connection| -> rusqlite::Result<(i64, i64)> {
        Ok((active.load(Ordering::SeqCst), wall.load(Ordering::SeqCst)))
    };
    let (served, actions) = mpsc::channel();
    let token = host.token.clone();
    let payload = json!({
        "session_id": "claude-session", "tool_use_id": "toolu_e2e", "tool_name": "Bash",
        "tool_input": {"command": GH_WATCH, "description": "Wait for CI"},
    })
    .to_string();
    std::thread::scope(|scope| {
        let (host, listener, server, clock) = (&host, &listener, &server, &clock);
        scope.spawn(move || {
            for _ in 0..2 {
                served
                    .send(host.serve_one(listener, server, clock))
                    .unwrap();
            }
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
        assert_eq!(actions.recv().unwrap(), "request");
        let hold: String = world
            .conn()
            .query_row(
                "SELECT id FROM sleep_holds WHERE tool_use_id = 'toolu_e2e'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        decide(world, &hold);
        active.store(world.active_ms(), Ordering::SeqCst);
        wall.store(world.now(), Ordering::SeqCst);
        let outcome = hook.join().unwrap();
        assert_eq!(actions.recv().unwrap(), "wait");
        outcome
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
