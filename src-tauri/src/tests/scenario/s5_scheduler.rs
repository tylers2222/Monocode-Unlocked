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

const P: &str = "/p";
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
/// Plan section 2: the first reminder goes out 5 active minutes before the
/// deadline.
const T5_LEAD: i64 = 5 * MINUTE;
/// R2: the active time a kill may lose, the clock row's persist interval.
const PERSIST: i64 = 5 * SECOND;

fn ask(
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

fn due(world: &World, ask: &MessageId) -> i64 {
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

/// The outbox rows of `ask` as `(key, recipient, at_active_ms)`.
fn reminders(world: &World, ask: &MessageId) -> Vec<(String, String, i64)> {
    world
        .conn()
        .prepare(
            "SELECT key, recipient, at_active_ms FROM mono_outbox WHERE ask_id = ?1 ORDER BY key",
        )
        .unwrap()
        .query_map([ask.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
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

fn advance_to(world: &mut World, at: i64) {
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
    let key = format!("reminder:{}:t5", ask.as_str());
    assert_eq!(reminders(world, ask), [(key, recipient.to_string(), at)]);
    assert_eq!(ladder(world, ask), (1, None));
}

/// S5 "killed and restarted 40 minutes later", "a one-hour sleep", "the
/// system clock jumps forward and back" and "after every restart, all
/// pending reminders are rebuilt from the database". Defect: a reminder is
/// enqueued before its active time; app downtime, sleep or a wall jump
/// moves a reminder earlier; a second enqueue of one key fails the tick; a
/// claim a window held when the app died keeps its reminder from every
/// window; an acked ask keeps a pending reminder; a ladder stage past this
/// build's ladder stops the tick; or a Giga's untimed ask is reminded.
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
    let (m, l) = (mono_caller(&mono, "m1"), session_caller(&lead, P));
    let untimed = ask(
        &world,
        &live,
        &mono_caller(&giga, "g1"),
        "s5-g",
        mono.id.as_str(),
        30,
    );
    let a = ask(&world, &live, &m, "s5-a", lead.as_str(), 10);
    let acked = ask(&world, &live, &l, "s5-f", w1.as_str(), 10);
    let b = ask(&world, &live, &l, "s5-b", w2.as_str(), 15);
    let c = ask(&world, &live, &m, "s5-c", lead.as_str(), 20);
    let d = ask(&world, &live, &l, "s5-d", w1.as_str(), 25);
    let [ta, tf, tb, tc, td] = [&a, &acked, &b, &c, &d].map(|id| due(&world, id) - T5_LEAD);
    assert_eq!(
        ladder(&world, &untimed),
        (0, None),
        "a Giga's ask has no clock"
    );
    assert_eq!(ladder(&world, &a), (0, Some(ta)));

    // At its active time to the millisecond, never before.
    falls_due_at(&mut world, &a, ta, "session:s5-lead");
    assert_eq!(tf, ta);
    assert_eq!(reminders(&world, &acked).len(), 1);
    assert_eq!(pending(&world), derived(&world));

    // Spending the key withdraws its unsent reminder.
    let ack = json!({"ask_id": acked.as_str(), "message": "Done"});
    written(world.tool(&session_caller(&w1, P), "s5-f-ack", "ack", ack, &live));
    assert!(
        reminders(&world, &acked).is_empty(),
        "an acked ask has no reminder"
    );

    // A window claimed A's reminder and had not sent it when the app died.
    let a_key = format!("reminder:{}:t5", a.as_str());
    world
        .conn()
        .execute(
            "UPDATE mono_outbox SET claimed_by = 'main' WHERE key = ?1",
            [&a_key],
        )
        .unwrap();

    // Killed and restarted 40 minutes later: the active clock lost at most
    // its persist interval, nothing fell due, and B's reminder comes 40
    // minutes later on the wall clock than it would have.
    let (wall_kill, active_kill) = (world.now(), world.active_ms());
    world.down(40 * MINUTE);
    let lost = active_kill - world.active_ms();
    assert!((0..=PERSIST).contains(&lost), "a restart lost {lost} ms");
    assert_eq!(
        pending(&world),
        derived(&world),
        "pending rebuilt from mono_asks"
    );
    assert_eq!(schema(&world), opened, "a second open changes no schema");
    falls_due_at(&mut world, &b, tb, "session:s5-w2");
    assert_eq!(
        world.now() - wall_kill - (tb - active_kill),
        40 * MINUTE + lost,
        "B's reminder moved back by the time the app was down"
    );

    // A stage write lost (an older copy of mono_asks) makes A fall due
    // again: its key keeps one row, and the tick goes on.
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET stage = 0, next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), ta],
        )
        .unwrap();
    world.advance(SECOND);
    assert_eq!(
        reminders(&world, &a),
        [(a_key.clone(), "session:s5-lead".into(), ta)]
    );
    assert_eq!(ladder(&world, &a), (1, None));

    // A one-hour sleep a minute before C's reminder moves it back an hour.
    advance_to(&mut world, tc - MINUTE);
    let (wall_sleep, active_sleep) = (world.now(), world.active_ms());
    world.sleep(HOUR);
    assert_eq!(world.active_ms(), active_sleep, "sleep adds no active time");
    falls_due_at(&mut world, &c, tc, "session:s5-lead");
    assert_eq!(
        world.now() - wall_sleep - (tc - active_sleep),
        HOUR,
        "C's reminder moved back by the hour asleep"
    );

    // The wall clock jumps 2 h forward past D's deadline, then 4 h back:
    // D's reminder still comes at its active time.
    advance_to(&mut world, td - MINUTE);
    world.jump_wall(2 * HOUR);
    assert!(
        reminders(&world, &d).is_empty(),
        "a wall jump is no active time"
    );
    world.jump_wall(-4 * HOUR);
    falls_due_at(&mut world, &d, td, "session:s5-w1");

    // A later build's ladder has stages past this one's: its next due on an
    // ask at the end of this ladder is left alone.
    let later = world.active_ms();
    world
        .conn()
        .execute(
            "UPDATE mono_asks SET next_due_active_ms = ?2 WHERE id = ?1",
            params![a.as_str(), later],
        )
        .unwrap();
    world.advance(SECOND);
    assert_eq!(reminders(&world, &a).len(), 1);
    assert_eq!(ladder(&world, &a), (1, Some(later)));

    // After a reboot the pending set is again what mono_asks implies: one
    // reminder per timed open ask, none for the Giga's ask.
    world.reboot(10 * MINUTE);
    assert_eq!(pending(&world), derived(&world));
    assert_eq!(schema(&world), opened);
    let keys: Vec<String> = pending(&world).into_iter().map(|(key, _)| key).collect();
    let mut expected = [&a, &b, &c, &d].map(|id| format!("reminder:{}:t5", id.as_str()));
    expected.sort();
    assert_eq!(keys, expected);
    assert!(
        reminders(&world, &untimed).is_empty(),
        "a Giga's ask is never reminded"
    );
}
