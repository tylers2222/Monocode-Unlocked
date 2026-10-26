//! S3 second half (R11), continuing `s3_extension`: at the extended deadline
//! the child is reminded again; 5 active minutes later the parent is told
//! and the ask gets its one `miss`; the child's reason is stored on it once,
//! through the function `mono_ask_reason` calls; the ask stays open until
//! the child acks. A paused ask gets neither. Real store, fake clock.

use rusqlite::params;
use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::asks::written;
use super::s5_scheduler::{advance_to, ask, due, ladder, reminders, MINUTE, P};
use super::world::World;
use crate::ask_scheduler::{self, Reminder};
use crate::asks::{self, ReasonInput, Reasoned};
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, SessionId};

/// Plan section 2: the parent hears 5 active minutes after the deadline.
const WHY_AFTER: i64 = 5 * MINUTE;
const REASON: &str = "The build cache was cold.";

fn reason(world: &World, caller: &Caller, input: Value) -> Reasoned {
    asks::reason(&world.conn(), caller, ReasonInput::parse(&input)).unwrap()
}

/// The hint of a refused reason; panics on any other answer.
fn refused(reasoned: Reasoned) -> String {
    match reasoned {
        Reasoned::Failed { error } if error.code == "reason_refused" => error.hint,
        other => panic!("expected reason_refused, got {other:?}"),
    }
}

/// The reason of each `miss` event of `ask`.
fn misses(world: &World, ask: &MessageId) -> Vec<Option<String>> {
    world
        .conn()
        .prepare("SELECT reason FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'miss'")
        .unwrap()
        .query_map([ask.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn state(world: &World, ask: &MessageId) -> String {
    world
        .conn()
        .query_row(
            "SELECT state FROM mono_asks WHERE id = ?1",
            [ask.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

fn row(ask: &MessageId, stage: &str, recipient: &str, at: i64) -> (String, String, String, i64) {
    let key = format!("reminder:{}:{stage}", ask.as_str());
    (key, stage.into(), recipient.into(), at)
}

/// S3 "At the new deadline: a second reminder.", "Still nothing: the parent
/// is told and asks "why?". The worker's reason is stored as a miss." and
/// "The ask is still open (nothing expires). The worker finally acks, and
/// the ask closes." `a` is `lead`'s ask from `mono`, extended once and at
/// 1 ms before its new deadline. Defect: the deadline reminder comes at any
/// time but the deadline or to anyone but the child; the parent's notice
/// comes at any time but 5 active minutes later or to anyone but the
/// parent, or does not name the child; the miss is not written, written
/// twice, or loses its reason when a stage write is lost; a reason before
/// the miss, from anyone but the child, or a second one is stored; the miss
/// closes the ask or the ladder goes on; a paused ask is reminded or missed.
pub(super) fn the_new_deadline_passes(
    world: &mut World,
    mono: &Mono,
    lead: &SessionId,
    worker: &SessionId,
    a: &MessageId,
) {
    let new_due = due(world, a);
    let parent = mono_caller(mono, "m2");
    let child = session_caller(lead, P);
    let by_id = json!({"ask_id": a.as_str(), "message": REASON});
    let child_actor = format!("session:{}", lead.as_str());
    let offer = reminders(world, a);

    // At the new deadline, to the millisecond: a second reminder, to the child.
    world.advance(1);
    let deadline = row(a, "deadline", &child_actor, new_due);
    assert_eq!(
        reminders(world, a),
        [vec![deadline.clone()], offer.clone()].concat()
    );
    assert_eq!(ladder(world, a), (2, Some(new_due + WHY_AFTER)));
    assert!(refused(reason(world, &child, by_id.clone())).contains("has not missed"));

    // 5 active minutes later the parent is told, naming the child to ask,
    // and the ask has one miss with no reason yet.
    advance_to(world, new_due + WHY_AFTER - 1);
    assert_eq!(reminders(world, a).len(), 2);
    assert!(misses(world, a).is_empty());
    world.advance(1);
    let why = row(
        a,
        "why",
        &format!("mono:{}", mono.id.as_str()),
        new_due + WHY_AFTER,
    );
    let sent = [vec![deadline], offer, vec![why.clone()]].concat();
    assert_eq!(reminders(world, a), sent);
    assert_eq!(ladder(world, a), (3, None));
    assert_eq!(misses(world, a), [None]);
    let notice = Reminder {
        key: why.0,
        ask_id: Some(a.as_str().into()),
        stage: "why".into(),
        child: Some(lead.as_str().into()),
    };
    assert!(ask_scheduler::pending(&world.conn(), &mono.chat)
        .unwrap()
        .contains(&notice));

    // Only the child gives the reason, once. The key stays.
    for other in [&parent, &session_caller(worker, P)] {
        assert!(refused(reason(world, other, by_id.clone())).contains("not yours"));
    }
    let Reasoned::Stored { ask_id } = reason(world, &child, by_id.clone()) else {
        panic!("the child's reason was refused");
    };
    assert_eq!(&ask_id, a);
    let again = json!({"ask_id": a.as_str(), "message": "Another reason."});
    assert!(refused(reason(world, &child, again)).contains("already has its reason"));
    let stored = [Some(REASON.to_string())];
    assert_eq!(misses(world, a), stored);

    // A lost stage write replays `why`: still one row and one miss, which
    // keeps its reason.
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET stage = 2, next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), new_due + WHY_AFTER],
        )
        .unwrap();
    world.advance(1_000);
    assert_eq!(
        (reminders(world, a), misses(world, a)),
        (sent.clone(), stored.to_vec())
    );

    // Nothing expires: across a restart and 10 active minutes, the ask is
    // open and nothing more is sent.
    world.kill();
    world.restart();
    world.advance(10 * MINUTE);
    assert_eq!(state(world, a), "open");
    assert_eq!(
        (reminders(world, a), misses(world, a)),
        (sent, stored.to_vec())
    );

    // The child finally acks, and the ask closes; a reason after that is refused.
    let ack = json!({"ask_id": a.as_str(), "message": "Done at last."});
    written(world.tool(&child, "s3-late-ack", "ack", ack, &[lead, worker]));
    assert_eq!(state(world, a), "acked");
    assert!(refused(reason(world, &child, by_id)).contains("already acked"));

    // A paused ask (R14 pauses it while a question waits) gets no reminder
    // and no miss.
    let b = ask(
        world,
        &[lead, worker],
        &parent,
        "s3-paused",
        lead.as_str(),
        10,
    );
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET paused_at_active_ms = ?2 WHERE id = ?1",
            params![b.as_str(), world.active_ms()],
        )
        .unwrap();
    advance_to(world, due(world, &b) + WHY_AFTER + MINUTE);
    assert!(reminders(world, &b).is_empty());
    assert!(misses(world, &b).is_empty());
}
