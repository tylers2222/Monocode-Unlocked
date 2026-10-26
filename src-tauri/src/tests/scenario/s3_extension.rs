//! S3 first half (R10): the `t5` reminder offers one extension, and the
//! child's `messages.extend` moves the deadline once, through the function
//! `mono_ask_extend` calls, on the real store and the fake clock.

use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller};
use super::s5_scheduler::{advance_to, ask, due, ladder, reminders, t5, MINUTE, P, T5_LEAD};
use super::world::World;
use crate::ask_scheduler;
use crate::asks::{self, ExtendInput, Extension};
use crate::mono_gate::Caller;
use crate::mono_messages::MessageId;
use crate::mono_roster::MonoRole;

const EXTENSION: i64 = 15 * MINUTE;

fn extend(world: &World, caller: &Caller, input: Value) -> Extension {
    asks::extend(
        &world.conn(),
        caller,
        ExtendInput::parse(&input),
        world.now(),
    )
    .unwrap()
}

/// The hint of a refused extend; panics on any other answer.
fn refused(extension: Extension) -> String {
    match extension {
        Extension::Failed { error } if error.code == "extend_refused" => error.hint,
        other => panic!("expected extend_refused, got {other:?}"),
    }
}

fn extensions(world: &World, ask: &MessageId) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'extension'",
            [ask.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// S3 "At 5 minutes before the deadline: a reminder that offers one
/// extension" (the row; `app.sh r9` reads its offer text and delivery) and
/// S3 "The extension is used: the deadline moves once." Defect: an extend
/// before the `t5` reminder, by the parent or another session, or a second
/// one moves the deadline; the deadline moves by anything but 15 minutes;
/// the scheduler's next stage is left at the old deadline or re-arms `t5`;
/// a restart forgets the extension.
#[test]
fn s3_the_t5_reminder_offers_one_extension_that_moves_the_deadline_once() {
    let mut world = World::new("s3x");
    let mono = world.add_mono_in("s3x-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "s3x-lead", P);
    let worker = world.add_session("s3x-worker", P, Some("s3x-lead"));
    let parent = mono_caller(&mono, "m1");
    let child = session_caller(&lead, P);
    let a = ask(
        &world,
        &[&lead, &worker],
        &parent,
        "s3x-a",
        lead.as_str(),
        10,
    );
    let old_due = due(&world, &a);
    let by_id = json!({"ask_id": a.as_str()});

    // Before the reminder the offer is closed.
    advance_to(&mut world, old_due - T5_LEAD - 1);
    assert!(refused(extend(&world, &child, by_id.clone())).contains("opens with the reminder"));
    assert_eq!(due(&world, &a), old_due);

    // The t5 reminder falls due 5 active minutes before the deadline.
    assert!(reminders(&world, &a).is_empty());
    world.advance(1);
    let offer = t5(&a, &format!("session:{}", lead.as_str()), old_due - T5_LEAD);
    assert_eq!(reminders(&world, &a), offer);
    assert_eq!(ladder(&world, &a), (1, Some(old_due)));

    // Only the child extends: not its parent, not another session.
    for other in [parent, session_caller(&worker, P)] {
        assert!(refused(extend(&world, &other, by_id.clone())).contains("not yours"));
    }
    assert_eq!((due(&world, &a), extensions(&world, &a)), (old_due, 0));

    // Unnamed, an ask is picked only when it is the child's one open ask;
    // this lead also has its start ask (R7). Named, the deadline moves 15
    // minutes, and `due_in` counts from the stored clock, as an ask's does.
    assert!(refused(extend(&world, &child, json!({}))).contains("exactly one open ask"));
    let Extension::Extended { ask_id, due_in_ms } = extend(&world, &child, by_id.clone()) else {
        panic!("the child's extend was refused");
    };
    let stored: i64 = world
        .conn()
        .query_row("SELECT active_ms FROM mono_clock WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let new_due = old_due + EXTENSION;
    assert_eq!((ask_id, due_in_ms), (a.clone(), new_due - stored));
    assert_eq!(due(&world, &a), new_due);
    assert_eq!(ladder(&world, &a), (1, ask_scheduler::next_due(new_due, 1)));
    assert_eq!(extensions(&world, &a), 1);

    // Once only, also after a restart.
    world.kill();
    world.restart();
    assert!(refused(extend(&world, &child, by_id)).contains("already extended once"));
    assert_eq!((due(&world, &a), extensions(&world, &a)), (new_due, 1));

    // Past the old deadline and up to the new one, the ladder sends nothing
    // more: no second offer, no stage at the old deadline.
    advance_to(&mut world, new_due - 1);
    assert_eq!(reminders(&world, &a), offer);
    assert_eq!(ladder(&world, &a), (1, ask_scheduler::next_due(new_due, 1)));

    // S3 second half (R11).
    super::s3_miss::the_new_deadline_passes(&mut world, &mono, &lead, &worker, &a);
}
