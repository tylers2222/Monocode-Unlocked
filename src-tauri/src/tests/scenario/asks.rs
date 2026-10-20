//! R5: the ask and ack ledger, through `mono_message_submit`'s dispatch.

use std::collections::HashMap;
use std::sync::{Arc, Barrier};

use rusqlite::{params, Connection};
use serde_json::json;

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::ask_gate::AckRefused;
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

fn ack_refused(reason: AckRefused) -> (&'static str, String) {
    ("ack_refused", reason.hint().into())
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

fn ack(world: &World, tree: &Tree, from: &Caller, key: &str, ask: &MessageId) -> Submitted {
    world.tool(
        from,
        key,
        "ack",
        json!({"ask_id": ask.as_str(), "message": "Done"}),
        &tree.live(),
    )
}

/// Where the ack row `id` went: its Mono or its session.
fn ack_went_to(world: &World, id: &MessageId) -> (Option<String>, Option<String>) {
    world
        .conn()
        .query_row(
            "SELECT to_mono_id, to_session_id FROM mono_messages
              WHERE id = (SELECT message_id FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'ack')",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
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

/// Review r2 m1. Defect: a retried ask that was rejected (here an unknown
/// target) is told `request_id_reused` instead of its first refusal, as if
/// its input had changed.
#[test]
fn a_retried_rejected_ask_gets_its_first_refusal() {
    let world = World::new("r5-rejected-retry");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let input = json!({"message": "Do x", "send_to": "r5-nobody"});
    let answer = |submitted| match submitted {
        Submitted::Failed { error } => (error.code, error.duplicate),
        other => panic!("not refused: {other:?}"),
    };
    let first = answer(world.tool(&lead, "lost", "ask", input.clone(), &tree.live()));
    let again = answer(world.tool(&lead, "lost", "ask", input, &tree.live()));
    assert_eq!(
        (first, again),
        (("unknown_target", false), ("unknown_target", true))
    );
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

/// S1 "Each ack() reaches only the direct parent, and the ask is ticked off
/// in the database". Defect: an ack goes to someone other than the asker, or
/// an acked ask stays open or has no ack event.
#[test]
fn s1_each_ack_reaches_only_its_direct_parent_and_ticks_the_ask() {
    let world = World::new("r5-s1");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let giga_ask = written(ask(
        &world,
        &tree,
        &mono_caller(&tree.giga, "g1"),
        "g-ask",
        tree.mono.id.as_str(),
        None,
    ));
    let mono_ask = written(ask(
        &world,
        &tree,
        &mono_caller(&tree.mono, "m1"),
        "m-ask",
        tree.lead.as_str(),
        None,
    ));
    let lead_ask = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    written(ack(
        &world,
        &tree,
        &session_caller(&tree.worker, P),
        "w-ack",
        &lead_ask,
    ));
    written(ack(&world, &tree, &lead, "l-ack", &mono_ask));
    written(ack(
        &world,
        &tree,
        &mono_caller(&tree.mono, "m2"),
        "m-ack",
        &giga_ask,
    ));
    for (ask, parent) in [
        (&lead_ask, (None, Some(tree.lead.as_str().to_string()))),
        (&mono_ask, (Some(tree.mono.id.as_str().to_string()), None)),
        (&giga_ask, (Some(tree.giga.id.as_str().to_string()), None)),
    ] {
        assert_eq!(ask_row(&world, ask).0, "acked");
        assert_eq!(ack_went_to(&world, ask), parent);
    }
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_ask_events WHERE kind = 'ack'"
        ),
        3
    );
    // Each child wrote its ack and its own ask down, nothing else.
    for (child, sent) in [
        (tree.worker.as_str(), 1),
        (tree.lead.as_str(), 2),
        (tree.mono.chat.as_str(), 2),
    ] {
        let rows: i64 = world
            .conn()
            .query_row(
                "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1 AND tool = 'send_message'",
                [child],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, sent, "{child}");
    }
}

/// S1 "A second ack() on the same ask is refused". Defect: a second ack of
/// an acked ask gets through or writes a row; or the ack's own repeat (same
/// request id) is refused instead of answered.
#[test]
fn s1_a_second_ack_of_one_ask_is_refused() {
    let world = World::new("r5-s1-again");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let worker = session_caller(&tree.worker, P);
    let id = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    written(ack(&world, &tree, &worker, "w-ack", &id));
    let rows = count(&world, "SELECT count(*) FROM mono_messages");
    assert_eq!(
        refusal(ack(&world, &tree, &worker, "w-ack-2", &id)),
        ack_refused(AckRefused::AlreadyAcked)
    );
    assert!(matches!(
        ack(&world, &tree, &worker, "w-ack", &id),
        Submitted::Accepted {
            duplicate: true,
            ..
        }
    ));
    assert_eq!(count(&world, "SELECT count(*) FROM mono_messages"), rows);
    assert_eq!(count(&world, "SELECT count(*) FROM mono_ask_events"), 1);
}

/// S2 "An ack for someone else's ask". Defect: a session acks an ask sent
/// to another (a sibling worker), or an unknown id.
#[test]
fn s2_an_ack_of_someone_elses_ask_is_refused() {
    let world = World::new("r5-s2-other");
    let tree = tree(&world);
    let id = written(ask(
        &world,
        &tree,
        &session_caller(&tree.lead, P),
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    assert_eq!(
        refusal(ack(
            &world,
            &tree,
            &session_caller(&tree.other, P),
            "o-ack",
            &id
        )),
        ack_refused(AckRefused::NotYours)
    );
    let unknown = MessageId::parse("m_aaaaaaaaaaaa").unwrap();
    assert_eq!(
        refusal(ack(
            &world,
            &tree,
            &session_caller(&tree.worker, P),
            "w-ack",
            &unknown
        )),
        ack_refused(AckRefused::NoOpenAsk)
    );
    assert_eq!(ask_row(&world, &id).0, "open");
    assert_eq!(count(&world, "SELECT count(*) FROM mono_ask_events"), 0);
}

/// S2 "Two acks at the same instant: exactly one gets through". Two threads,
/// each on its own connection to the file, ack one ask at once under two
/// request ids. Defect: both get through, or two ack rows are written.
#[test]
fn s2_two_acks_at_the_same_instant_one_gets_through() {
    let world = World::new("r5-s2-race");
    let tree = tree(&world);
    let id = written(ask(
        &world,
        &tree,
        &session_caller(&tree.lead, P),
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    let barrier = Arc::new(Barrier::new(2));
    let outcomes: Vec<Submitted> = ["w-ack-a", "w-ack-b"]
        .map(|key| {
            let (path, barrier, worker, lead, id) = (
                world.path().to_path_buf(),
                barrier.clone(),
                tree.worker.clone(),
                tree.lead.clone(),
                id.clone(),
            );
            let now = world.now();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
                    .unwrap();
                let sessions = [&lead, &worker].map(|session| LiveSession {
                    session_id: session.as_str().into(),
                    steerable: true,
                    own_project: true,
                });
                let live = Live {
                    sessions: &sessions,
                    running: &HashMap::new(),
                    steerable_harnesses: &[],
                };
                barrier.wait();
                asks::ack(
                    &conn,
                    &session_caller(&worker, P),
                    &RequestId::parse(key).unwrap(),
                    Ok(
                        serde_json::from_value(json!({"ask_id": id.as_str(), "message": "Done"}))
                            .unwrap(),
                    ),
                    &live,
                    now,
                )
                .unwrap()
            })
        })
        .map(|thread| thread.join().unwrap())
        .into_iter()
        .collect();
    let through = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome,
                Submitted::Accepted {
                    duplicate: false,
                    ..
                }
            )
        })
        .count();
    assert_eq!(through, 1, "{outcomes:?}");
    assert!(outcomes.into_iter().any(
        |outcome| matches!(outcome, Submitted::Failed { ref error } if error.code == "ack_refused")
    ));
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_ask_events WHERE kind = 'ack'"
        ),
        1
    );
    let carriers: i64 = world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1",
            params![tree.worker.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(carriers, 1, "one ack row to the lead");
}

/// S4 "The worker acks. Then the parent says "redo it": that is a new ask
/// with a new key". Defect: the redo reopens or reuses the spent key, or
/// cannot be acked.
#[test]
fn s4_a_redo_after_an_ack_is_a_new_ask_with_a_new_key() {
    let world = World::new("r5-s4-redo");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let worker = session_caller(&tree.worker, P);
    let first = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    written(ack(&world, &tree, &worker, "w-ack", &first));
    let redo = written(ask(
        &world,
        &tree,
        &lead,
        "l-redo",
        tree.worker.as_str(),
        None,
    ));
    assert_ne!(redo, first);
    assert_eq!(ask_row(&world, &first).0, "acked");
    assert_eq!(ask_row(&world, &redo).0, "open");
    written(ack(&world, &tree, &worker, "w-ack-redo", &redo));
    assert_eq!(ask_row(&world, &redo).0, "acked");
}

/// The ack, its event and its row to the parent are one transaction.
/// Defect: an ack whose row to the parent is refused (here the 6-a-window
/// rate limit) still spends the key or leaves an event.
#[test]
fn a_refused_ack_row_leaves_the_ask_open() {
    let world = World::new("r5-ack-atomic");
    let tree = tree(&world);
    let worker = session_caller(&tree.worker, P);
    let id = written(ask(
        &world,
        &tree,
        &session_caller(&tree.lead, P),
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    for n in 0..6 {
        let input = json!({"message": "Update", "send_to": tree.lead.as_str(), "wanting_reply": false, "intent": "new"});
        written(world.tool(
            &worker,
            &format!("note-{n}"),
            "send_message",
            input,
            &tree.live(),
        ));
    }
    assert_eq!(
        refusal(ack(&world, &tree, &worker, "w-ack", &id)).0,
        "rate_limited"
    );
    assert_eq!(ask_row(&world, &id).0, "open");
    assert_eq!(count(&world, "SELECT count(*) FROM mono_ask_events"), 0);
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE gate_result = 'rate_limited'"
        ),
        0
    );
}
