//! Alarms S1, lead ruling S1 r3 a: the alarms' own rows (alarm, hold,
//! nudge) wait for no recipient on any path a wake is read from. Messages
//! and completion results are written by tools and turns, never by these
//! rows, so the outbox and the saved queue are the paths they travel.

use serde_json::json;

use super::agents::Mono;
use super::alarms_stall::{align, episodes, ran_a_turn, save_queue, start_lead, N, P};
use super::world::World;
use crate::ask_scheduler::ALARM_STAGES;
use crate::mono_roster::MonoRole;

/// Where a row of the outbox can wait for its recipient's provider.
#[derive(Debug, Clone, Copy)]
enum Carrier {
    /// Unclaimed, with a prompt (`pending`) when this build has one.
    Unclaimed,
    Claimed,
    /// Sent, and the window's saved queue still lists it: it saves its queue
    /// after the submit that sent the row.
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

/// Lead `alw-lead` idles with `mono`'s ask, so at N the detector gives the
/// Mono an alarm row with its prompt; the ask's id.
fn idle_lead_with_an_ask(world: &mut World, mono: &Mono) -> String {
    let lead = start_lead(world, mono, "alw-lead");
    ran_a_turn(world, &lead);
    world
        .conn()
        .query_row(
            "SELECT id FROM mono_asks WHERE child_actor = 'session:alw-lead'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Puts a row of `stage` for `mono` on `carrier`. An unclaimed row waits
/// only with a prompt: S1 has none for a hold or a nudge (S2 and H bring
/// theirs).
fn place(world: &mut World, carrier: Carrier, stage: &str, mono: &Mono) {
    let (key, ask) = match (carrier, stage) {
        (Carrier::Unclaimed, "alarm") => {
            idle_lead_with_an_ask(world, mono);
            return;
        }
        (Carrier::Unclaimed, "t5") => {
            let ask = idle_lead_with_an_ask(world, mono);
            (format!("reminder:{ask}:t5"), Some(ask))
        }
        _ => (format!("{stage}-a1b2-1"), None),
    };
    let recipient = format!("mono:{}", mono.id.as_str());
    let (claimed, sent) = match carrier {
        Carrier::Unclaimed | Carrier::QueuedRowGone => (None, None),
        Carrier::Claimed | Carrier::Queued => (Some("main"), None),
        Carrier::Sent => (Some("main"), Some(1)),
    };
    if !matches!(carrier, Carrier::QueuedRowGone) {
        world
            .conn()
            .execute(
                "INSERT INTO mono_outbox
                   (key, stage, recipient, ask_id, at_active_ms, claimed_by, sent_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
                rusqlite::params![key, stage, recipient, ask, claimed, sent],
            )
            .unwrap();
    }
    if matches!(
        carrier,
        Carrier::Sent | Carrier::Queued | Carrier::QueuedRowGone
    ) {
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
    place(&mut world, carrier, stage, &mono);
    world.advance(2 * N);
    let actor = format!("mono:{}", mono.id.as_str());
    episodes(&world)
        .into_iter()
        .filter(|episode| episode.actor == actor)
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
    for carrier in CARRIERS {
        let stage = match carrier {
            Carrier::Unclaimed => "t5",
            _ => "resume",
        };
        assert_eq!(
            causes_for(carrier, stage),
            vec!["wake_missed"],
            "a reminder on {carrier:?} still waits"
        );
    }
}
