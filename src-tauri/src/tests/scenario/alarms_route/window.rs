//! Alarms S2: the acts a window does. An answer goes to the window that
//! owns the stalled agent's session; a plan's answer and a Mono's steer
//! go out as the caller's own `sessions.send`.

use super::*;

const ANSWER: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{"behavior":"allow"}}}"#;

const PERMISSION_2: &str = r#"{"type":"control_request","request_id":"req_2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"git push --force"}}}"#;

// Defect: an answer reaches another session than the stalled agent's, or a
// dialog other than the one the alarm named; it is logged before the
// window answered; a retry answers twice or reports a failure.
#[test]
fn the_lead_answers_its_workers_dialog_through_the_workers_window_once() {
    let mut world = World::new("alarm-answer");
    let chain = chain(&world, "alw");
    let claude = claude_tap(&world, &chain);
    let episode = blocked_worker(&mut world, &chain, &claude);
    let input = json!({"alarm": episode, "allow": true});
    let lead = chain.lead.as_str();

    let gone = json!({"ok": false, "code": "dialog_gone"});
    let (refused, _) = seam(
        &world,
        &chain,
        lead,
        "alarms.answer",
        input.clone(),
        "a-0",
        &gone,
    );
    assert_eq!(refused["code"], "dialog_gone");
    assert_eq!(
        acts(&world, &episode),
        Vec::new(),
        "no act before the window answered"
    );

    let (answered, sent) = seam(
        &world,
        &chain,
        lead,
        "alarms.answer",
        input.clone(),
        "a-1",
        &ok(),
    );
    assert_eq!(answered["result"]["acted"], "answer", "{answered}");
    assert_eq!(
        sent,
        vec![(
            "worker-window".to_string(),
            chain.worker.as_str().to_string(),
            "alarms.answer".to_string(),
            json!({"alarm": episode, "kind": "permission", "decision": "allow", "dialogRequestId": "req_1"}),
        )],
        "the worker's window answers the dialog the alarm named"
    );

    // The user answered req_1 and the agent asked req_2; the pass closes
    // the episode. A retry of the answer is the first answer; a new one is
    // refused.
    claude.line(Pipe::Stdin, ANSWER);
    claude.line(Pipe::Stdout, PERMISSION_2);
    world.advance(PASS);
    let (retried, sent) = seam(
        &world,
        &chain,
        lead,
        "alarms.answer",
        input.clone(),
        "a-1",
        &ok(),
    );
    assert_eq!(retried["result"]["duplicate"], true, "{retried}");
    assert_eq!(retried["result"]["seq"], answered["result"]["seq"]);
    assert_eq!(sent, Vec::new(), "no second answer");
    let (late, _) = seam(&world, &chain, lead, "alarms.answer", input, "a-2", &ok());
    assert_eq!(late["code"], "alarm_closed");
    assert_eq!(acts(&world, &episode).len(), 1);
}

/// alarmsMonoSteer.fixtures.json: the `sessions.send` a Mono's steer sends
/// its window. agentAppAlarms.test.ts runs the same input through the
/// window's `handleAgentApp`, so both ends of the forward are tested.
fn mono_steer_fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src/features/agent-app/model/alarmsMonoSteer.fixtures.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

// Defect (r2 M2): the steer a Mono with two projects forwards names no
// project, so its window cannot find the lead's and refuses the send.
#[test]
fn a_monos_steer_names_its_leads_project_for_its_window() {
    let mut world = World::new("alarm-mono-project");
    let chain = chain(&world, "alp");
    let episode = stalled_worker(&mut world, &chain);
    world.advance(ESCALATE + PASS);
    let fixture = mono_steer_fixture();
    assert_eq!(fixture["monoChat"]["id"], chain.mono.chat.as_str());
    assert_eq!(fixture["projects"], json!([P, P2]));
    assert_eq!(fixture["lead"]["id"], chain.lead.as_str());

    let (steered, sent) = seam(
        &world,
        &chain,
        chain.mono.chat.as_str(),
        "alarms.steer",
        json!({"alarm": episode, "text": "Your worker is idle with its ask."}),
        fixture["requestId"].as_str().unwrap(),
        &ok(),
    );
    assert_eq!(steered["ok"], true, "{steered}");
    let [(window, session, action, input)] = &sent[..] else {
        panic!("one window event: {sent:?}");
    };
    let without_prompt = |input: &Value| {
        let mut input = input.clone();
        input.as_object_mut().unwrap().remove("prompt");
        input
    };
    assert_eq!(
        (
            window.as_str(),
            session.as_str(),
            action.as_str(),
            without_prompt(input)
        ),
        (
            "mono-window",
            chain.mono.chat.as_str(),
            fixture["action"].as_str().unwrap(),
            without_prompt(&fixture["input"])
        ),
        "the window gets the lead's project"
    );
    assert!(input["prompt"]
        .as_str()
        .unwrap()
        .starts_with(&format!("<mono_steer alarm=\"{episode}\"")));

    let (nudged, sent) = seam(
        &world,
        &chain,
        chain.lead.as_str(),
        "alarms.nudge",
        json!({"alarm": episode}),
        "n-1",
        &ok(),
    );
    assert_eq!(nudged["ok"], true, "{nudged}");
    assert_eq!(
        sent[0].3.get("project"),
        None,
        "a session's window takes no project"
    );
}

const PLAN: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_plan","name":"ExitPlanMode","input":{"plan":"Split act.rs"}}]}}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","result":"done"}"#;

/// The worker's turn proposed a plan and ended; the plan waited 3
/// minutes: its dialog episode.
fn planned_worker(world: &mut World, chain: &Chain, claude: &ChildTap) -> String {
    align(world);
    turn(world, &chain.worker, "t1", true);
    claude.line(Pipe::Stdout, PLAN);
    claude.line(Pipe::Stdout, RESULT);
    turn(world, &chain.worker, "t1", false);
    world.advance(3 * MINUTE);
    let [(episode, _, _)] = <[_; 1]>::try_from(open_episodes(world)).unwrap();
    episode
}

fn dialog_kind(world: &World, session: &SessionId) -> Option<String> {
    world
        .conn()
        .query_row(
            "SELECT dialog_kind FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

// Defect (r2 m1, lead ruling): a plan dialog cannot be answered (it has
// no request id), its answer is not a message the agent's next turn
// reads, it is not logged as an answer, or the plan still blocks after it.
#[test]
fn the_lead_answers_its_workers_plan_with_a_new_message() {
    let mut world = World::new("alarm-plan");
    let chain = chain(&world, "alq");
    let claude = claude_tap(&world, &chain);
    let episode = planned_worker(&mut world, &chain, &claude);
    assert_eq!(dialog_kind(&world, &chain.worker).as_deref(), Some("plan"));
    let alarm = take(&world, &test_taps::outbox_key(&episode, 1));
    assert!(
        alarm.contains("Options: --allow | --deny --reason ..."),
        "{alarm}"
    );
    let lead = chain.lead.as_str();

    for (input, expected) in [
        (json!({"alarm": episode, "deny": true}), "invalid_input"),
        (
            json!({"alarm": episode, "allow": true, "reason": "fine"}),
            "invalid_input",
        ),
        (json!({"alarm": episode, "text": "go"}), "invalid_input"),
    ] {
        assert_eq!(
            code(call(&world, lead, "alarms.answer", input.clone())),
            expected,
            "{input}"
        );
    }
    let (denied, sent) = seam(
        &world,
        &chain,
        lead,
        "alarms.answer",
        json!({"alarm": episode, "deny": true, "reason": "It skips the tests."}),
        "p-1",
        &ok(),
    );
    assert_eq!(denied["result"]["acted"], "answer", "{denied}");
    let (window, from, action, to, when_busy, prompt) = steer_sent(&sent);
    assert_eq!(
        (
            window.as_str(),
            from.as_str(),
            action.as_str(),
            to.as_str(),
            when_busy.as_str(),
            prompt.as_str()
        ),
        (
            "lead-window",
            lead,
            "sessions.send",
            chain.worker.as_str(),
            "steer",
            "Your plan is not approved: It skips the tests. Revise it."
        ),
        "a new message from the lead to its worker"
    );
    let acted: (String, String) = world
        .conn()
        .query_row(
            "SELECT acted_how, acted_by FROM alarm_episodes WHERE id = ?1",
            [&episode],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(acted, ("answer".into(), format!("session:{lead}")));
    world.advance(PASS);
    assert_eq!(
        dialog_kind(&world, &chain.worker),
        None,
        "the plan waits no more"
    );
    assert_eq!(
        close_reason(&world, &episode).as_deref(),
        Some("progress"),
        "as when a permission is answered"
    );
    let (again, sent) = seam(
        &world,
        &chain,
        lead,
        "alarms.answer",
        json!({"alarm": episode, "allow": true}),
        "p-2",
        &ok(),
    );
    assert_eq!(again["code"], "alarm_closed", "{again}");
    assert_eq!(sent, Vec::new());
    assert_eq!(acts(&world, &episode).len(), 1);
}
