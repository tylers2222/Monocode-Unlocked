//! S3 second half (R11), continuing `s3_extension`: at the extended deadline
//! the child is reminded again; 5 active minutes later the parent is told
//! and the ask gets its one `miss`; the child's reason is stored on it once,
//! through the function `mono_ask_reason` calls; the ask stays open until
//! the child acks. A paused ask gets neither. Real store, fake clock.

use rusqlite::params;
use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::asks::written;
use super::s5_delivery::claim;
use super::s5_scheduler::{advance_to, ask, due, ladder, reminders, MINUTE, P};
use super::world::World;
use crate::ask_scheduler;
use crate::asks::{self, ReasonInput, Reasoned};
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, SessionId};

/// Plan section 2: the parent hears 5 active minutes after the deadline.
const WHY_AFTER: i64 = 5 * MINUTE;
const REASON: &str = "The build cache was cold.";

/// An outbox row as `(key, stage, recipient, at_active_ms)`.
type Row = (String, String, String, i64);

/// The ask the second half follows: `a`, `lead`'s ask from `mono`, due at
/// `new_due` after its one extension.
struct Missed<'a> {
    mono: &'a Mono,
    lead: &'a SessionId,
    worker: &'a SessionId,
    a: &'a MessageId,
    parent: Caller,
    child: Caller,
    new_due: i64,
}

impl Missed<'_> {
    fn reason_input(&self, message: &str) -> Value {
        json!({"ask_id": self.a.as_str(), "message": message})
    }
}

fn reason(world: &World, caller: &Caller, input: Value) -> Reasoned {
    asks::reason(&world.conn(), caller, ReasonInput::parse(&input)).unwrap()
}

/// The hint of a reason failed with `code`; panics on any other answer.
fn failed(reasoned: Reasoned, code: &str) -> String {
    match reasoned {
        Reasoned::Failed { error } if error.code == code => error.hint,
        other => panic!("expected {code}, got {other:?}"),
    }
}

fn refused(reasoned: Reasoned) -> String {
    failed(reasoned, "reason_refused")
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

fn row(ask: &MessageId, stage: &str, recipient: &str, at: i64) -> Row {
    let key = format!("reminder:{}:{stage}", ask.as_str());
    (key, stage.into(), recipient.into(), at)
}

/// The prompt a window claims for reminder `key`.
fn claimed_text(world: &World, key: &str) -> Option<String> {
    claim(world, key, "main", &[], false).map(|claimed| claimed.text)
}

/// The exact prompt of the child's deadline reminder of ask `ask`.
fn deadline_text(ask: &MessageId) -> String {
    let id = ask.as_str();
    format!(
        "<mono_reminder ask_id=\"{id}\" stage=\"deadline\">\nAsk {id} is due now. Finish its work and ack it with this ask_id. In 5 active minutes your parent is told you missed it and will ask you why.\n</mono_reminder>"
    )
}

/// The exact prompt of the parent's `why` notice of ask `ask`, naming the
/// child `child` in its `child` attribute (the r11 puppet reads it) and text.
fn why_text(ask: &MessageId, child: &SessionId) -> String {
    let (id, child) = (ask.as_str(), child.as_str());
    format!(
        "<mono_reminder ask_id=\"{id}\" stage=\"why\" child=\"{child}\">\n{child} has not acked {id}. Ask it why: send it a message with send_to \"{child}\". It stores its reason with app messages.reason (the reason tool); the ask stays open until it acks.\n</mono_reminder>"
    )
}

/// S3 "At the new deadline: a second reminder.", "Still nothing: the parent
/// is told and asks "why?". The worker's reason is stored as a miss." and
/// "The ask is still open (nothing expires). The worker finally acks, and
/// the ask closes." `a` is `lead`'s ask from `mono`, extended once and at
/// 1 ms before its new deadline. Defect: the deadline reminder comes at any
/// time but the deadline or to anyone but the child, or its prompt changes;
/// the parent's notice comes at any time but 5 active minutes later or to
/// anyone but the parent, or its prompt does not name the child; the miss
/// is not written, written twice, or loses its reason when a stage write is
/// lost; a reason before the miss, from anyone but the child, or a second
/// one is stored; a blank reason or one over 8000 characters is stored, or
/// one of exactly 8000 is invalid; the miss closes the ask or the ladder
/// goes on; a paused ask is reminded or missed.
pub(super) fn the_new_deadline_passes(
    world: &mut World,
    mono: &Mono,
    lead: &SessionId,
    worker: &SessionId,
    a: &MessageId,
) {
    let missed = Missed {
        mono,
        lead,
        worker,
        a,
        parent: mono_caller(mono, "m2"),
        child: session_caller(lead, P),
        new_due: due(world, a),
    };
    let reminded = the_child_is_reminded_at_the_deadline(world, &missed);
    let sent = the_parent_is_told_and_asks_why(world, &missed, reminded);
    let stored = only_the_child_gives_its_reason_once(world, &missed);
    a_lost_stage_write_replays_why(world, &missed, &sent, &stored);
    nothing_expires_until_the_child_acks(world, &missed, &sent, &stored);
    a_paused_ask_gets_no_reminder_or_miss(world, &missed);
}

/// At the new deadline, to the millisecond: a second reminder, to the
/// child. A reason is refused before the miss, also one of exactly 8000
/// characters, which is valid input. Returns the ask's rows so far.
fn the_child_is_reminded_at_the_deadline(world: &mut World, missed: &Missed) -> Vec<Row> {
    let a = missed.a;
    let offer = reminders(world, a);
    world.advance(1);
    let child_actor = format!("session:{}", missed.lead.as_str());
    let deadline = row(a, "deadline", &child_actor, missed.new_due);
    let rows = [vec![deadline.clone()], offer].concat();
    assert_eq!(reminders(world, a), rows);
    assert_eq!(ladder(world, a), (2, Some(missed.new_due + WHY_AFTER)));
    assert_eq!(claimed_text(world, &deadline.0), Some(deadline_text(a)));
    let early = reason(world, &missed.child, missed.reason_input(REASON));
    assert!(refused(early).contains("has not missed"));
    let at_limit = reason(world, &missed.child, missed.reason_input(&"x".repeat(8000)));
    assert!(refused(at_limit).contains("has not missed"));
    rows
}

/// 5 active minutes later the parent is told, naming the child to ask, and
/// the ask has one miss with no reason yet. Returns every row sent.
fn the_parent_is_told_and_asks_why(
    world: &mut World,
    missed: &Missed,
    before: Vec<Row>,
) -> Vec<Row> {
    let a = missed.a;
    let at = missed.new_due + WHY_AFTER;
    advance_to(world, at - 1);
    assert_eq!(reminders(world, a).len(), 2);
    assert!(misses(world, a).is_empty());
    world.advance(1);
    let why = row(a, "why", &format!("mono:{}", missed.mono.id.as_str()), at);
    let sent = [before, vec![why.clone()]].concat();
    assert_eq!(reminders(world, a), sent);
    assert_eq!(ladder(world, a), (3, None));
    assert_eq!(misses(world, a), [None]);
    let listed = ask_scheduler::pending(&world.conn(), &missed.mono.chat).unwrap();
    assert!(listed.contains(&why.0), "the parent's chat lists `why`");
    assert_eq!(claimed_text(world, &why.0), Some(why_text(a, missed.lead)));
    sent
}

/// Only the child gives the reason, once; a blank one or one of 8001
/// characters is invalid input and stores nothing. Returns the stored misses.
fn only_the_child_gives_its_reason_once(world: &World, missed: &Missed) -> Vec<Option<String>> {
    let a = missed.a;
    for other in [&missed.parent, &session_caller(missed.worker, P)] {
        let answer = reason(world, other, missed.reason_input(REASON));
        assert!(refused(answer).contains("not yours"));
    }
    for message in [" \n ".to_string(), "x".repeat(8001)] {
        let answer = reason(world, &missed.child, missed.reason_input(&message));
        let hint = failed(answer, "invalid_input");
        assert!(hint.contains("at most 8000 characters"), "{hint}");
    }
    assert_eq!(misses(world, a), [None]);
    let Reasoned::Stored { ask_id } = reason(world, &missed.child, missed.reason_input(REASON))
    else {
        panic!("the child's reason was refused");
    };
    assert_eq!(&ask_id, a);
    let again = reason(world, &missed.child, missed.reason_input("Another reason."));
    assert!(refused(again).contains("already has its reason"));
    let stored = vec![Some(REASON.to_string())];
    assert_eq!(misses(world, a), stored);
    stored
}

/// A lost stage write replays `why`: still one row and one miss, which
/// keeps its reason.
fn a_lost_stage_write_replays_why(
    world: &mut World,
    missed: &Missed,
    sent: &[Row],
    stored: &[Option<String>],
) {
    let a = missed.a;
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET stage = 2, next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), missed.new_due + WHY_AFTER],
        )
        .unwrap();
    world.advance(1_000);
    assert_eq!(
        (reminders(world, a), misses(world, a)),
        (sent.to_vec(), stored.to_vec())
    );
}

/// Nothing expires: across a restart and 10 active minutes, the ask is open
/// and nothing more is sent. The child finally acks, and the ask closes; a
/// reason after that is refused.
fn nothing_expires_until_the_child_acks(
    world: &mut World,
    missed: &Missed,
    sent: &[Row],
    stored: &[Option<String>],
) {
    let a = missed.a;
    world.kill();
    world.restart();
    world.advance(10 * MINUTE);
    assert_eq!(state(world, a), "open");
    assert_eq!(
        (reminders(world, a), misses(world, a)),
        (sent.to_vec(), stored.to_vec())
    );
    let ack = json!({"ask_id": a.as_str(), "message": "Done at last."});
    let sessions = [missed.lead, missed.worker];
    written(world.tool(&missed.child, "s3-late-ack", "ack", ack, &sessions));
    assert_eq!(state(world, a), "acked");
    let late = reason(world, &missed.child, missed.reason_input(REASON));
    assert!(refused(late).contains("already acked"));
}

/// A paused ask (R14 pauses it while a question waits) gets no reminder and
/// no miss.
fn a_paused_ask_gets_no_reminder_or_miss(world: &mut World, missed: &Missed) {
    let sessions = [missed.lead, missed.worker];
    let b = ask(
        world,
        &sessions,
        &missed.parent,
        "s3-paused",
        missed.lead.as_str(),
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
