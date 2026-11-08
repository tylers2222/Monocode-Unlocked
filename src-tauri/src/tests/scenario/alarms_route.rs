//! Alarms S2: an episode goes up the chain of provisioners on the active
//! clock's own tick, ends at Tyler's notification, and stops when a
//! recipient acts. Acts run as `control.rs` runs them: `act::call` on the
//! store with the caller's session.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::agents::{session_caller, Mono};
use super::world::World;
use crate::alarms::act::{self, Outcome, Refusal};
use crate::alarms::taps::{Harness, Pipe, Tap};
use crate::alarms::{test_taps, TylerAlarm};
use crate::ask_scheduler;
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;

const P: &str = "/p/alarm-route";
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const N: i64 = 15 * MINUTE;
const ESCALATE: i64 = 15 * MINUTE;
const PASS: i64 = 5 * SECOND;

/// A Giga, its Mono, the Mono's lead and the lead's worker, each with
/// the longest deadline on its ask, so no ask reminder runs.
struct Chain {
    giga: Mono,
    mono: Mono,
    lead: SessionId,
    worker: SessionId,
}

fn chain(world: &World, name: &str) -> Chain {
    let giga = world.add_mono_in(&format!("{name}-giga"), MonoRole::Giga, &[P]);
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

fn turn(world: &World, session: &SessionId, turn_id: &str, started: bool) {
    let conn = world.conn();
    let (at, wall) = (world.active_ms(), world.now());
    if started {
        test_taps::turn_started(&conn, session, turn_id, at, wall).unwrap();
    } else {
        test_taps::turn_ended(&conn, session, turn_id, at, wall).unwrap();
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
        Ok(Outcome::Answer(plan)) => panic!("an answer plan: {plan:?}"),
        Err(refusal) => panic!("refused: {refusal:?}"),
    }
}

fn code(outcome: Result<Outcome, Refusal>) -> &'static str {
    match outcome {
        Err(refusal) => refusal.code(),
        Ok(_) => "ok",
    }
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
// goes up before its level was taken or before 15 minutes, goes past Tyler,
// or Tyler's notification shows twice.
#[test]
fn no_act_for_fifteen_minutes_goes_one_level_up_and_ends_at_tyler_once() {
    let mut world = World::new("alarm-escalate");
    let chain = chain(&world, "ale");
    let episode = stalled_worker(&mut world, &chain);
    let lead = format!("session:{}", chain.lead.as_str());
    let mono = format!("mono:{}", chain.mono.id.as_str());
    let giga = format!("mono:{}", chain.giga.id.as_str());
    assert_eq!(sends(&world, &episode), vec![(1, lead.clone())]);

    world.advance(2 * ESCALATE);
    assert_eq!(sends(&world, &episode).len(), 1, "not taken: no escalation");
    let first = take(&world, &test_taps::outbox_key(&episode, 1));
    assert!(
        first.contains(&format!(
            "No act in 15 min goes to {}.",
            chain.mono.id.as_str()
        )),
        "{first}"
    );

    for (level, recipient) in [(2, &mono), (3, &giga)] {
        world.advance(ESCALATE - MINUTE);
        assert_eq!(sends(&world, &episode).len(), level - 1, "at 14 min");
        world.advance(MINUTE + PASS);
        assert_eq!(
            sends(&world, &episode).last(),
            Some(&(i64::try_from(level).unwrap(), recipient.clone())),
            "level {level}"
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
// agent, or an act that changed nothing leaves the episode open forever
// instead of a new alarm N minutes later.
#[test]
fn a_nudge_stops_escalation_wakes_the_agent_and_no_progress_opens_a_new_episode() {
    let mut world = World::new("alarm-nudge");
    let chain = chain(&world, "aln");
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
    let nudged = result(call(
        &world,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": episode}),
    ));
    let worker = format!("session:{}", chain.worker.as_str());
    assert_eq!(nudged["target"], worker.as_str());
    let key = format!("nudge:{episode}:1");
    assert!(ask_scheduler::pending(&world.conn(), &chain.worker)
        .unwrap()
        .contains(&key));
    let text = take(&world, &key);
    assert!(
        text.starts_with(&format!(
            "<mono_nudge alarm=\"{episode}\" from=\"session:{}\">\nYour provisioner nudged you. Open work: ask ",
            chain.lead.as_str()
        )),
        "{text}"
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
    let closed: String = world
        .conn()
        .query_row(
            "SELECT close_reason FROM alarm_episodes WHERE id = ?1",
            [&episode],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(closed, "act_no_effect");
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
    result(call(
        &world,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": renudged}),
    ));
    turn(&world, &chain.worker, "woken", true);
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
        act::message(
            &conn,
            chain.lead.as_str(),
            chain.worker.as_str(),
            "msg-1",
            (world.active_ms(), world.now()),
        )
        .unwrap();
        act::message(
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
const ANSWER: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{"behavior":"allow"}}}"#;

// Defect: someone off the chain, the agent itself or a recipient the alarm
// left behind acts on it; a Mono answers its worker's dialog past its lead;
// an answer goes to a dialog that is gone; report or set skip their checks.
#[test]
fn acts_are_refused_to_all_but_the_current_recipient_and_those_above() {
    let mut world = World::new("alarm-auth");
    let chain = chain(&world, "ala");
    let stranger = world.add_session("ala-stranger", P, None);
    let mut claude = Tap::new(Harness::Claude);
    let episode = blocked_worker(&mut world, &chain, &mut claude);
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

/// Line `line` of the worker's Claude reaches its tap now.
fn tap(world: &World, chain: &Chain, claude: &mut Tap, pipe: Pipe, line: &str) {
    let now = world.active_ms();
    if let Some(row) = claude.line(pipe, line, now) {
        test_taps::write_tap(&world.conn(), &chain.worker, &row, world.now()).unwrap();
        claude.wrote(row, now);
    }
}

/// The worker's turn asked a permission and it waited 3 minutes: its
/// dialog episode.
fn blocked_worker(world: &mut World, chain: &Chain, claude: &mut Tap) -> String {
    align(world);
    turn(world, &chain.worker, "t1", true);
    tap(world, chain, claude, Pipe::Stdout, PERMISSION);
    world.advance(3 * MINUTE);
    let [(episode, _, _)] = <[_; 1]>::try_from(open_episodes(world)).unwrap();
    episode
}

// Defect: an answer reaches another session than the stalled agent's, or
// a dialog that is gone.
#[test]
fn the_lead_answers_its_workers_dialog_through_the_workers_window_once() {
    let mut world = World::new("alarm-answer");
    let chain = chain(&world, "alw");
    let mut claude = Tap::new(Harness::Claude);
    let episode = blocked_worker(&mut world, &chain, &mut claude);
    let plan = match call(
        &world,
        chain.lead.as_str(),
        "alarms.answer",
        json!({"alarm": episode, "allow": true}),
    ) {
        Ok(Outcome::Answer(plan)) => plan,
        other => panic!("{:?}", other.map(|_| ()).map_err(|refusal| refusal.code())),
    };
    assert_eq!(
        plan.session,
        chain.worker.as_str(),
        "the worker's window answers"
    );
    assert_eq!(
        plan.forward,
        json!({"alarm": episode, "kind": "permission", "decision": "allow"})
    );
    let answered = act::answered(&world.conn(), &plan, (world.active_ms(), world.now())).unwrap();
    assert_eq!(answered["acted"], "answer");

    tap(&world, &chain, &mut claude, Pipe::Stdin, ANSWER);
    assert_eq!(
        code(call(
            &world,
            chain.lead.as_str(),
            "alarms.answer",
            json!({"alarm": episode, "deny": true})
        )),
        "dialog_gone"
    );
}

// Defect: the lead an alarm went past still acts on it, or the Mono it
// reached acts on the worker past its lead.
#[test]
fn a_recipient_the_alarm_left_behind_is_refused_and_the_mono_steers_its_lead() {
    let mut world = World::new("alarm-auth-up");
    let chain = chain(&world, "alu");
    let episode = stalled_worker(&mut world, &chain);
    take(&world, &test_taps::outbox_key(&episode, 1));
    world.advance(ESCALATE + PASS);
    assert_eq!(sends(&world, &episode).len(), 2);
    assert_eq!(
        code(call(
            &world,
            chain.lead.as_str(),
            "alarms.nudge",
            json!({"alarm": episode})
        )),
        "alarm_moved_up"
    );
    let steered = result(call(
        &world,
        chain.mono.chat.as_str(),
        "alarms.steer",
        json!({"alarm": episode, "text": "Your worker is idle with its ask."}),
    ));
    assert_eq!(
        steered["target"],
        format!("session:{}", chain.lead.as_str()),
        "the Mono steers its lead, never the worker"
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
