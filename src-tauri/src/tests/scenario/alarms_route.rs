//! Alarms S2: an episode goes up the chain of provisioners on the active
//! clock's own tick, ends at Tyler's notification, and stops when a
//! recipient acts. Acts run as `control.rs` runs them: `act::call` on the
//! store with the caller's session.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::agents::{session_caller, Mono};
use super::world::World;
use crate::alarms::act::{self, Outcome, Refusal};
use crate::alarms::deliver;
use crate::alarms::taps::Pipe;
use crate::alarms::{test_taps, ChildTap, TylerAlarm};
use crate::ask_scheduler;
use crate::control::alarm_seam::{self, Sent};
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;

mod window;

const P: &str = "/p/alarm-route";
/// The chain's Mono holds a second project, so the window cannot guess its.
const P2: &str = "/p/alarm-route-2";
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const N: i64 = 15 * MINUTE;
const ESCALATE: i64 = 15 * MINUTE;
const PASS: i64 = 5 * SECOND;

/// A Giga, its Mono (two projects), the Mono's lead and the lead's worker,
/// each with the longest deadline on its ask, so no ask reminder runs.
struct Chain {
    giga: Mono,
    mono: Mono,
    lead: SessionId,
    worker: SessionId,
}

fn chain(world: &World, name: &str) -> Chain {
    let giga = world.add_mono_in(&format!("{name}-giga"), MonoRole::Giga, &[P]);
    let mono = world.add_mono_in(&format!("{name}-mono"), MonoRole::Mono, &[P, P2]);
    let start = |caller, id: &str| {
        let input = json!({
            "prompt": "Do this", "harness": "codex", "model": "codex:default", "due_minutes": 480,
        });
        world
            .start(caller, id, P, &format!("start-{id}"), &input)
            .unwrap();
        SessionId::parse(id).unwrap()
    };
    let mono_start = super::agents::mono_caller(&mono, "start");
    let lead = start(&mono_start, &format!("{name}-lead"));
    let lead_start = session_caller(&lead, P);
    let worker = start(&lead_start, &format!("{name}-worker"));
    Chain {
        giga,
        mono,
        lead,
        worker,
    }
}

fn align(world: &mut World) {
    let rest = world.active_ms() % PASS;
    if rest != 0 {
        world.advance(PASS - rest);
    }
}

fn turn(world: &mut World, session: &SessionId, turn_id: &str, started: bool) {
    if started {
        world.alarm_turn_started(session, turn_id);
    } else {
        world.alarm_turn_ended(session, turn_id);
    }
}

fn ran_a_turn(world: &mut World, session: &SessionId) {
    let turn_id = format!("turn-{}", world.active_ms());
    turn(world, session, &turn_id, true);
    world.advance(MINUTE);
    turn(world, session, &turn_id, false);
}

/// The worker ran a turn and sat idle with its ask for `n`: its episode.
fn stalled_worker_after(world: &mut World, chain: &Chain, n: i64) -> String {
    align(world);
    ran_a_turn(world, &chain.worker);
    world.advance(n);
    let open = open_episodes(world);
    assert_eq!(open.len(), 1, "{open:?}");
    open[0].0.clone()
}

fn stalled_worker(world: &mut World, chain: &Chain) -> String {
    stalled_worker_after(world, chain, N)
}

/// Open episodes as `(id, actor, level)`.
fn open_episodes(world: &World) -> Vec<(String, String, i64)> {
    world
        .conn()
        .prepare(
            "SELECT id, actor, level FROM alarm_episodes WHERE closed_active_ms IS NULL
              ORDER BY opened_active_ms",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The episode's sends as `(level, recipient)`.
fn sends(world: &World, episode: &str) -> Vec<(i64, String)> {
    world
        .conn()
        .prepare("SELECT level, recipient FROM alarm_sends WHERE episode_id = ?1 ORDER BY level")
        .unwrap()
        .query_map([episode], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The recipient's window takes outbox row `key` and its provider takes
/// the text; returns the text.
fn take(world: &World, key: &str) -> String {
    let conn = world.conn();
    let claimed = ask_scheduler::claim(&conn, key, "main", &HashMap::new(), true)
        .unwrap()
        .unwrap_or_else(|| panic!("{key} is claimable"));
    assert!(ask_scheduler::sent(&conn, key, "main", world.now()).unwrap());
    claimed.text
}

fn call(world: &World, caller: &str, action: &str, input: Value) -> Result<Outcome, Refusal> {
    act::call(
        &world.conn(),
        caller,
        action,
        &input,
        &format!("req-{}", world.active_ms()),
        (world.active_ms(), world.now()),
    )
}

fn result(outcome: Result<Outcome, Refusal>) -> Value {
    match outcome {
        Ok(Outcome::Done { result, .. }) => result,
        Ok(Outcome::Forward(plan)) => panic!("a window act: {plan:?}"),
        Err(refusal) => panic!("refused: {refusal:?}"),
    }
}

fn code(outcome: Result<Outcome, Refusal>) -> &'static str {
    match outcome {
        Err(refusal) => refusal.code(),
        Ok(_) => "ok",
    }
}

/// The window each agent of `chain` has its session loaded in.
fn windows(chain: &Chain) -> [(&'static str, &str); 4] {
    [
        ("worker-window", chain.worker.as_str()),
        ("lead-window", chain.lead.as_str()),
        ("mono-window", chain.mono.chat.as_str()),
        ("giga-window", chain.giga.chat.as_str()),
    ]
}

/// One `alarms.*` call from `caller`'s window as `serve` runs it; every
/// window it reaches replies `reply`. The response, and what each window
/// was sent.
fn seam(
    world: &World,
    chain: &Chain,
    caller: &str,
    action: &str,
    input: Value,
    request_id: &str,
    reply: &Value,
) -> (Value, Vec<Sent>) {
    let loaded = windows(chain);
    let window = loaded
        .iter()
        .find(|(_, session)| *session == caller)
        .map_or("main", |(window, _)| window);
    alarm_seam::call(
        world.store(),
        &loaded,
        (window, caller),
        action,
        input,
        request_id,
        reply,
    )
}

fn ok() -> Value {
    json!({"ok": true, "result": {"submitted": true}})
}

/// What `sessions.send` with whenBusy steer that `from`'s window was sent
/// says, as `(window, session, action, sessionId, whenBusy, prompt)`.
fn steer_sent(sent: &[Sent]) -> (String, String, String, String, String, String) {
    let [(window, session, action, input)] = sent else {
        panic!("one window event: {sent:?}");
    };
    (
        window.clone(),
        session.clone(),
        action.clone(),
        input["sessionId"].as_str().unwrap_or_default().into(),
        input["whenBusy"].as_str().unwrap_or_default().into(),
        input["prompt"].as_str().unwrap_or_default().into(),
    )
}

fn acts(world: &World, episode: &str) -> Vec<(String, String, String)> {
    world
        .conn()
        .prepare("SELECT actor, how, target FROM alarm_acts WHERE episode_id = ?1 ORDER BY seq")
        .unwrap()
        .query_map([episode], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn close_reason(world: &World, episode: &str) -> Option<String> {
    world
        .conn()
        .query_row(
            "SELECT close_reason FROM alarm_episodes WHERE id = ?1",
            [episode],
            |row| row.get(0),
        )
        .unwrap()
}

fn notify(world: &World) -> Vec<TylerAlarm> {
    let mut shown = Vec::new();
    crate::alarms::notify_tyler(world.store(), (world.active_ms(), world.now()), |alarm| {
        shown.push(alarm.clone());
        Ok(())
    })
    .unwrap();
    shown
}

// Defect: an alarm nobody acts on stays with the lead, skips the Mono,
// waits for its row to be taken (a closed window never escalates), goes up
// before 15 minutes, goes past Tyler, or Tyler's notification shows twice.
#[test]
fn no_act_for_fifteen_minutes_goes_one_level_up_and_ends_at_tyler_once() {
    let mut world = World::new("alarm-escalate");
    let chain = chain(&world, "ale");
    let episode = stalled_worker(&mut world, &chain);
    let lead = format!("session:{}", chain.lead.as_str());
    let mono = format!("mono:{}", chain.mono.id.as_str());
    let giga = format!("mono:{}", chain.giga.id.as_str());
    assert_eq!(sends(&world, &episode), vec![(1, lead.clone())]);

    // No window takes level 1 or level 3; each still goes up at 15 min.
    for (level, recipient) in [(2, &mono), (3, &giga)] {
        world.advance(ESCALATE - MINUTE);
        assert_eq!(sends(&world, &episode).len(), level - 1, "at 14 min");
        world.advance(MINUTE + PASS);
        assert_eq!(
            sends(&world, &episode).last(),
            Some(&(i64::try_from(level).unwrap(), recipient.clone())),
            "level {level}, its row untaken"
        );
        if level == 3 {
            continue;
        }
        let first = take(&world, &test_taps::outbox_key(&episode, 1));
        assert!(
            first.contains(&format!(
                "No act in 15 min goes to {}.",
                chain.mono.id.as_str()
            )),
            "{first}"
        );
        let text = take(
            &world,
            &test_taps::outbox_key(&episode, i64::try_from(level).unwrap()),
        );
        assert!(text.starts_with(&format!(
            "<mono_alarm alarm=\"{episode}\" level=\"{level}\" cause=\"idle_open_work\">"
        )));
        if level == 2 {
            let lead_label = chain.lead.as_str();
            assert!(
                text.contains(&format!(
                    "{lead_label} ({lead}) did not act on its worker's stall for 15 min."
                )),
                "{text}"
            );
            assert!(text.contains(&format!("Act on {lead_label}: ")), "{text}");
        }
    }
    world.advance(ESCALATE + PASS);
    assert_eq!(
        sends(&world, &episode).last(),
        Some(&(4, "tyler".to_string()))
    );
    let shown = notify(&world);
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].episode, episode);
    assert_eq!(
        shown[0].title,
        format!("{} is stalled", chain.worker.as_str())
    );
    assert_eq!(notify(&world), Vec::new(), "shown once");
    world.advance(2 * HOUR);
    assert_eq!(sends(&world, &episode).len(), 4, "nothing after Tyler");
    assert_eq!(notify(&world), Vec::new());
}

// Defect: an act does not stop escalation, a nudge never reaches the
// agent's turn (a running one is steered, an idle one woken, through the
// lead's own sessions.send), or an act that changed nothing leaves the
// episode open forever instead of a new alarm N minutes later.
#[test]
fn a_nudge_stops_escalation_wakes_the_agent_and_no_progress_opens_a_new_episode() {
    let mut world = World::new("alarm-nudge");
    let chain = chain(&world, "aln");
    // Past its due, the worker's ask counts for the ceiling (review r3 M1), so
    // the nudged first turn below shows open work holds no alarm before a turn.
    let moved = world
        .conn()
        .execute(
            "UPDATE mono_asks SET due_active_ms = 0 WHERE child_actor = ?1",
            [format!("session:{}", chain.worker.as_str())],
        )
        .unwrap();
    assert_eq!(moved, 1);
    let set = result(call(
        &world,
        chain.lead.as_str(),
        "alarms.set",
        json!({"agent": chain.worker.as_str(), "stall_minutes": 30}),
    ));
    assert_eq!(set["stall_minutes"], 30);
    let episode = stalled_worker_after(&mut world, &chain, 30 * MINUTE);
    take(&world, &test_taps::outbox_key(&episode, 1));
    align(&mut world);
    let (nudged, sent) = seam(
        &world,
        &chain,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": episode}),
        "nudge-1",
        &ok(),
    );
    let worker = format!("session:{}", chain.worker.as_str());
    assert_eq!(nudged["result"]["target"], worker.as_str(), "{nudged}");
    let (window, from, action, to, when_busy, text) = steer_sent(&sent);
    assert_eq!(
        (
            window.as_str(),
            from.as_str(),
            action.as_str(),
            to.as_str(),
            when_busy.as_str()
        ),
        (
            "lead-window",
            chain.lead.as_str(),
            "sessions.send",
            chain.worker.as_str(),
            "steer"
        ),
    );
    assert!(
        text.starts_with(&format!(
            "<mono_nudge alarm=\"{episode}\" from=\"session:{}\">\nYour provisioner nudged you. Open work: ask ",
            chain.lead.as_str()
        )),
        "{text}"
    );
    assert!(
        ask_scheduler::pending(&world.conn(), &chain.worker)
            .unwrap()
            .is_empty(),
        "a session's nudge is no outbox row"
    );

    world.advance(ESCALATE + MINUTE);
    assert_eq!(sends(&world, &episode).len(), 1, "acted: no level 2");
    world.advance(30 * MINUTE - ESCALATE - MINUTE - PASS);
    assert_eq!(
        open_episodes(&world)[0].0,
        episode,
        "the worker's N (30 min) - 5 s after the act"
    );
    world.advance(PASS);
    assert_eq!(
        close_reason(&world, &episode).as_deref(),
        Some("act_no_effect")
    );
    world.advance(PASS);
    let open = open_episodes(&world);
    assert_eq!(open.len(), 1);
    assert_ne!(open[0].0, episode, "a new episode");
    assert_eq!(
        sends(&world, &open[0].0),
        vec![(1, format!("session:{}", chain.lead.as_str()))]
    );

    // The new alarm's nudge wakes the worker; its turn is progress.
    let renudged = open[0].0.clone();
    let (response, _) = seam(
        &world,
        &chain,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": renudged}),
        "nudge-2",
        &ok(),
    );
    assert_eq!(response["ok"], true, "{response}");
    turn(&mut world, &chain.worker, "woken", true);
    world.advance(PASS);
    assert_eq!(open_episodes(&world), Vec::new(), "the turn closed it");
}

// Defect: a dismiss loses its reason, or does not stop escalation; a
// plain message from the recipient to the agent is not an act.
#[test]
fn a_dismiss_records_its_reason_and_a_message_to_the_agent_is_an_act() {
    let mut world = World::new("alarm-dismiss");
    let chain = chain(&world, "ald");
    let episode = stalled_worker(&mut world, &chain);
    take(&world, &test_taps::outbox_key(&episode, 1));
    result(call(
        &world,
        chain.lead.as_str(),
        "alarms.dismiss",
        json!({"alarm": episode, "reason": "It waits on CI; I watch it."}),
    ));
    let logged: (String, String, Option<String>) = world
        .conn()
        .query_row(
            "SELECT actor, how, detail FROM alarm_acts WHERE episode_id = ?1",
            [&episode],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        logged,
        (
            format!("session:{}", chain.lead.as_str()),
            "dismiss".into(),
            Some("It waits on CI; I watch it.".into())
        )
    );
    world.advance(ESCALATE + PASS);
    assert_eq!(sends(&world, &episode).len(), 1, "dismissed: no level 2");

    world.advance(N);
    let [(fresh, _, _)] = <[_; 1]>::try_from(open_episodes(&world)).unwrap();
    {
        let conn = world.conn();
        deliver::message(
            &conn,
            chain.lead.as_str(),
            chain.worker.as_str(),
            "msg-1",
            (world.active_ms(), world.now()),
        )
        .unwrap();
        deliver::message(
            &conn,
            chain.mono.chat.as_str(),
            chain.worker.as_str(),
            "msg-2",
            (world.active_ms(), world.now()),
        )
        .unwrap();
    }
    let acts: Vec<(String, String)> = world
        .conn()
        .prepare("SELECT actor, how FROM alarm_acts WHERE episode_id = ?1 ORDER BY seq")
        .unwrap()
        .query_map([&fresh], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        acts,
        vec![(
            format!("session:{}", chain.lead.as_str()),
            "message".to_string()
        )],
        "the Mono's message to the worker is not its act"
    );
}

const PERMISSION: &str = r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf target"}}}"#;

// Defect: someone off the chain or the agent itself acts on it; a Mono
// answers its worker's dialog past its lead; report or set skip their
// checks.
#[test]
fn acts_are_refused_off_the_chain_and_answers_past_the_provisioner() {
    let mut world = World::new("alarm-auth");
    let chain = chain(&world, "ala");
    let stranger = world.add_session("ala-stranger", P, None);
    let claude = claude_tap(&world, &chain);
    let episode = blocked_worker(&mut world, &chain, &claude);
    let alarm = json!({"alarm": episode});
    let mono = chain.mono.chat.as_str();

    for (caller, action, input, expected) in [
        (
            stranger.as_str(),
            "alarms.read",
            alarm.clone(),
            "not_your_alarm",
        ),
        (
            chain.worker.as_str(),
            "alarms.dismiss",
            json!({"alarm": episode, "reason": "fine"}),
            "not_your_alarm",
        ),
        (
            chain.lead.as_str(),
            "alarms.read",
            json!({"alarm": "al_missing"}),
            "unknown_alarm",
        ),
        (
            chain.lead.as_str(),
            "alarms.read",
            json!({"alarm": episode, "extra": 1}),
            "invalid_input",
        ),
        (
            mono,
            "alarms.answer",
            json!({"alarm": episode, "allow": true}),
            "answer_via_recipient",
        ),
        (
            chain.lead.as_str(),
            "alarms.answer",
            json!({"alarm": episode, "allow": true, "deny": true}),
            "invalid_input",
        ),
        (
            chain.lead.as_str(),
            "alarms.answer",
            json!({"alarm": episode, "text": "yes"}),
            "invalid_input",
        ),
        (
            chain.lead.as_str(),
            "alarms.report",
            json!({"from": 0, "to": 1}),
            "mono_only",
        ),
        (
            mono,
            "alarms.set",
            json!({"agent": chain.worker.as_str(), "stall_minutes": 30}),
            "not_the_parent",
        ),
        (
            chain.lead.as_str(),
            "alarms.set",
            json!({"agent": chain.worker.as_str(), "stall_minutes": 4}),
            "invalid_input",
        ),
    ] {
        assert_eq!(
            code(call(&world, caller, action, input.clone())),
            expected,
            "{caller} {action} {input}"
        );
    }
    assert_eq!(
        code(call(&world, mono, "alarms.read", alarm.clone())),
        "ok",
        "a Mono above reads"
    );
    let set = result(call(
        &world,
        chain.lead.as_str(),
        "alarms.set",
        json!({"agent": chain.worker.as_str(), "stall_minutes": 30}),
    ));
    assert_eq!(set["stall_minutes"], 30);
}

/// The stdio tap `harness_spawn` gives the worker's Claude child, which it
/// spawned now.
fn claude_tap(world: &World, chain: &Chain) -> ChildTap {
    super::alarms_stall::claude_tap(world, &chain.worker)
}

/// The worker's turn asked a permission and it waited 3 minutes: its
/// dialog episode.
fn blocked_worker(world: &mut World, chain: &Chain, claude: &ChildTap) -> String {
    align(world);
    turn(world, &chain.worker, "t1", true);
    claude.line(Pipe::Stdout, PERMISSION);
    world.advance(3 * MINUTE);
    let [(episode, _, _)] = <[_; 1]>::try_from(open_episodes(world)).unwrap();
    episode
}

// Defect: the lead loses its acts once the alarm went past it (rights
// accumulate), a steer to a running session waits for its turn end, the
// Mono acts on the worker past its lead, a lower recipient dismisses the
// alarm above it, or a steer to a Mono chat is dropped when the episode
// closes; or a key the front end reads does not match its request id rule.
#[test]
fn every_recipient_so_far_acts_one_level_down_and_a_steer_outlives_its_episode() {
    let mut world = World::new("alarm-auth-up");
    let chain = chain(&world, "alu");
    let episode = stalled_worker(&mut world, &chain);
    world.advance(ESCALATE + PASS);
    assert_eq!(sends(&world, &episode).len(), 2);
    world.alarm_turn_started(&chain.lead, "lead-turn");
    let (lead, mono) = (chain.lead.as_str(), chain.mono.chat.as_str());

    let (nudged, sent) = seam(
        &world,
        &chain,
        lead,
        "alarms.nudge",
        json!({"alarm": episode}),
        "n-1",
        &ok(),
    );
    assert_eq!(nudged["ok"], true, "{nudged}");
    assert_eq!(
        steer_sent(&sent).3,
        chain.worker.as_str(),
        "the lead still nudges its worker"
    );
    let text = "Your worker is idle with its ask.";
    let (_, sent) = seam(
        &world,
        &chain,
        mono,
        "alarms.steer",
        json!({"alarm": episode, "text": text}),
        "s-1",
        &ok(),
    );
    let (window, from, action, to, when_busy, prompt) = steer_sent(&sent);
    assert_eq!(
        (
            window.as_str(),
            from.as_str(),
            action.as_str(),
            to.as_str(),
            when_busy.as_str()
        ),
        ("mono-window", mono, "sessions.send", lead, "steer"),
        "the Mono steers its running lead, never the worker"
    );
    assert!(prompt.contains(text), "{prompt}");
    assert_eq!(
        code(call(
            &world,
            lead,
            "alarms.dismiss",
            json!({"alarm": episode, "reason": "fine"})
        )),
        "alarm_moved_up"
    );
    let lead_actor = format!("session:{lead}");
    let mono_actor = format!("mono:{}", chain.mono.id.as_str());
    let worker_actor = format!("session:{}", chain.worker.as_str());
    assert_eq!(
        acts(&world, &episode),
        vec![
            (lead_actor.clone(), "nudge".into(), worker_actor),
            (mono_actor.clone(), "steer".into(), lead_actor),
        ]
    );

    // The Giga steers its Mono through the outbox; the worker's turn closes
    // the episode and the steer still goes out.
    let (steered, sent) = seam(
        &world,
        &chain,
        chain.giga.chat.as_str(),
        "alarms.steer",
        json!({"alarm": episode, "text": "Check your lead."}),
        "s-2",
        &ok(),
    );
    assert_eq!(sent, Vec::new(), "a Mono chat is steered by its outbox row");
    assert_eq!(steered["result"]["target"], mono_actor.as_str());
    let key = deliver::nudge_key(&episode, steered["result"]["seq"].as_i64().unwrap());
    ran_a_turn(&mut world, &chain.worker);
    world.advance(PASS);
    assert_eq!(close_reason(&world, &episode).as_deref(), Some("progress"));
    assert!(take(&world, &key).contains("Check your lead."));

    assert_front_end_keys(&world);
}

/// Every alarms key that reaches the front end matches its request id rule
/// (sessionStore keeps an appRequestId only if it matches).
fn assert_front_end_keys(world: &World) {
    let front_end = regex::Regex::new("^[A-Za-z0-9_-]+$").unwrap();
    let conn = world.conn();
    for sql in [
        "SELECT key FROM mono_outbox WHERE stage IN ('alarm', 'nudge')",
        "SELECT outbox_key FROM alarm_prompts",
        "SELECT outbox_key FROM alarm_acts WHERE outbox_key IS NOT NULL",
        "SELECT outbox_key FROM alarm_sends WHERE outbox_key IS NOT NULL",
        "SELECT id FROM alarm_episodes",
    ] {
        let keys: Vec<String> = conn
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!keys.is_empty(), "{sql}");
        for key in keys {
            assert!(front_end.is_match(&key), "{sql}: {key}");
        }
    }
}

// Defect (lead ruling b): raising N mid-episode closes it as work_cleared,
// so its escalation restarts and the report says the work went away.
#[test]
fn raising_n_mid_episode_keeps_the_episode_and_its_escalation() {
    let mut world = World::new("alarm-set-mid");
    let chain = chain(&world, "als");
    let episode = stalled_worker(&mut world, &chain);
    world.advance(5 * MINUTE);
    result(call(
        &world,
        chain.lead.as_str(),
        "alarms.set",
        json!({"agent": chain.worker.as_str(), "stall_minutes": 60}),
    ));
    world.advance(ESCALATE - 5 * MINUTE + PASS);
    assert_eq!(close_reason(&world, &episode), None);
    assert_eq!(open_episodes(&world)[0].0, episode);
    assert_eq!(
        sends(&world, &episode).len(),
        2,
        "level 2 at 15 min, as before"
    );
    ran_a_turn(&mut world, &chain.worker);
    world.advance(PASS);
    assert_eq!(close_reason(&world, &episode).as_deref(), Some("progress"));
}

fn message(world: &World, from: &str, to: &str, request_id: &str) {
    deliver::message(
        &world.conn(),
        from,
        to,
        request_id,
        (world.active_ms(), world.now()),
    )
    .unwrap();
}

// Defect (r2 M1): a routine message from a provisioner the alarm has not
// reached is logged as its act, so escalation stops and the alarm never
// goes past the lead while the Mono talks to it.
#[test]
fn a_plain_message_counts_only_from_a_recipient_the_alarm_reached() {
    let mut world = World::new("alarm-message-told");
    let chain = chain(&world, "alt");
    let episode = stalled_worker(&mut world, &chain);
    let (lead, mono) = (chain.lead.as_str(), chain.mono.chat.as_str());

    message(&world, mono, lead, "status-1");
    assert_eq!(acts(&world, &episode), Vec::new(), "the Mono was not told");
    world.advance(ESCALATE + PASS);
    assert_eq!(
        sends(&world, &episode).last(),
        Some(&(2, format!("mono:{}", chain.mono.id.as_str()))),
        "the Mono's message did not stop level 2"
    );

    message(&world, mono, lead, "status-2");
    assert_eq!(
        acts(&world, &episode),
        vec![(
            format!("mono:{}", chain.mono.id.as_str()),
            "message".to_string(),
            format!("session:{lead}")
        )],
        "told at level 2, the Mono's message to its lead is its act"
    );
    world.advance(ESCALATE + PASS);
    assert_eq!(sends(&world, &episode).len(), 2, "acted: no level 3");
}

// Defect (r3 m1, lead ruling a): the Giga's steer and nudge wait untaken
// as outbox rows at its Mono, whose chat no window has open, and count as
// the Mono's missed wakes: a second episode, at the Mono, alarms the Giga
// that just acted.
#[test]
fn a_gigas_untaken_steer_and_nudge_open_no_episode_at_its_mono() {
    let mut world = World::new("alarm-giga-acts");
    let chain = chain(&world, "alg");
    align(&mut world);
    // The Mono ran a turn, so the detector watches it too.
    ran_a_turn(&mut world, &chain.mono.chat);
    let episode = stalled_worker(&mut world, &chain);
    world.advance(2 * (ESCALATE + PASS));
    let mono = format!("mono:{}", chain.mono.id.as_str());
    let giga = chain.giga.chat.as_str();
    for (action, input) in [
        (
            "alarms.steer",
            json!({"alarm": episode, "text": "Check your lead."}),
        ),
        ("alarms.nudge", json!({"alarm": episode})),
    ] {
        let acted = result(call(&world, giga, action, input));
        assert_eq!(acted["target"], mono.as_str(), "{action}: {acted}");
        world.advance(PASS);
    }

    world.advance(10 * MINUTE);
    let untaken: i64 = world
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM mono_outbox
              WHERE stage = 'nudge' AND recipient = ?1 AND sent_at IS NULL",
            [&mono],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(untaken, 2, "both rows wait at the Mono");
    assert_eq!(
        open_episodes(&world),
        vec![(episode, format!("session:{}", chain.worker.as_str()), 3)],
        "one stall, one episode"
    );
}

const SAID: &str =
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running the tests."}]}}"#;

fn hh_mm(world: &World, wall: i64) -> String {
    world
        .conn()
        .query_row(
            "SELECT strftime('%H:%M', ?1 / 1000, 'unixepoch', 'localtime')",
            [wall],
            |row| row.get(0),
        )
        .unwrap()
}

// Defect (r3 m2): the alarm dates the last activity as now minus the
// active time since it, but the active clock stops while the Mac sleeps,
// so after a night's sleep the worker reads as active minutes ago, after
// its own stall began.
#[test]
fn the_last_activity_keeps_its_clock_time_across_a_sleep() {
    let mut world = World::new("alarm-last-activity");
    let chain = chain(&world, "all");
    let claude = claude_tap(&world, &chain);
    align(&mut world);
    turn(&mut world, &chain.worker, "t1", true);
    let worked = world.now();
    claude.line(Pipe::Stdout, SAID);
    world.advance(MINUTE);
    turn(&mut world, &chain.worker, "t1", false);
    world.advance(5 * MINUTE);
    world.sleep(8 * HOUR);
    world.advance(N - 5 * MINUTE);
    let [(episode, _, _)] = <[_; 1]>::try_from(open_episodes(&world)).unwrap();

    let text = take(&world, &test_taps::outbox_key(&episode, 1));
    assert!(
        text.contains(&format!("Last activity: {} output.", hh_mm(&world, worked))),
        "{text}"
    );
    let read = result(call(
        &world,
        chain.lead.as_str(),
        "alarms.read",
        json!({"alarm": episode}),
    ));
    assert_eq!(
        read["lastActivity"],
        format!("{} output", hh_mm(&world, worked)),
        "{read}"
    );
}

/// An episode row of the report: id, Mono, stalled since (wall), closed
/// (wall), acted by, level.
type Row = (
    &'static str,
    &'static str,
    i64,
    Option<i64>,
    Option<&'static str>,
    i64,
);

// Defect: the report counts minutes outside the window, before the stall,
// after the close, or mixes two Monos' minutes.
#[test]
fn the_report_gives_each_monos_exact_stall_minutes_in_a_night_window() {
    let world = World::new("alarm-report");
    let night = world.now();
    let to = night + 8 * HOUR;
    let conn = world.conn();
    let rows: [Row; 5] = [
        // Before the window into it: counts from :from, 30 min.
        (
            "al_r1",
            "mono-a",
            night - HOUR,
            Some(night + 30 * MINUTE),
            None,
            1,
        ),
        // Inside, acted, level 3: 45 min.
        (
            "al_r2",
            "mono-a",
            night + HOUR,
            Some(night + HOUR + 45 * MINUTE),
            Some("mono:b"),
            3,
        ),
        // Open past the window: counts to :to, 60 min.
        ("al_r3", "mono-b", to - HOUR, None, None, 2),
        // Wholly before the window: not counted.
        (
            "al_r4",
            "mono-b",
            night - 3 * HOUR,
            Some(night - 2 * HOUR),
            None,
            1,
        ),
        // Inside, 10 min.
        (
            "al_r5",
            "mono-b",
            night + 2 * HOUR,
            Some(night + 2 * HOUR + 10 * MINUTE),
            None,
            1,
        ),
    ];
    for (id, mono, stalled, closed, acted, level) in rows {
        conn.execute(
            "INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause, open_work_json,
               stalled_since_active_ms, stalled_since_wall_ms, opened_active_ms, opened_wall_ms,
               level, acted_by, closed_active_ms, closed_wall_ms, close_reason)
             VALUES (?1, 'session:s-' || ?1, ?2, 'tyler', 'idle_open_work', '[]', 0, ?3, 0, ?3,
               ?6, ?5, CASE WHEN ?4 IS NULL THEN NULL ELSE 1 END, ?4,
               CASE WHEN ?4 IS NULL THEN NULL ELSE 'progress' END)",
            rusqlite::params![id, mono, stalled, closed, acted, level],
        )
        .unwrap();
    }
    let report = act::report(&conn, night, to).unwrap();
    assert_eq!(
        report["monos"],
        json!([
            {"monoId": "mono-a", "episodes": 2, "stallMinutes": 75.0, "acted": 1, "maxLevel": 3},
            {"monoId": "mono-b", "episodes": 2, "stallMinutes": 70.0, "acted": 0, "maxLevel": 2},
        ])
    );
}
