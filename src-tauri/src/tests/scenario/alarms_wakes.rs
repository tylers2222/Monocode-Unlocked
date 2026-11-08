//! Alarms S1, lead ruling S1 r3 a: the alarms' own rows (alarm, hold,
//! nudge) wait for no recipient on any path a wake is read from. Messages
//! and completion results are written by tools and turns, never by these
//! rows, so the outbox and the saved queue are the paths they travel.

use serde_json::json;

use super::agents::Mono;
use super::alarms_stall::{align, episodes, ran_a_turn, save_queue, N, P};
use super::world::World;
use crate::ask_scheduler::ALARM_STAGES;
use crate::mono_roster::MonoRole;

/// Where a row of the outbox can wait for its recipient's provider.
#[derive(Debug, Clone, Copy)]
enum Carrier {
    Unclaimed,
    Claimed,
    Sent,
    /// Claimed, and the window queued it under its key (monoReminders.ts).
    Queued,
    /// The window's saved queue still holds it after its row went.
    QueuedRowGone,
}

const CARRIERS: [Carrier; 5] = [
    Carrier::Unclaimed,
    Carrier::Claimed,
    Carrier::Sent,
    Carrier::Queued,
    Carrier::QueuedRowGone,
];

/// Puts row `key` of `stage` for `mono` on `carrier`.
fn place(world: &World, carrier: Carrier, stage: &str, key: &str, mono: &Mono) {
    let (claimed, sent) = match carrier {
        Carrier::Unclaimed => (None, None),
        Carrier::Claimed | Carrier::Queued => (Some("main"), None),
        Carrier::Sent => (Some("main"), Some(1)),
        Carrier::QueuedRowGone => (None, None),
    };
    if !matches!(carrier, Carrier::QueuedRowGone) {
        world
            .conn()
            .execute(
                "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms, claimed_by, sent_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                rusqlite::params![
                    key,
                    stage,
                    format!("mono:{}", mono.id.as_str()),
                    claimed,
                    sent
                ],
            )
            .unwrap();
    }
    if matches!(carrier, Carrier::Queued | Carrier::QueuedRowGone) {
        let item = json!({"id": key, "text": "..", "appRequestId": key, "waitForIdle": true});
        save_queue(world, &mono.chat, json!([item]), "active");
    }
}

/// The causes an idle Mono chat opens in 2 x N with `stage`'s row on `carrier`.
fn causes_for(carrier: Carrier, stage: &str) -> Vec<String> {
    let mut world = World::new(&format!("alarm-wakes-{stage}-{carrier:?}"));
    world.add_mono_in("alw-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alw-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    place(&world, carrier, stage, &format!("{stage}-a1b2-1"), &mono);
    world.advance(2 * N);
    episodes(&world)
        .into_iter()
        .map(|episode| episode.cause)
        .collect()
}

// Defect (S1 r4 M1, lead ruling S1 r3 a): an alarm, hold or nudge row
// counts toward its recipient's WAKE_MISSED on one path or another (the
// claimed outbox row, the saved queue item the window keeps under the row's
// key), so each recipient alarms its own provisioner and one stall climbs.
#[test]
fn no_alarm_family_row_is_a_missed_wake_on_any_path() {
    for carrier in CARRIERS {
        for stage in ALARM_STAGES {
            assert_eq!(
                causes_for(carrier, stage),
                Vec::<String>::new(),
                "{stage} on {carrier:?}"
            );
        }
    }
    for (carrier, expected) in [
        (Carrier::Claimed, vec!["wake_missed"]),
        (Carrier::Queued, vec!["wake_missed"]),
        (Carrier::QueuedRowGone, vec!["wake_missed"]),
    ] {
        assert_eq!(
            causes_for(carrier, "resume"),
            expected,
            "a reminder on {carrier:?} still waits"
        );
    }
}
