//! S1 (R4): levels come from the database, not from the agent.

use serde_json::{json, Value};

use super::world::World;
use crate::mono_gate::SessionLevel;
use crate::mono_messages::{self, RequestId, SessionId, Submitted, Tool};
use crate::mono_roster::MonoRole;

/// A session caller as the web app sends it, plus `extra` fields.
fn session_caller(id: &SessionId, extra: Value) -> Value {
    let mut caller = json!({
        "kind": "session", "sessionId": id.as_str(), "turnId": "t1",
        "access": "started", "cwd": "/p",
    });
    caller
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    caller
}

fn report(to: &SessionId) -> Value {
    json!({"message": "Tests pass", "send_to": to.as_str(), "wanting_reply": false, "intent": "new"})
}

/// The code of a refused call, and whether it wrote a row.
fn refusal(submitted: Submitted) -> (&'static str, bool) {
    match submitted {
        Submitted::Failed { error } => (error.code, error.message_id.is_some()),
        other => panic!("not refused: {other:?}"),
    }
}

fn rows(world: &World) -> i64 {
    world
        .conn()
        .query_row("SELECT count(*) FROM mono_messages", [], |row| row.get(0))
        .unwrap()
}

/// Defect: a session's level comes from what its call claims (a lead id the
/// agent or window sends), so a worker escapes its lead's scope; a session
/// with no saved row gets a level anyway; or a stored session's level is not
/// the one its `lead_session_id` says.
#[test]
fn s1_levels_come_from_the_database_not_from_the_agent() {
    let mut world = World::new("s1");
    world.add_mono("s1-giga", MonoRole::Giga);
    let lead_a = world.add_session("s1-lead-a", "/p", None);
    let lead_b = world.add_session("s1-lead-b", "/p", None);
    let worker = world.add_session("s1-worker", "/p", Some("s1-lead-a"));
    let live = [&lead_a, &lead_b, &worker];

    // A worker that claims another lead in its call is refused at the
    // command boundary: the call has no lead field.
    let claimed = world.session_call(
        session_caller(&worker, json!({"leadSessionId": "s1-lead-b"})),
        "claim",
        Tool::SendMessage,
        report(&lead_b),
        &live,
    );
    assert!(
        claimed
            .as_ref()
            .is_err_and(|error| error.contains("unknown field `leadSessionId`")),
        "{claimed:?}"
    );

    // Without the claim it is judged by its stored row: a worker of lead-a,
    // which may message only lead-a.
    let caller = session_caller(&worker, json!({}));
    let to_b = world.session_call(
        caller.clone(),
        "to-b",
        Tool::SendMessage,
        report(&lead_b),
        &live,
    );
    assert_eq!(refusal(to_b.unwrap()), ("worker_scope", true));
    let to_a = world.session_call(
        caller.clone(),
        "to-a",
        Tool::SendMessage,
        report(&lead_a),
        &live,
    );
    assert!(matches!(
        to_a.unwrap(),
        Submitted::Accepted {
            duplicate: false,
            ..
        }
    ));

    // The stored row still decides after a restart.
    world.down(60_000);
    let again = world.session_call(caller, "to-b-2", Tool::SendMessage, report(&lead_b), &live);
    assert_eq!(refusal(again.unwrap()), ("worker_scope", true));

    // A session MonoCode has not saved is refused with no row, on the
    // message path and the app action path.
    let ghost = SessionId::parse("s1-ghost").unwrap();
    let before = rows(&world);
    let unsaved = world.session_call(
        session_caller(&ghost, json!({})),
        "ghost",
        Tool::SendMessage,
        report(&lead_a),
        &live,
    );
    assert_eq!(refusal(unsaved.unwrap()), ("unknown_caller", false));
    let caller =
        serde_json::from_value::<mono_messages::CallerArg>(session_caller(&ghost, json!({})))
            .unwrap()
            .parse()
            .unwrap();
    let start = mono_messages::check_action(
        &world.conn(),
        &caller,
        "sessions.start",
        None,
        None,
        &RequestId::parse("ghost-start").unwrap(),
        &json!({"prompt": "Help", "harness": "codex", "model": "codex:default"}),
        world.now(),
    )
    .unwrap();
    assert!(
        matches!(&start, mono_messages::ActionChecked::Failed { error }
            if error.code == "unknown_caller" && error.message_id.is_none()),
        "{start:?}"
    );
    assert_eq!(rows(&world), before, "an unknown caller writes no row");

    every_stored_session_has_the_level_its_row_says(&world);
}

/// Every `sessions` row (a real copy's too) gets the level its
/// `lead_session_id` says. Prints the count per level.
fn every_stored_session_has_the_level_its_row_says(world: &World) {
    let conn = world.conn();
    let stored: Vec<(String, Option<String>)> = conn
        .prepare("SELECT id, lead_session_id FROM sessions")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let (mut leads, mut workers) = (0, 0);
    for (id, lead) in &stored {
        let level = mono_messages::session_level(&conn, &SessionId::parse(id).unwrap()).unwrap();
        match (level, lead) {
            (Some(SessionLevel::Lead), None) => leads += 1,
            (Some(SessionLevel::Worker { lead: read }), Some(lead)) if read.as_str() == lead => {
                workers += 1
            }
            (level, lead) => panic!("{id}: stored lead {lead:?}, read {level:?}"),
        }
    }
    eprintln!(
        "scenario s1: {} sessions: {leads} leads, {workers} workers",
        stored.len()
    );
}
