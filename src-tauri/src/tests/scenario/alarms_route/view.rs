//! Alarms U2: what the front end reads (`alarms_view`), the settings and
//! stall minutes the app user writes, Tyler's own acts, his answers to his
//! holds in the app, and a chat card's act as its session
//! (`control_alarm_act`).

use super::*;
use crate::alarms::act::TylerActed;
use crate::alarms::view::{self, AlarmsSnapshot};
use crate::alarms::Settings;
use crate::mono_messages::Tool;
use crate::sleep_hold::service::{self, DENIED_FROM_BANNER};
use crate::sleep_hold::store::ANSWER_MS;
use crate::sleep_hold::{classify, Harness, HoldRequest};

fn snapshot(world: &World) -> Value {
    let read: AlarmsSnapshot =
        view::snapshot(&world.conn(), (world.active_ms(), world.now())).unwrap();
    serde_json::to_value(read).unwrap()
}

fn agent(actor: &str, name: &str, role: &str, session: Option<&str>) -> Value {
    json!({"actor": actor, "name": name, "role": role, "sessionId": session})
}

fn worker_wire(chain: &Chain) -> Value {
    let worker = chain.worker.as_str();
    agent(&format!("session:{worker}"), worker, "worker", Some(worker))
}

fn lead_wire(chain: &Chain) -> Value {
    let lead = chain.lead.as_str();
    agent(&format!("session:{lead}"), lead, "lead", Some(lead))
}

fn mono_wire(mono: &Mono, role: &str) -> Value {
    let id = mono.id.as_str();
    agent(&format!("mono:{id}"), id, role, Some(mono.chat.as_str()))
}

fn tyler_act(
    world: &World,
    alarm: &str,
    how: &str,
    detail: Option<&str>,
    request_id: &str,
) -> Result<TylerActed, Refusal> {
    act::tyler_act(
        &world.conn(),
        alarm,
        how,
        detail,
        request_id,
        (world.active_ms(), world.now()),
    )
}

fn tyler_code(acted: Result<TylerActed, Refusal>) -> &'static str {
    acted.map_or_else(|refusal| refusal.code(), |_| "ok")
}

/// The worker's alarm went up to Tyler: level 4.
fn at_tyler(world: &mut World, episode: &str) {
    world.advance(3 * (ESCALATE + PASS));
    assert_eq!(sends(world, episode).last().unwrap().1, "tyler");
}

// Defect: the front end reads a stalled worker with the wrong recipient,
// role or session per level, no `missed` once it went past the lead, no
// next recipient or minutes, or an act with no one's name on it.
#[test]
fn the_snapshot_shows_a_stalled_worker_at_each_level_and_who_acted() {
    let mut world = World::new("alarm-view-levels");
    let chain = chain(&world, "avl");
    let episode = stalled_worker(&mut world, &chain);
    let read = snapshot(&world);
    let [shown] = read["episodes"].as_array().unwrap().as_slice() else {
        panic!("one episode: {read}");
    };
    assert_eq!(shown["id"], episode.as_str());
    assert_eq!(shown["agent"], worker_wire(&chain));
    assert_eq!(shown["cause"], "idle_open_work");
    assert_eq!(
        (shown["level"].clone(), shown["minutes"].clone()),
        (json!(1), json!(15))
    );
    assert!(
        shown["openWork"][0].as_str().unwrap().starts_with("ask "),
        "{shown}"
    );
    assert_eq!(shown["dialog"], Value::Null);
    assert_eq!(shown["acted"], Value::Null);
    assert_eq!(shown["nextMinutes"], 15);
    assert_eq!(
        shown["sends"],
        json!([{
            "level": 1, "recipient": lead_wire(&chain), "at": null, "missed": null,
            "next": {"kind": "agent", "agent": mono_wire(&chain.mono, "mono")},
        }])
    );

    take(&world, &test_taps::outbox_key(&episode, 1));
    let taken = hh_mm(&world, world.now());
    world.advance(ESCALATE + PASS);
    let read = snapshot(&world);
    let shown = &read["episodes"][0];
    assert_eq!(shown["level"], 2);
    assert_eq!(shown["sends"][0]["at"], taken);
    assert_eq!(
        shown["sends"][1],
        json!({
            "level": 2, "recipient": mono_wire(&chain.mono, "mono"), "at": null,
            "missed": "its worker's stall",
            "next": {"kind": "agent", "agent": mono_wire(&chain.giga, "giga")},
        })
    );
    assert_eq!(shown["nextMinutes"], 15);

    let (nudged, _) = seam(
        &world,
        &chain,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": episode}),
        "n-1",
        &ok(),
    );
    assert_eq!(nudged["ok"], true, "{nudged}");
    // `control.rs` stamps an act with the computer's wall clock.
    let at: i64 = world
        .conn()
        .query_row(
            "SELECT at_wall_ms FROM alarm_acts WHERE episode_id = ?1",
            [&episode],
            |row| row.get(0),
        )
        .unwrap();
    let read = snapshot(&world);
    assert_eq!(
        read["episodes"][0]["acted"],
        json!({"how": "nudge", "by": chain.lead.as_str(), "at": hh_mm(&world, at)})
    );
}

// Defect: the level that reached Tyler shows a recipient, a next level or
// minutes to it, or the level below does not say Tyler is next.
#[test]
fn the_snapshot_shows_tylers_level_as_the_last() {
    let mut world = World::new("alarm-view-tyler");
    let chain = chain(&world, "avt");
    let episode = stalled_worker(&mut world, &chain);
    at_tyler(&mut world, &episode);
    let read = snapshot(&world);
    let shown = &read["episodes"][0];
    assert_eq!(shown["level"], 4);
    assert_eq!(shown["nextMinutes"], Value::Null);
    assert_eq!(shown["sends"][2]["next"], json!({"kind": "tyler"}));
    assert_eq!(shown["sends"][2]["missed"], "the level-2 alarm");
    assert_eq!(
        (
            shown["sends"][3]["recipient"].clone(),
            shown["sends"][3]["next"].clone(),
            shown["sends"][3]["missed"].clone()
        ),
        (Value::Null, Value::Null, json!("the level-3 alarm"))
    );
}

// Defect: a dialog episode loses the request id of the dialog it named, so
// the front end cannot answer that one dialog; or a Mono's episode has no
// session to open.
#[test]
fn the_snapshot_names_a_dialogs_request_and_a_monos_chat() {
    let mut world = World::new("alarm-view-dialog");
    let chain = chain(&world, "avd");
    let claude = claude_tap(&world, &chain);
    blocked_worker(&mut world, &chain, &claude);
    let read = snapshot(&world);
    let dialog = &read["episodes"][0]["dialog"];
    assert_eq!(
        (
            dialog["kind"].clone(),
            dialog["minutes"].clone(),
            dialog["requestId"].clone()
        ),
        (json!("permission"), json!(3), json!("req_1")),
        "{dialog}"
    );

    let mut world = World::new("alarm-view-mono");
    let giga = world.add_mono_in("avm-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("avm-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    let input = json!({
        "message": "Is CI green?", "send_to": mono.id.as_str(),
        "wanting_reply": false, "intent": "new",
    });
    world.call(&giga, "g1", "wake-1", Tool::SendMessage, input, &mono);
    // The wake threshold, then the one retry's wait (ADE-276).
    world.advance(4 * MINUTE + 2 * PASS);
    let read = snapshot(&world);
    let shown = &read["episodes"][0];
    assert_eq!(shown["cause"], "wake_missed");
    assert_eq!(shown["agent"], mono_wire(&mono, "mono"));
    assert_eq!(shown["sends"][0]["recipient"], mono_wire(&giga, "giga"));
}

fn hold_wire(tool_use_id: &str, command: &str) -> Value {
    serde_json::to_value(HoldRequest {
        harness: Harness::Claude,
        session_hint: None,
        tool_use_id: tool_use_id.into(),
        command: command.into(),
        background: false,
        reason: Some("Wait for CI".into()),
        wait: classify(command, false).unwrap(),
    })
    .unwrap()
}

fn hold(world: &World, session: &SessionId, tool_use_id: &str) -> String {
    service::request(
        &world.conn(),
        session.as_str(),
        &hold_wire(tool_use_id, "sleep 60"),
        (world.active_ms(), world.now()),
    )
    .unwrap()
    .0
}

fn set_setting(world: &World, key: &str, value: &str) -> Result<(), String> {
    view::set_setting(&world.conn(), key, value)
}

fn in_app(
    world: &World,
    hold: &str,
    allow: bool,
    reason: Option<&str>,
    request_id: &str,
) -> Result<bool, Refusal> {
    service::tyler_decides_in_app(
        &world.conn(),
        hold,
        allow,
        reason,
        request_id,
        (world.active_ms(), world.now()).into(),
    )
}

// Defect: Tyler's card misses one of his holds, lists an agent's or a
// skipped one, counts down a decided one, or cannot tell an answer from
// the app from one from the banner.
#[test]
fn the_snapshot_lists_tylers_holds_and_how_each_was_decided() {
    let mut world = World::new("alarm-view-holds");
    let chain = chain(&world, "avh");
    let solo = world.add_session("avh-solo", P, None);
    let undecided = hold(&world, &solo, "toolu_1");
    let in_the_app = hold(&world, &solo, "toolu_2");
    let banner = hold(&world, &solo, "toolu_3");
    hold(&world, &chain.worker, "toolu_4");
    set_setting(&world, "tyler_sleep_hold", "off").unwrap();
    hold(&world, &solo, "toolu_5");
    world.advance(10 * SECOND);
    assert!(in_app(&world, &in_the_app, true, None, "card-1").unwrap());
    assert!(service::tyler_decides(
        &world.conn(),
        &banner,
        false,
        Some(DENIED_FROM_BANNER),
        None,
        (world.active_ms(), world.now()).into(),
    )
    .unwrap());

    let read = snapshot(&world);
    let listed = |id: &str| -> Value {
        read["holds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hold| hold["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut ids: Vec<&str> = read["holds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hold| hold["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![undecided.as_str(), in_the_app.as_str(), banner.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected, "not the worker's, not the skipped one");
    let holds = [listed(&undecided), listed(&in_the_app), listed(&banner)];
    let solo_wire = agent("session:avh-solo", "avh-solo", "session", Some("avh-solo"));
    assert_eq!(
        holds[0],
        json!({
            "id": undecided, "sessionId": "avh-solo", "toolUseId": "toolu_1",
            "agent": solo_wire, "command": "sleep 60", "howLong": "1 min",
            "reason": "Wait for CI", "remainingMs": ANSWER_MS - 10 * SECOND,
            "windowMs": ANSWER_MS, "decision": null,
        })
    );
    let at = hh_mm(&world, world.now());
    assert_eq!(holds[1]["remainingMs"], 0);
    assert_eq!(
        holds[1]["decision"],
        json!({"kind": "allow", "by": "Tyler", "at": at, "via": "app", "reason": null})
    );
    assert_eq!(
        holds[2]["decision"],
        json!({
            "kind": "deny", "by": "Tyler", "at": at, "via": "notification",
            "reason": "denied from notification",
        })
    );

    world.jump_wall(7 * HOUR);
    let read = snapshot(&world);
    let ids: Vec<&str> = read["holds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hold| hold["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&in_the_app.as_str()) && !ids.contains(&banner.as_str()),
        "decided holds go after 6 h: {ids:?}"
    );
}

// Defect: the in-app card answers a hold that is not Tyler's, a second
// answer overrides the first, a deny loses its reason, or a retried
// request reads as refused.
#[test]
fn tyler_answers_his_hold_in_the_app_once() {
    let world = World::new("alarm-view-decide");
    let chain = chain(&world, "avc");
    let solo = world.add_session("avc-solo", P, None);
    let allowed = hold(&world, &solo, "toolu_1");
    let denied = hold(&world, &solo, "toolu_2");
    let default_reason = hold(&world, &solo, "toolu_3");
    let agents = hold(&world, &chain.worker, "toolu_4");

    assert!(in_app(&world, &allowed, true, None, "c-1").unwrap());
    assert!(
        in_app(&world, &allowed, true, None, "c-1").unwrap(),
        "a retry of the same request"
    );
    assert!(
        !in_app(&world, &allowed, false, None, "c-2").unwrap(),
        "the first stands"
    );
    assert!(in_app(&world, &denied, false, Some("Use CI instead."), "c-3").unwrap());
    assert!(in_app(&world, &default_reason, false, None, "c-4").unwrap());
    type Logged = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let logged = |hold: &str| -> Logged {
        world
            .conn()
            .query_row(
                "SELECT decision, decided_by, decision_reason, decided_request_id
                   FROM sleep_holds WHERE id = ?1",
                [hold],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    };
    assert_eq!(
        logged(&allowed),
        (
            Some("allow".into()),
            Some("tyler".into()),
            None,
            Some("c-1".into())
        )
    );
    assert_eq!(
        logged(&denied),
        (
            Some("deny".into()),
            Some("tyler".into()),
            Some("Use CI instead.".into()),
            Some("c-3".into())
        )
    );
    assert_eq!(
        logged(&default_reason).2.as_deref(),
        Some("denied in the app")
    );

    let refused = |hold: &str, allow: bool, reason: Option<&str>, request_id: &str| {
        in_app(&world, hold, allow, reason, request_id)
            .map_or_else(|refusal| refusal.code(), |_| "ok")
    };
    assert_eq!(refused(&agents, true, None, "c-5"), "not_your_hold");
    assert_eq!(refused("sh_missing", true, None, "c-6"), "unknown_hold");
    assert_eq!(refused(&agents, true, Some("why"), "c-7"), "invalid_input");
    assert_eq!(refused(&agents, true, None, "bad id"), "invalid_input");
    assert_eq!(logged(&agents).0, None, "untouched");
}

// Defect: a write the app makes does not reach Settings or the snapshot,
// a bad key or value is stored, or a root Giga that is no Giga is shown
// as the root (#543 nit).
#[test]
fn the_app_writes_settings_and_stall_minutes_the_snapshot_reads() {
    let world = World::new("alarm-view-settings");
    let chain = chain(&world, "avs");
    world
        .conn()
        .execute(
            "INSERT INTO alarm_settings (key, value) VALUES ('root_giga', 'deleted-giga')",
            [],
        )
        .unwrap();
    let giga = json!({"id": chain.giga.id.as_str(), "name": chain.giga.id.as_str()});
    let read = snapshot(&world);
    assert_eq!(
        read["settings"],
        json!({
            "stallMinutes": 15, "escalateMinutes": 15, "sleepHold": true,
            "tylerSleepHold": true, "shortSleepSkipSeconds": 10,
            "rootGiga": giga, "gigas": [giga],
        }),
        "the real Giga, not the stored id"
    );

    for (key, value) in [
        ("stall_minutes", "30"),
        ("sleep_hold", "off"),
        ("tyler_sleep_hold", "off"),
        ("root_giga", chain.giga.id.as_str()),
    ] {
        set_setting(&world, key, value).unwrap();
    }
    let settings = Settings::load(&world.conn()).unwrap();
    assert_eq!(
        (
            settings.stall_minutes,
            settings.sleep_hold,
            settings.tyler_sleep_hold,
            settings.root_giga.as_deref()
        ),
        (30, false, false, Some(chain.giga.id.as_str()))
    );
    let read = snapshot(&world);
    assert_eq!(
        (
            read["settings"]["stallMinutes"].clone(),
            read["settings"]["sleepHold"].clone(),
            read["settings"]["tylerSleepHold"].clone()
        ),
        (json!(30), json!(false), json!(false))
    );
    for (key, value) in [
        ("dialog_minutes", "5"),
        ("escalate_minutes", "5"),
        ("stall_minutes", "4"),
        ("stall_minutes", "241"),
        ("stall_minutes", "soon"),
        ("sleep_hold", "yes"),
        ("root_giga", "no-such-giga"),
        ("root_giga", chain.mono.id.as_str()),
    ] {
        assert!(set_setting(&world, key, value).is_err(), "{key} = {value}");
    }
    assert_eq!(
        Settings::load(&world.conn()).unwrap(),
        settings,
        "unchanged"
    );

    let set = |session: &str, minutes| view::set_stall_minutes(&world.conn(), session, minutes);
    set(chain.worker.as_str(), Some(30)).unwrap();
    set(chain.mono.chat.as_str(), Some(240)).unwrap();
    assert!(set(chain.worker.as_str(), Some(4)).is_err());
    assert!(set("no-such-session", Some(30)).is_err());
    let worker = chain.worker.as_str();
    assert_eq!(
        snapshot(&world)["stallMinutes"],
        json!([
            {"actor": format!("mono:{}", chain.mono.id.as_str()),
             "sessionId": chain.mono.chat.as_str(), "minutes": 240},
            {"actor": format!("session:{worker}"), "sessionId": worker, "minutes": 30},
        ]),
        "a Mono chat sets its Mono's"
    );
    set(chain.mono.chat.as_str(), None).unwrap();
    assert_eq!(
        snapshot(&world)["stallMinutes"],
        json!([{"actor": format!("session:{worker}"), "sessionId": worker, "minutes": 30}])
    );
}

// Defect: Tyler acts before the alarm reached him, a retry logs a second
// act, his act is logged as someone else's, his nudge differs from the one
// a provisioner's nudge gives the agent, or he answers a dialog other
// than the one the alarm named.
#[test]
fn tyler_acts_on_the_stalled_agent_once_his_level_is_reached() {
    let mut world = World::new("alarm-view-tyler-act");
    let chain = chain(&world, "ava");
    let episode = stalled_worker(&mut world, &chain);
    assert_eq!(
        tyler_code(tyler_act(&world, &episode, "nudge", None, "t-0")),
        "not_your_alarm"
    );
    at_tyler(&mut world, &episode);
    for (how, detail, request_id) in [
        ("steer", None, "t-1"),
        ("dismiss", Some("  "), "t-1"),
        ("nudge", Some("now"), "t-1"),
        ("poke", None, "t-1"),
        ("nudge", None, "bad id"),
    ] {
        assert_eq!(
            tyler_code(tyler_act(&world, &episode, how, detail, request_id)),
            "invalid_input",
            "{how} {detail:?} {request_id}"
        );
    }
    assert_eq!(
        tyler_code(tyler_act(&world, &episode, "answer", Some("allow"), "t-2")),
        "dialog_gone",
        "no dialog to answer"
    );

    let nudged = tyler_act(&world, &episode, "nudge", None, "t-3").unwrap();
    let prompt = nudged.result["prompt"].as_str().unwrap().to_string();
    assert!(
        prompt.starts_with(&format!(
            "<mono_nudge alarm=\"{episode}\" from=\"tyler\">\nYour provisioner nudged you. Open work: ask "
        )),
        "{prompt}"
    );
    assert_eq!(nudged.result["acted"], "nudge");
    assert_eq!(
        nudged.result["target"],
        format!("session:{}", chain.worker.as_str())
    );
    let retried = tyler_act(&world, &episode, "nudge", None, "t-3").unwrap();
    assert_eq!(retried.result["duplicate"], true);
    assert_eq!(retried.result["seq"], nudged.result["seq"]);
    assert_eq!(retried.result["prompt"], prompt.as_str());
    let worker = format!("session:{}", chain.worker.as_str());
    assert_eq!(
        acts(&world, &episode),
        vec![("tyler".into(), "nudge".into(), worker.clone())]
    );
    assert_eq!(
        ask_scheduler::pending(&world.conn(), &chain.worker)
            .unwrap()
            .len(),
        0,
        "no outbox row: the front end sends it"
    );
    let read = snapshot(&world);
    assert_eq!(read["episodes"][0]["acted"]["by"], "Tyler");
    assert_eq!(read["episodes"][0]["acted"]["how"], "nudge");
    let steered = tyler_act(&world, &episode, "steer", Some("Run the tests."), "t-4").unwrap();
    assert_eq!(steered.result["prompt"], Value::Null);
    assert_eq!(acts(&world, &episode).len(), 2);
}

// Defect: Tyler's answer is logged for a dialog the agent no longer waits
// on, or not for the one the alarm named.
#[test]
fn tylers_answer_needs_the_dialog_the_alarm_named() {
    let mut world = World::new("alarm-view-tyler-answer");
    let chain = chain(&world, "avq");
    let claude = claude_tap(&world, &chain);
    let episode = blocked_worker(&mut world, &chain, &claude);
    at_tyler(&mut world, &episode);
    let stale = |world: &World, request: &str| {
        world
            .conn()
            .execute(
                "UPDATE agent_activity SET dialog_request_id = ?2 WHERE session_id = ?1",
                [chain.worker.as_str(), request],
            )
            .unwrap();
    };
    stale(&world, "req_9");
    assert_eq!(
        tyler_code(tyler_act(&world, &episode, "answer", Some("allow"), "a-1")),
        "dialog_gone"
    );
    stale(&world, "req_1");
    assert_eq!(
        tyler_code(tyler_act(&world, &episode, "answer", Some("maybe"), "a-2")),
        "invalid_input",
        "a permission is allow or deny"
    );
    let answered = tyler_act(&world, &episode, "answer", Some("allow"), "a-3").unwrap();
    assert_eq!(answered.result["acted"], "answer");
    assert!(answered.answered_plan.is_none());
    let worker = format!("session:{}", chain.worker.as_str());
    assert_eq!(
        acts(&world, &episode),
        vec![("tyler".into(), "answer".into(), worker)]
    );
}

// Defect: a chat card's act runs as another session than the card's, so
// the act is logged for the wrong provisioner, or its steer goes out of
// another window; or a bad request id or a non-alarms action runs.
#[test]
fn a_chat_cards_act_runs_as_its_session_in_its_window() {
    let mut world = World::new("alarm-view-card");
    let chain = chain(&world, "avk");
    let episode = stalled_worker(&mut world, &chain);
    let stranger = world.add_session("avk-stranger", P, None);
    // Only the worker is loaded, so the lead's send goes out of the window
    // the grant names: the card's.
    let loaded = [("worker-window", chain.worker.as_str())];
    let card = |session: &str, action: &str, input: Value, request_id: &str| {
        alarm_seam::ui_call(
            world.store(),
            &loaded,
            ("card-window", session),
            action,
            input,
            request_id,
            &ok(),
        )
    };
    let nudge = json!({"alarm": episode});
    let (refused, sent) = card(stranger.as_str(), "alarms.nudge", nudge.clone(), "k-1");
    assert_eq!(refused["code"], "not_your_alarm");
    assert_eq!(sent, Vec::new());
    for (action, request_id) in [("sessions.send", "k-2"), ("alarms.nudge", "bad id")] {
        let (refused, sent) = card(chain.lead.as_str(), action, nudge.clone(), request_id);
        assert_eq!(refused["ok"], false, "{action} {request_id}");
        assert_eq!(sent, Vec::new());
    }
    assert_eq!(acts(&world, &episode), Vec::new());

    let (nudged, sent) = card(chain.lead.as_str(), "alarms.nudge", nudge, "k-3");
    assert_eq!(nudged["ok"], true, "{nudged}");
    let (window, from, action, to, when_busy, _) = steer_sent(&sent);
    assert_eq!(
        (
            window.as_str(),
            from.as_str(),
            action.as_str(),
            to.as_str(),
            when_busy.as_str()
        ),
        (
            "card-window",
            chain.lead.as_str(),
            "sessions.send",
            chain.worker.as_str(),
            "steer"
        ),
        "the lead's own send, from the card's window"
    );
    assert_eq!(
        acts(&world, &episode),
        vec![(
            format!("session:{}", chain.lead.as_str()),
            "nudge".into(),
            format!("session:{}", chain.worker.as_str())
        )]
    );
}

// Defect: the digest does not move when an episode opens, is acted on or
// a hold is decided, so the front end shows a stale alarm.
#[test]
fn the_digest_moves_with_the_rows_the_snapshot_shows() {
    let mut world = World::new("alarm-view-digest");
    let chain = chain(&world, "avg");
    let digest = |world: &World| {
        let rows = view::digest_rows(&world.conn(), world.now()).unwrap();
        view::digest(&rows, world.active_ms())
    };
    let quiet = digest(&world);
    let episode = stalled_worker(&mut world, &chain);
    let opened = digest(&world);
    assert_ne!(opened, quiet, "an episode opened");
    result(call(
        &world,
        chain.lead.as_str(),
        "alarms.dismiss",
        json!({"alarm": episode, "reason": "CI"}),
    ));
    let dismissed = digest(&world);
    assert_ne!(dismissed, opened, "acted");
    let solo = world.add_session("avg-solo", P, None);
    let held = hold(&world, &solo, "toolu_1");
    let requested = digest(&world);
    assert_ne!(requested, dismissed, "a Tyler hold is listed");
    assert!(in_app(&world, &held, true, None, "g-1").unwrap());
    assert_ne!(digest(&world), requested, "decided");
}
