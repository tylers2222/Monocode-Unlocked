//! R5: the ask ledger, through `mono_message_submit`'s dispatch.

use std::collections::HashMap;

use serde_json::json;

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::asks;
use crate::mono_gate::Caller;
use crate::mono_messages::{Live, LiveSession, MessageId, RequestId, SessionId, Submitted};
use crate::mono_roster::MonoRole;

const P: &str = "/p";

/// A Giga, its Mono in `/p`, a lead the Mono started and that lead's two
/// workers: every level, as saved rows.
struct Tree {
    giga: Mono,
    mono: Mono,
    lead: SessionId,
    worker: SessionId,
    other: SessionId,
}

fn tree(world: &World) -> Tree {
    let giga = world.add_mono_in("r5-giga", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("r5-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "r5-lead", P);
    let worker = world.add_session("r5-worker", P, Some("r5-lead"));
    let other = world.add_session("r5-other", P, Some("r5-lead"));
    Tree {
        giga,
        mono,
        lead,
        worker,
        other,
    }
}

impl Tree {
    fn live(&self) -> [&SessionId; 3] {
        [&self.lead, &self.worker, &self.other]
    }
}

/// The ask row a call wrote; panics on anything else.
fn written(submitted: Submitted) -> MessageId {
    match submitted {
        Submitted::Accepted {
            mut messages,
            duplicate: false,
            ..
        } => messages.remove(0).message.id,
        other => panic!("not a new accept: {other:?}"),
    }
}

fn refusal(submitted: Submitted) -> (&'static str, String) {
    match submitted {
        Submitted::Failed { error } => (error.code, error.hint),
        other => panic!("not refused: {other:?}"),
    }
}

/// An ask row: state, due and levels.
fn ask_row(world: &World, id: &MessageId) -> (String, Option<i64>, i64, i64) {
    world
        .conn()
        .query_row(
            "SELECT state, due_active_ms, parent_level, child_level FROM mono_asks WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn count(world: &World, sql: &str) -> i64 {
    world.conn().query_row(sql, [], |row| row.get(0)).unwrap()
}

fn stored_active(world: &World) -> i64 {
    count(world, "SELECT active_ms FROM mono_clock WHERE id = 1")
}

fn ask(
    world: &World,
    tree: &Tree,
    from: &Caller,
    key: &str,
    to: &str,
    due: Option<u32>,
) -> Submitted {
    let mut input = json!({"message": format!("Do {key}"), "send_to": to});
    if let Some(due) = due {
        input["due_minutes"] = json!(due);
    }
    world.tool(from, key, "ask", input, &tree.live())
}

/// An ask is a key on a direct child, with a deadline in active time
/// unless a Giga asks. Defect: a parent asks one that is not its direct
/// child; a Giga's ask gets a deadline, or another's is not the stored
/// active time plus its budget; a repeat of the call writes a second ask; a
/// budget out of range is taken; or a kill loses an ask.
#[test]
fn an_ask_goes_only_to_a_direct_child_with_its_due_in_active_time() {
    let mut world = World::new("r5-ask");
    let tree = tree(&world);
    world.advance(20_000);
    let at = stored_active(&world);
    assert!(at > 0, "the clock ran");
    let giga_ask = written(ask(
        &world,
        &tree,
        &mono_caller(&tree.giga, "g1"),
        "g-ask",
        tree.mono.id.as_str(),
        Some(30),
    ));
    let mono_ask = written(ask(
        &world,
        &tree,
        &mono_caller(&tree.mono, "m1"),
        "m-ask",
        tree.lead.as_str(),
        None,
    ));
    let lead = session_caller(&tree.lead, P);
    let lead_ask = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask",
        tree.worker.as_str(),
        Some(30),
    ));
    assert_eq!(ask_row(&world, &giga_ask), ("open".into(), None, 1, 2));
    assert_eq!(
        ask_row(&world, &mono_ask),
        ("open".into(), Some(at + 60 * 60_000), 2, 3)
    );
    assert_eq!(
        ask_row(&world, &lead_ask),
        ("open".into(), Some(at + 30 * 60_000), 3, 4)
    );
    assert!(matches!(
        ask(
            &world,
            &tree,
            &lead,
            "l-ask",
            tree.worker.as_str(),
            Some(30)
        ),
        Submitted::Accepted {
            duplicate: true,
            ..
        }
    ));
    // Plan section 2: due_minutes is 10 to 480.
    for (due, taken) in [(9, false), (10, true), (480, true), (481, false)] {
        let key = format!("l-due-{due}");
        match ask(&world, &tree, &lead, &key, tree.worker.as_str(), Some(due)) {
            Submitted::Accepted { .. } => assert!(taken, "{due} taken"),
            refused => assert_eq!((refusal(refused).0, taken), ("invalid_input", false)),
        }
    }
    let stranger = world.start_lead(&tree.mono, "r5-stranger", P);
    let other_mono = world.add_mono_in("r5-mono-2", MonoRole::Mono, &[P]);
    for (from, to) in [
        (mono_caller(&tree.giga, "g1"), tree.lead.as_str()),
        (session_caller(&stranger, P), tree.worker.as_str()),
        (session_caller(&tree.worker, P), tree.lead.as_str()),
        (mono_caller(&other_mono, "o1"), tree.lead.as_str()),
    ] {
        let refused = refusal(ask(&world, &tree, &from, "bad-ask", to, None));
        assert_eq!(
            refused.0, "not_direct_child",
            "{:?} -> {to}",
            from.session_id
        );
    }
    world.down(60_000);
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_asks WHERE state = 'open'"
        ),
        5,
        "asks outlive a kill"
    );
}

/// Review r1 m2. Defect: an ask under the request id of a plain send with
/// the same fields answers that send as its own accepted duplicate, so the
/// caller holds an ask id with no ask.
#[test]
fn an_ask_reusing_a_sends_request_id_is_refused() {
    let world = World::new("r5-reuse");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let send = json!({"message": "Do x", "send_to": tree.worker.as_str(), "wanting_reply": false, "intent": "new"});
    written(world.tool(&lead, "dup", "send_message", send, &tree.live()));
    let reused = refusal(world.tool(
        &lead,
        "dup",
        "ask",
        json!({"message": "Do x", "send_to": tree.worker.as_str()}),
        &tree.live(),
    ));
    assert_eq!(reused.0, "request_id_reused");
    assert_eq!(count(&world, "SELECT count(*) FROM mono_asks"), 0);
}

/// Review r1 m3. Defect: a caller or target with no saved row is told
/// `not_direct_child` instead of what the send gate answers it.
#[test]
fn an_ask_from_or_to_no_saved_row_gets_the_send_gates_answer() {
    let world = World::new("r5-unknown");
    let tree = tree(&world);
    let ghost = SessionId::parse("r5-ghost").unwrap();
    let mut live = tree.live().to_vec();
    live.push(&ghost);
    let input = |to: &str| json!({"message": "Do x", "send_to": to});
    let from_ghost = refusal(world.tool(
        &session_caller(&ghost, P),
        "g-ask",
        "ask",
        input(tree.worker.as_str()),
        &live,
    ));
    assert_eq!(from_ghost.0, "unknown_caller");
    let to_nobody = refusal(world.tool(
        &session_caller(&tree.lead, P),
        "n-ask",
        "ask",
        input("r5-nobody"),
        &live,
    ));
    assert_eq!(to_nobody.0, "unknown_target");
    // A listed session the send gate lets a Mono message, with no saved
    // row: no child, and no row is kept.
    let to_ghost = refusal(world.tool(
        &mono_caller(&tree.mono, "m1"),
        "l-ask",
        "ask",
        input(ghost.as_str()),
        &live,
    ));
    assert_eq!(to_ghost.0, "not_direct_child");
    assert_eq!(count(&world, "SELECT count(*) FROM mono_asks"), 0);
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE request_id = 'l-ask'"
        ),
        0
    );
}

/// Review r1 m1. Defect: without the transaction a failed ask insert keeps
/// the carrier row, so a retry under the same id gets a duplicate carrier
/// and no ask is ever written.
#[test]
fn a_failed_ask_insert_leaves_no_carrier_row() {
    let world = World::new("r5-fault");
    let tree = tree(&world);
    world
        .conn()
        .execute_batch(
            "CREATE TEMP TRIGGER r5_fault BEFORE INSERT ON mono_asks
             BEGIN SELECT RAISE(ABORT, 'r5 fault'); END;",
        )
        .unwrap();
    let sessions = tree.live().map(|session| LiveSession {
        session_id: session.as_str().into(),
        steerable: true,
        own_project: true,
    });
    let running = HashMap::new();
    let live = Live {
        sessions: &sessions,
        running: &running,
        steerable_harnesses: &[],
    };
    let got = asks::ask(
        &world.conn(),
        &session_caller(&tree.lead, P),
        &RequestId::parse("fault").unwrap(),
        Ok(
            serde_json::from_value(json!({"message": "Do y", "send_to": tree.worker.as_str()}))
                .unwrap(),
        ),
        &live,
        world.now(),
    );
    assert!(got.is_err(), "{got:?}");
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE request_id = 'fault'"
        ),
        0
    );
}
