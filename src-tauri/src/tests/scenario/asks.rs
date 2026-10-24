//! R5 and R5c: the ask and ack ledger, through `mono_message_submit`'s dispatch.

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
use crate::mono_roster::{self, MonoRole};

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
pub(super) fn written(submitted: Submitted) -> MessageId {
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

/// `f` with the tree's sessions as the live window, for a direct store call.
fn with_live<T>(tree: &Tree, f: impl FnOnce(&Live) -> T) -> T {
    let sessions = tree.live().map(|session| LiveSession {
        session_id: session.as_str().into(),
        steerable: true,
        own_project: true,
    });
    let running = HashMap::new();
    f(&Live {
        sessions: &sessions,
        running: &running,
        steerable_harnesses: &[],
    })
}

/// Every insert into `table` fails from now on.
fn fail_inserts(world: &World, table: &str) {
    world
        .conn()
        .execute_batch(&format!(
            "CREATE TEMP TRIGGER fault_{table} BEFORE INSERT ON {table}
             BEGIN SELECT RAISE(ABORT, 'forced fault'); END;"
        ))
        .unwrap();
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
    fail_inserts(&world, "mono_asks");
    let got = with_live(&tree, |live| {
        asks::ask(
            &world.conn(),
            &session_caller(&tree.lead, P),
            &RequestId::parse("fault").unwrap(),
            Ok(
                serde_json::from_value(json!({"message": "Do y", "send_to": tree.worker.as_str()}))
                    .unwrap(),
            ),
            live,
            world.now(),
        )
    });
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
/// an acked ask stays open or has no ack event, or its event loses the
/// active time it happened at.
#[test]
fn s1_each_ack_reaches_only_its_direct_parent_and_ticks_the_ask() {
    let mut world = World::new("r5-s1");
    let tree = tree(&world);
    world.advance(20_000);
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
    let active = stored_active(&world);
    assert!(active > 0, "the clock ran");
    assert_eq!(
        count(
            &world,
            &format!("SELECT count(*) FROM mono_ask_events WHERE kind = 'ack' AND at_active_ms = {active}")
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

/// Review r1 m3 and m5. Defect: a refused ack touches the ask; its row is
/// rolled back, so the id it names finds nothing; or a retry under its
/// request id is decided again instead of getting its first refusal, as
/// every other tool's retry does.
#[test]
fn a_refused_ack_keeps_its_row_and_a_retry_gets_its_first_refusal() {
    let mut world = World::new("r5-ack-refused");
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
    let answer = |submitted| match submitted {
        Submitted::Failed { error } => (error.code, error.duplicate, error.message_id),
        other => panic!("not refused: {other:?}"),
    };
    let (code, duplicate, named) = answer(ack(&world, &tree, &worker, "w-ack", &id));
    assert_eq!((code, duplicate), ("rate_limited", false));
    let rejected: String = world
        .conn()
        .query_row(
            "SELECT id FROM mono_messages WHERE request_id = 'w-ack' AND gate_result = 'rate_limited'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(named.map(|id| id.as_str().to_string()), Some(rejected));
    world.jump_wall(700_000);
    let (code, duplicate, _) = answer(ack(&world, &tree, &worker, "w-ack", &id));
    assert_eq!((code, duplicate), ("rate_limited", true));
    assert_eq!(ask_row(&world, &id).0, "open");
    assert_eq!(count(&world, "SELECT count(*) FROM mono_ask_events"), 0);
}

/// Review r2 m1. Defect: an ack that is not the caller's to give is sent
/// first, so the send gate answers it (a parent acking its own ask is told
/// `self_send`) and its refused row is kept under the request id.
#[test]
fn a_parent_that_acks_its_own_ask_is_told_not_yours_and_writes_no_row() {
    let world = World::new("r5-ack-own");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let id = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    assert_eq!(
        refusal(ack(&world, &tree, &lead, "l-own", &id)),
        ack_refused(AckRefused::NotYours)
    );
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE request_id = 'l-own'"
        ),
        0
    );
    assert_eq!(ask_row(&world, &id).0, "open");
}

/// Review r2 m1. Defect: a rate-limited child that acks an ask it already
/// acked is told `rate_limited` by the send gate instead of `ack_refused`
/// "already acked".
#[test]
fn a_rate_limited_ack_of_an_acked_ask_is_told_already_acked() {
    let world = World::new("r5-ack-acked-limited");
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
    written(ack(&world, &tree, &worker, "w-ack", &id));
    for n in 0..5 {
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
        refusal(ack(&world, &tree, &worker, "w-ack-2", &id)),
        ack_refused(AckRefused::AlreadyAcked)
    );
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE request_id = 'w-ack-2'"
        ),
        0
    );
}

/// Review r1 m4. Defect: the event is written apart from the guarded
/// `UPDATE` and the row to the parent, so a failed event insert leaves an
/// acked ask and a delivered ack with no event.
#[test]
fn a_failed_ack_event_insert_leaves_the_ask_open_and_no_row() {
    let world = World::new("r5-ack-fault");
    let tree = tree(&world);
    let id = written(ask(
        &world,
        &tree,
        &session_caller(&tree.lead, P),
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    fail_inserts(&world, "mono_ask_events");
    let got = with_live(&tree, |live| {
        asks::ack(
            &world.conn(),
            &session_caller(&tree.worker, P),
            &RequestId::parse("fault").unwrap(),
            Ok(serde_json::from_value(json!({"ask_id": id.as_str(), "message": "Done"})).unwrap()),
            live,
            world.now(),
        )
    });
    assert!(got.is_err(), "{got:?}");
    assert_eq!(ask_row(&world, &id).0, "open");
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM mono_messages WHERE request_id = 'fault'"
        ),
        0
    );
}

/// Review r1 m1. Defect: an ack under a request id that a plain send, or the
/// ack of another ask, already used is answered with that call's row as a
/// repeat, and its own ask stays open.
#[test]
fn an_ack_reusing_another_calls_request_id_is_refused() {
    let world = World::new("r5-ack-reuse");
    let tree = tree(&world);
    let lead = session_caller(&tree.lead, P);
    let worker = session_caller(&tree.worker, P);
    let a = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask-a",
        tree.worker.as_str(),
        None,
    ));
    let b = written(ask(
        &world,
        &tree,
        &lead,
        "l-ask-b",
        tree.worker.as_str(),
        None,
    ));
    // Same fields as the ack's row: same parent, same text.
    let send = json!({"message": "Done", "send_to": tree.lead.as_str(), "wanting_reply": false, "intent": "new"});
    written(world.tool(&worker, "sent", "send_message", send, &tree.live()));
    assert_eq!(
        refusal(ack(&world, &tree, &worker, "sent", &a)).0,
        "request_id_reused"
    );
    written(ack(&world, &tree, &worker, "w-ack", &a));
    assert_eq!(
        refusal(ack(&world, &tree, &worker, "w-ack", &b)).0,
        "request_id_reused"
    );
    assert_eq!(
        (ask_row(&world, &a).0, ask_row(&world, &b).0),
        ("acked".to_string(), "open".to_string())
    );
}

/// Review r1 m2. Defect: an ack from a caller with no saved row (a session
/// the window lists, a Mono whose row is gone) is told `ack_refused` "not
/// yours" instead of what the send gate answers it.
#[test]
fn an_ack_from_no_saved_row_gets_the_send_gates_answer() {
    let world = World::new("r5-ack-rowless");
    let tree = tree(&world);
    let lead_ask = written(ask(
        &world,
        &tree,
        &session_caller(&tree.lead, P),
        "l-ask",
        tree.worker.as_str(),
        None,
    ));
    let giga_ask = written(ask(
        &world,
        &tree,
        &mono_caller(&tree.giga, "g1"),
        "g-ask",
        tree.mono.id.as_str(),
        None,
    ));
    let ghost = SessionId::parse("r5-ghost").unwrap();
    let mut live = tree.live().to_vec();
    live.push(&ghost);
    let input = json!({"ask_id": lead_ask.as_str(), "message": "Done"});
    let got = world.tool(&session_caller(&ghost, P), "g-ack", "ack", input, &live);
    assert_eq!(refusal(got).0, "unknown_caller");
    world
        .conn()
        .execute("DELETE FROM monos WHERE id = ?1", [tree.mono.id.as_str()])
        .unwrap();
    let got = ack(
        &world,
        &tree,
        &mono_caller(&tree.mono, "m2"),
        "m-ack",
        &giga_ask,
    );
    assert_eq!(refusal(got).0, "not_a_mono");
    assert_eq!(
        (ask_row(&world, &lead_ask).0, ask_row(&world, &giga_ask).0),
        ("open".to_string(), "open".to_string())
    );
}

/// S1.6 "A Mono resets its chat mid-ask and still acks; its lead's ack still
/// reaches it" (R5c). A Giga asks the Mono, the Mono asks its lead, then the
/// Mono resets its chat through the app's own path. Defect: the Mono's ack
/// from its new chat is refused because the ask names the dead chat; the
/// dead chat can still ack (the send gate, not the ask rules, binds a Mono's
/// ack to a live chat); the lead's ack goes to the dead chat or never
/// reaches the new one; or either ask is left open.
#[test]
fn s1_6_a_mono_resets_its_chat_mid_ask_and_both_acks_reach_their_parent() {
    let mut world = World::new("r5c-s1-6");
    let tree = tree(&world);
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
    let reset = world.reset_mono_chat(&tree.mono, "r5c-mono-chat-2");
    let mut live = tree.live().to_vec();
    live.push(&reset.chat);
    let done = |ask: &MessageId| json!({"ask_id": ask.as_str(), "message": "Done"});
    let dead = world.tool(
        &mono_caller(&tree.mono, "m2"),
        "dead-ack",
        "ack",
        done(&giga_ask),
        &live,
    );
    assert_eq!(refusal(dead).0, "unknown_token", "the dead chat cannot ack");
    assert_eq!(
        ask_row(&world, &giga_ask).0,
        "open",
        "a refused ack spends no key"
    );
    let mono_ack = world.tool(
        &mono_caller(&reset, "m3"),
        "m-ack",
        "ack",
        done(&giga_ask),
        &live,
    );
    written(mono_ack);
    assert_eq!(
        ask_row(&world, &giga_ask).0,
        "acked",
        "the Mono acked from its new chat"
    );
    assert_eq!(
        ack_went_to(&world, &giga_ask),
        (Some(tree.giga.id.as_str().to_string()), None)
    );
    let lead_ack = written(world.tool(
        &session_caller(&tree.lead, P),
        "l-ack",
        "ack",
        done(&mono_ask),
        &live,
    ));
    assert_eq!(ask_row(&world, &mono_ask).0, "acked");
    assert_eq!(
        ack_went_to(&world, &mono_ask),
        (Some(tree.mono.id.as_str().to_string()), None)
    );
    let handed = world.start_turn(&reset.chat, "m4");
    assert!(
        handed.iter().any(|row| row.message.id == lead_ack),
        "the lead's ack reaches the Mono's new chat: {handed:?}"
    );
}

/// The chat an actor column names, as R5 stored it: a Mono's chat at the
/// time, a session itself.
fn chat_of(column: &str) -> String {
    format!(
        "CASE WHEN {column} GLOB 'mono:*'
           THEN (SELECT json_extract(data_json, '$.sessionId') FROM monos WHERE id = substr({column}, 6))
           ELSE substr({column}, 9) END"
    )
}

/// Rewrites `mono_asks` as tyler/main stored it before R5c: parties by chat,
/// `dropped` allowed, here on ask `dropped`.
fn store_as_r5(world: &World, dropped: &MessageId) {
    world
        .conn()
        .execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             CREATE TABLE mono_asks_r5 (
               id                  TEXT PRIMARY KEY,
               parent_session      TEXT NOT NULL,
               child_session       TEXT NOT NULL CHECK (child_session <> parent_session),
               parent_level        INTEGER NOT NULL CHECK (parent_level BETWEEN 1 AND 3),
               child_level         INTEGER NOT NULL CHECK (child_level = parent_level + 1),
               state               TEXT NOT NULL CHECK (state IN ('open', 'acked', 'dropped')),
               due_active_ms       INTEGER CHECK ((due_active_ms IS NULL) = (parent_level = 1)),
               paused_at_active_ms INTEGER,
               stage               INTEGER NOT NULL DEFAULT 0 CHECK (stage BETWEEN 0 AND 3),
               next_due_active_ms  INTEGER,
               created_wall_ms     INTEGER NOT NULL
             );
             INSERT INTO mono_asks_r5 SELECT id, {}, {}, parent_level, child_level, state,
               due_active_ms, paused_at_active_ms, stage, next_due_active_ms, created_wall_ms
               FROM mono_asks;
             DROP TABLE mono_asks;
             ALTER TABLE mono_asks_r5 RENAME TO mono_asks;
             CREATE INDEX mono_asks_child ON mono_asks (child_session) WHERE state = 'open';
             CREATE INDEX mono_asks_next_due ON mono_asks (next_due_active_ms) WHERE state = 'open';
             UPDATE mono_asks SET state = 'dropped' WHERE id = '{}';
             PRAGMA foreign_keys = ON;",
            chat_of("parent_actor"),
            chat_of("child_actor"),
            dropped.as_str()
        ))
        .unwrap();
}

/// One ask as `asks_snapshot` lists it: id, parent, child and state.
type AskRow = (String, String, String, String);

fn ask_actors(id: &MessageId, parent: &str, child: &str, state: &str) -> AskRow {
    (
        id.as_str().into(),
        parent.into(),
        child.into(),
        state.into(),
    )
}

/// An ask's columns the migration carries as they are: id, levels, due,
/// paused, stage, next due and created.
type Carried = (
    String,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    i64,
);

/// The schema of the ask tables, every ask by actor, and every ask's
/// carried columns.
fn asks_snapshot(world: &World) -> (Vec<Option<String>>, Vec<AskRow>, Vec<Carried>) {
    let conn = world.conn();
    let schema = conn
        .prepare("SELECT sql FROM sqlite_master WHERE tbl_name LIKE 'mono_ask%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let rows = conn
        .prepare("SELECT id, parent_actor, child_actor, state FROM mono_asks ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let carried = conn
        .prepare(
            "SELECT id, parent_level, child_level, due_active_ms, paused_at_active_ms, stage,
                    next_due_active_ms, created_wall_ms FROM mono_asks ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    (schema, rows, carried)
}

/// R5c migration. A database from tyler/main before R5c: its asks name
/// chats, one was `dropped`, one is acked with its event, one has its
/// scheduler columns set, the Mono reset its chat after asking, a Giga that
/// asked was deleted and so was a Mono that was asked. Defect: the restart
/// leaves a party as a chat, maps a Mono to anything but its id (its dead
/// chat, or the other party), keeps `dropped`, loses an ask, an ack event or
/// a carried column, fails on a deleted Mono's ask on either side or keeps
/// it and its event, changes anything on a second restart, or a migrated ask
/// cannot be acked.
#[test]
fn r5c_a_restart_moves_asks_from_chats_to_actors_once() {
    let mut world = World::new("r5c-migrate");
    let tree = tree(&world);
    let gone = world.add_mono_in("r5c-gone", MonoRole::Giga, &[P]);
    let lost = world.add_mono_in("r5c-lost", MonoRole::Mono, &[P]);
    let (giga, mono) = (tree.giga.id.as_str(), tree.mono.id.as_str());
    let lead = session_caller(&tree.lead, P);
    let asked =
        |from: &Caller, key: &str, to: &str| written(ask(&world, &tree, from, key, to, None));
    let acked = |from: &Caller, key: &str, ask: &MessageId| {
        written(ack(&world, &tree, from, key, ask));
    };
    let giga_ask = asked(&mono_caller(&tree.giga, "g1"), "g-ask", mono);
    let gone_ask = asked(&mono_caller(&gone, "x1"), "x-ask", mono);
    let lost_ask = asked(&mono_caller(&tree.giga, "g2"), "y-ask", lost.id.as_str());
    let mono_ask = asked(&mono_caller(&tree.mono, "m1"), "m-ask", tree.lead.as_str());
    let lead_ask = asked(&lead, "l-ask", tree.worker.as_str());
    let other_ask = asked(&lead, "o-ask", tree.other.as_str());
    acked(&session_caller(&tree.worker, P), "w-ack", &lead_ask);
    acked(&mono_caller(&tree.mono, "x2"), "x-ack", &gone_ask);
    acked(&mono_caller(&lost, "y1"), "y-ack", &lost_ask);
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET stage = 2, next_due_active_ms = 4321, paused_at_active_ms = 1234
              WHERE id = ?1",
            [mono_ask.as_str()],
        )
        .unwrap();
    let carried = asks_snapshot(&world).2;
    store_as_r5(&world, &other_ask);
    for deleted in [&gone, &lost] {
        assert!(mono_roster::delete(&world.conn(), deleted.id.as_str()).unwrap());
    }
    let reset = world.reset_mono_chat(&tree.mono, "r5c-mono-chat-2");
    world.kill();
    world.restart();
    let migrated = asks_snapshot(&world);
    let gone_ids = [gone_ask.as_str(), lost_ask.as_str()];
    let carried: Vec<_> = carried
        .into_iter()
        .filter(|row| !gone_ids.contains(&row.0.as_str()))
        .collect();
    assert_eq!(
        migrated.2, carried,
        "every carried column survives as it was"
    );
    let (giga, mono) = (format!("mono:{giga}"), format!("mono:{mono}"));
    let mut expected = vec![
        ask_actors(&giga_ask, &giga, &mono, "open"),
        ask_actors(&mono_ask, &mono, "session:r5-lead", "open"),
        ask_actors(&lead_ask, "session:r5-lead", "session:r5-worker", "acked"),
        ask_actors(&other_ask, "session:r5-lead", "session:r5-other", "open"),
    ];
    expected.sort();
    assert_eq!(
        migrated.1, expected,
        "every ask a party can still ack, by actor"
    );
    assert!(
        migrated
            .0
            .iter()
            .flatten()
            .all(|sql| !sql.contains("_session")),
        "no chat column or index is left: {:?}",
        migrated.0
    );
    let events = "SELECT count(*), max(ask_id) FROM mono_ask_events";
    let events: (i64, String) = world
        .conn()
        .query_row(events, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    let kept = (1, lead_ask.as_str().to_string());
    assert_eq!(
        events, kept,
        "each deleted Mono's ask goes with its event, no other"
    );
    let dangling = "SELECT count(*) FROM pragma_foreign_key_check('mono_ask_events')";
    assert_eq!(count(&world, dangling), 0);
    let dropped = world.conn().execute(
        "UPDATE mono_asks SET state = 'dropped' WHERE id = ?1",
        [other_ask.as_str()],
    );
    assert!(dropped.is_err(), "nothing expires: {dropped:?}");
    world.kill();
    world.restart();
    assert_eq!(
        asks_snapshot(&world),
        migrated,
        "a second restart changes nothing"
    );
    ack_after_migration(&world, &tree, &reset, [&giga_ask, &mono_ask, &other_ask]);
}

/// Each migrated open ask is acked by its child: the Giga's by the Mono from
/// its new chat, the Mono's by its lead, the formerly `dropped` one by its
/// worker. The lead's ack reaches the Mono by its id.
fn ack_after_migration(world: &World, tree: &Tree, reset: &Mono, asks: [&MessageId; 3]) {
    let [giga_ask, mono_ask, other_ask] = asks;
    let mut live = tree.live().to_vec();
    live.push(&reset.chat);
    let done = |ask: &MessageId| json!({"ask_id": ask.as_str(), "message": "Done"});
    let lead = session_caller(&tree.lead, P);
    let other = session_caller(&tree.other, P);
    written(world.tool(
        &mono_caller(reset, "m2"),
        "m-ack",
        "ack",
        done(giga_ask),
        &live,
    ));
    written(world.tool(&lead, "l-ack", "ack", done(mono_ask), &live));
    written(world.tool(&other, "o-ack", "ack", done(other_ask), &live));
    assert_eq!(
        ack_went_to(world, mono_ask),
        (Some(tree.mono.id.as_str().into()), None)
    );
}

/// Review r1 n3. Defect: an ack with a field the tool does not have is
/// taken, or a second ack event of one ask can be stored.
#[test]
fn an_ack_takes_only_its_fields_and_stores_one_event() {
    let world = World::new("r5-ack-shape");
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
    let input = json!({"ask_id": id.as_str(), "message": "Done", "due_minutes": 5});
    let got = world.tool(&worker, "w-extra", "ack", input, &tree.live());
    assert_eq!(refusal(got).0, "invalid_input");
    assert_eq!(ask_row(&world, &id).0, "open");
    written(ack(&world, &tree, &worker, "w-ack", &id));
    let second = world.conn().execute(
        "INSERT INTO mono_ask_events (ask_id, kind, seq, at_active_ms, at_wall_ms)
         VALUES (?1, 'ack', 2, 0, 0)",
        [id.as_str()],
    );
    assert!(second.is_err(), "{second:?}");
}
