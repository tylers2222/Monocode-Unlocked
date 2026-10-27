//! S5 (R8): the ask scheduler enqueues each reminder at its active time,
//! across a kill and a restart 40 minutes later, a 1 h sleep, wall-clock
//! jumps and a reboot, and every restart finds the pending reminders in
//! the database.

use rusqlite::params;
use serde_json::json;

use super::agents::{mono_caller, session_caller};
use super::asks::written;
use super::world::World;
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, SessionId};
use crate::mono_roster::MonoRole;

pub(super) const P: &str = "/p";
const SECOND: i64 = 1_000;
pub(super) const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
/// Plan section 2: the first reminder goes out 5 active minutes before the
/// deadline.
pub(super) const T5_LEAD: i64 = 5 * MINUTE;
/// R2: the active time a kill may lose, the clock row's persist interval.
const PERSIST: i64 = 5 * SECOND;

type Reminder = (String, String, String, i64);

pub(super) fn ask(
    world: &World,
    live: &[&SessionId],
    from: &Caller,
    key: &str,
    to: &str,
    due: u32,
) -> MessageId {
    let input = json!({"message": format!("Do {key}"), "send_to": to, "due_minutes": due});
    written(world.tool(from, key, "ask", input, live))
}

fn ack(world: &World, live: &[&SessionId], child: &SessionId, ask: &MessageId) {
    let input = json!({"ask_id": ask.as_str(), "message": "Done"});
    let key = format!("ack-{}", ask.as_str());
    written(world.tool(&session_caller(child, P), &key, "ack", input, live));
}

pub(super) fn due(world: &World, ask: &MessageId) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT due_active_ms FROM mono_asks WHERE id = ?1",
            [ask.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// The scheduler's state of `ask`: its stage and next due.
fn ladder(world: &World, ask: &MessageId) -> (i64, Option<i64>) {
    world
        .conn()
        .query_row(
            "SELECT stage, next_due_active_ms FROM mono_asks WHERE id = ?1",
            [ask.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

pub(super) fn t5_key(ask: &MessageId) -> String {
    format!("reminder:{}:t5", ask.as_str())
}

/// The outbox rows of `ask` as `(key, stage, recipient, at_active_ms)`.
fn reminders(world: &World, ask: &MessageId) -> Vec<Reminder> {
    world
        .conn()
        .prepare(
            "SELECT key, stage, recipient, at_active_ms FROM mono_outbox
              WHERE ask_id = ?1 ORDER BY key",
        )
        .unwrap()
        .query_map([ask.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn t5(ask: &MessageId, recipient: &str, at: i64) -> Vec<Reminder> {
    vec![(t5_key(ask), "t5".into(), recipient.into(), at)]
}

fn claimed_by(world: &World, key: &str) -> Option<String> {
    world
        .conn()
        .query_row(
            "SELECT claimed_by FROM mono_outbox WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn pairs(world: &World, sql: &str) -> Vec<(String, String)> {
    world
        .conn()
        .prepare(sql)
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The reminders waiting for a window: unsent and unclaimed.
fn pending(world: &World) -> Vec<(String, String)> {
    pairs(
        world,
        "SELECT key, recipient FROM mono_outbox
          WHERE sent_at IS NULL AND claimed_by IS NULL ORDER BY key",
    )
}

/// The pending reminders `mono_asks` implies: a `t5` reminder to the child
/// of every open ask whose ladder is past stage 0.
fn derived(world: &World) -> Vec<(String, String)> {
    pairs(
        world,
        "SELECT 'reminder:' || id || ':t5', child_actor FROM mono_asks
          WHERE state = 'open' AND stage >= 1 ORDER BY 1",
    )
}

fn schema(world: &World) -> Vec<(String, String)> {
    pairs(
        world,
        "SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name",
    )
}

pub(super) fn advance_to(world: &mut World, at: i64) {
    let left = at - world.active_ms();
    assert!(left >= 0, "active time is already past {at}");
    world.advance(left);
}

/// 1 ms before `at` `ask` has no reminder; at `at` its `t5` reminder to
/// `recipient` is enqueued and its ladder has no next stage.
fn falls_due_at(world: &mut World, ask: &MessageId, at: i64, recipient: &str) {
    advance_to(world, at - 1);
    assert!(
        reminders(world, ask).is_empty(),
        "no reminder 1 ms before {at}"
    );
    world.advance(1);
    assert_eq!(reminders(world, ask), t5(ask, recipient, at));
    assert_eq!(ladder(world, ask), (1, None));
}

fn outbox_rows(world: &World) -> i64 {
    world
        .conn()
        .query_row("SELECT count(*) FROM mono_outbox", [], |row| row.get(0))
        .unwrap()
}

/// Review r1 m3. The tick is one transaction: a fault on the stage write,
/// then on the outbox insert, put on the store's own connection, fails the
/// tick and leaves `a` and the outbox as they were. With the fault gone, a
/// tick at the same active time enqueues `a`'s reminder once.
fn a_failed_tick_writes_nothing(world: &mut World, a: &MessageId, ta: i64) {
    advance_to(world, ta - 1);
    assert_eq!(outbox_rows(world), 0, "no reminder before {ta}");
    for (step, fault) in [
        (1, "BEFORE UPDATE OF stage ON mono_asks"),
        (0, "BEFORE INSERT ON mono_outbox"),
    ] {
        world
            .conn()
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER s5_fault {fault} BEGIN SELECT RAISE(ABORT, 's5 fault'); END;"
            ))
            .unwrap();
        let ticked = world.try_advance(step);
        world
            .conn()
            .execute_batch("DROP TRIGGER temp.s5_fault;")
            .unwrap();
        assert!(ticked.is_err(), "a fault {fault} fails the tick");
        assert_eq!(world.active_ms(), ta);
        assert_eq!(ladder(world, a), (0, Some(ta)), "{fault}: no stage moved");
        assert_eq!(outbox_rows(world), 0, "{fault}: no row written");
    }
    world.try_advance(0).unwrap();
    assert_eq!(reminders(world, a), t5(a, "session:s5-lead", ta));
    assert_eq!(ladder(world, a), (1, None));
}

/// Spending a key withdraws its unsent reminder, and keeps a sent one: a
/// window claimed and sent `sent`'s reminder before its child acked.
fn acks_withdraw_unsent_reminders(
    world: &World,
    live: &[&SessionId],
    (acked, acked_child): (&MessageId, &SessionId),
    (sent, sent_child): (&MessageId, &SessionId),
) {
    world
        .conn()
        .execute(
            "UPDATE mono_outbox SET claimed_by = 'main', sent_at = ?2 WHERE key = ?1",
            params![t5_key(sent), world.now()],
        )
        .unwrap();
    ack(world, live, acked_child, acked);
    ack(world, live, sent_child, sent);
    assert!(
        reminders(world, acked).is_empty(),
        "an acked ask has no unsent reminder"
    );
    assert_eq!(reminders(world, sent).len(), 1, "a sent reminder is kept");
}

/// S5 "killed and restarted 40 minutes later": the active clock lost at
/// most its persist interval, the unsent claim is pending again, nothing
/// fell due, and `b`'s reminder comes 40 minutes later on the wall clock
/// than it would have. Review r1 m2: `legacy`, an open ask as R5 wrote it
/// (no next due), gets its next due on open and is reminded at the first
/// tick, since its time passed while it had none.
fn restarts_40_minutes_later(
    world: &mut World,
    (b, tb): (&MessageId, i64),
    (legacy, t_legacy): (&MessageId, i64),
    sent: &MessageId,
) {
    let (wall_kill, active_kill) = (world.now(), world.active_ms());
    world.down(40 * MINUTE);
    let lost = active_kill - world.active_ms();
    assert!((0..=PERSIST).contains(&lost), "a restart lost {lost} ms");
    assert_eq!(
        pending(world),
        derived(world),
        "pending rebuilt from mono_asks"
    );
    assert_eq!(
        claimed_by(world, &t5_key(sent)).as_deref(),
        Some("main"),
        "a sent reminder stays claimed"
    );
    assert_eq!(ladder(world, legacy), (0, Some(t_legacy)), "seeded on open");
    let first_tick = world.active_ms() + SECOND;
    falls_due_at(world, b, tb, "session:s5-w2");
    assert_eq!(
        reminders(world, legacy),
        t5(legacy, "session:s5-w1", first_tick)
    );
    assert_eq!(
        world.now() - wall_kill - (tb - active_kill),
        40 * MINUTE + lost,
        "b's reminder moved back by the time the app was down"
    );
}

/// A stage write lost (an older copy of `mono_asks`) makes `a` fall due
/// again: its key keeps one row, and the tick goes on.
fn a_lost_stage_write_keeps_one_row(world: &mut World, a: &MessageId, ta: i64) {
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET stage = 0, next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), ta],
        )
        .unwrap();
    world.advance(SECOND);
    assert_eq!(reminders(world, a), t5(a, "session:s5-lead", ta));
    assert_eq!(ladder(world, a), (1, None));
}

/// S5 "a one-hour sleep": an hour asleep a minute before `c`'s reminder
/// moves it back an hour.
fn sleeps_an_hour(world: &mut World, c: &MessageId, tc: i64) {
    advance_to(world, tc - MINUTE);
    let (wall_sleep, active_sleep) = (world.now(), world.active_ms());
    world.sleep(HOUR);
    assert_eq!(world.active_ms(), active_sleep, "sleep adds no active time");
    falls_due_at(world, c, tc, "session:s5-lead");
    assert_eq!(
        world.now() - wall_sleep - (tc - active_sleep),
        HOUR,
        "c's reminder moved back by the hour asleep"
    );
}

/// S5 "the system clock jumps forward and back": a jump 2 h forward past
/// `d`'s deadline and 4 h back leave its reminder to active time. A tick
/// that lands past the due time enqueues at the tick's active time.
fn wall_jumps_leave_reminders_to_active_time(world: &mut World, d: &MessageId, td: i64) {
    advance_to(world, td - MINUTE);
    world.jump_wall(2 * HOUR);
    assert!(
        reminders(world, d).is_empty(),
        "a wall jump is no active time"
    );
    world.jump_wall(-4 * HOUR);
    advance_to(world, td - 1);
    assert!(reminders(world, d).is_empty(), "no reminder before {td}");
    world.stall(2 * SECOND);
    assert_eq!(
        reminders(world, d),
        t5(d, "session:s5-w1", td - 1 + 2 * SECOND)
    );
}

/// A later build's ladder has stages past this one's: its next due on an
/// ask at the end of this ladder is left alone, and the tick goes on.
fn a_later_builds_stage_is_left_alone(world: &mut World, a: &MessageId) {
    let later = world.active_ms();
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), later],
        )
        .unwrap();
    world.advance(SECOND);
    assert_eq!(reminders(world, a).len(), 1);
    assert_eq!(ladder(world, a), (1, Some(later)));
}

/// S5 "killed and restarted 40 minutes later", "a one-hour sleep", "the
/// system clock jumps forward and back" and "after every restart, all
/// pending reminders are rebuilt from the database". Defect: a reminder is
/// enqueued before its active time or stamped with another time; app
/// downtime, sleep or a wall jump moves a reminder earlier; a second enqueue
/// of one key fails the tick; a claim a window held when the app died keeps
/// its reminder from every window, or a sent one is pending again; an ack
/// leaves an unsent reminder or drops a sent one; a ladder stage past this
/// build's ladder stops the tick; a Giga's untimed ask is reminded; an open
/// ask from before R8 is never reminded; or a failed tick keeps half its
/// writes.
#[test]
fn s5_reminders_fall_due_in_active_time_across_restarts_sleep_and_wall_jumps() {
    let mut world = World::new("s5");
    let giga = world.add_mono_in("s5-giga", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("s5-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "s5-lead", P);
    let w1 = world.add_session("s5-w1", P, Some("s5-lead"));
    let w2 = world.add_session("s5-w2", P, Some("s5-lead"));
    let opened = schema(&world);
    world.advance(20 * SECOND);

    let live = [&lead, &w1, &w2];
    let (g, m, l) = (
        mono_caller(&giga, "g1"),
        mono_caller(&mono, "m1"),
        session_caller(&lead, P),
    );
    let untimed = ask(&world, &live, &g, "s5-g", mono.id.as_str(), 30);
    let a = ask(&world, &live, &m, "s5-a", lead.as_str(), 10);
    let acked = ask(&world, &live, &l, "s5-f", w1.as_str(), 10);
    let sent = ask(&world, &live, &l, "s5-e", w2.as_str(), 10);
    let b = ask(&world, &live, &l, "s5-b", w2.as_str(), 15);
    let c = ask(&world, &live, &m, "s5-c", lead.as_str(), 20);
    let d = ask(&world, &live, &l, "s5-d", w1.as_str(), 25);
    let legacy = ask(&world, &live, &l, "s5-r5", w1.as_str(), 10);
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET next_due_active_ms = NULL WHERE id = ?1",
            [legacy.as_str()],
        )
        .unwrap();
    let [ta, tb, tc, td] = [&a, &b, &c, &d].map(|id| due(&world, id) - T5_LEAD);
    assert_eq!(ladder(&world, &untimed), (0, None), "a Giga sets no clock");
    assert_eq!(ladder(&world, &a), (0, Some(ta)));

    // At its active time to the millisecond, never before, in one transaction.
    a_failed_tick_writes_nothing(&mut world, &a, ta);
    assert_eq!(reminders(&world, &acked), t5(&acked, "session:s5-w1", ta));
    assert_eq!(reminders(&world, &sent), t5(&sent, "session:s5-w2", ta));
    assert!(
        reminders(&world, &legacy).is_empty(),
        "no next due, no reminder"
    );
    assert_eq!(pending(&world), derived(&world));
    acks_withdraw_unsent_reminders(&world, &live, (&acked, &w1), (&sent, &w2));

    // A window claimed `a`'s reminder and had not sent it when the app died.
    world
        .conn()
        .execute(
            "UPDATE mono_outbox SET claimed_by = 'main' WHERE key = ?1",
            [t5_key(&a)],
        )
        .unwrap();
    let t_legacy = due(&world, &legacy) - T5_LEAD;
    restarts_40_minutes_later(&mut world, (&b, tb), (&legacy, t_legacy), &sent);
    assert_eq!(schema(&world), opened, "a second open changes no schema");
    a_lost_stage_write_keeps_one_row(&mut world, &a, ta);
    sleeps_an_hour(&mut world, &c, tc);
    wall_jumps_leave_reminders_to_active_time(&mut world, &d, td);
    a_later_builds_stage_is_left_alone(&mut world, &a);

    // After a reboot the pending set is again what mono_asks implies: one
    // reminder per timed open ask, none for the Giga's ask.
    world.reboot(10 * MINUTE);
    assert_eq!(pending(&world), derived(&world));
    assert_eq!(schema(&world), opened);
    let keys: Vec<String> = pending(&world).into_iter().map(|(key, _)| key).collect();
    let mut expected = [&a, &b, &c, &d, &legacy].map(t5_key);
    expected.sort();
    assert_eq!(keys, expected);
    assert!(
        reminders(&world, &untimed).is_empty(),
        "a Giga's ask is never reminded"
    );
}
