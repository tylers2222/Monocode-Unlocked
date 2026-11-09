//! Alarms S1, lead ruling S1 r3 a: the alarms' own rows (alarm, hold,
//! nudge) wait for no recipient on any path a wake is read from. Messages
//! and completion results are written by tools and turns, never by these
//! rows, so the outbox and the saved queue are the paths they travel.
//!
//! ADE-270: a row of `mono_completions` is a wake only once it holds a result.

use serde_json::{json, Value};

use super::agents::Mono;
use super::alarms_stall::{
    align, episodes, ran_a_turn, save_queue, start_lead, turn, MINUTE, N, P, PASS,
};
use super::world::World;
use crate::ask_scheduler::ALARM_STAGES;
use crate::mono_completions::{self, Owner};
use crate::mono_messages::SessionId;
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

const WINDOW: Owner<'static> = Owner {
    window: "main",
    run: "run-1",
};

/// A row as the window writes it while the request's turn runs
/// (`undelivered` in monoSessionCompletion.ts).
fn watching(mono: &Mono, request: &str, session: &SessionId) -> Value {
    json!({
        "origin": {"monoId": mono.chat.as_str(), "turn": 1},
        "request": {
            "requestId": request, "sessionId": session.as_str(), "project": P, "prompt": "Build it.",
        },
    })
}

/// The same row once the turn ended: it holds the frozen result.
fn finished(mono: &Mono, request: &str, session: &SessionId) -> Value {
    let mut row = watching(mono, request, session);
    row["result"] = json!({
        "sessionId": session.as_str(), "project": P, "title": "Agent session",
        "status": "completed", "originalPrompt": "Build it.", "result": "Done.", "truncated": false,
    });
    row
}

fn hold(world: &World, rows: &[Value]) {
    mono_completions::hold(&world.conn(), WINDOW, rows).unwrap();
}

/// The completion results the agents' episodes list.
fn listed_notices(world: &World) -> Vec<String> {
    world
        .conn()
        .prepare(
            "SELECT json_extract(w.value, '$.id')
               FROM alarm_episodes e, json_each(e.open_work_json) w
              WHERE json_extract(w.value, '$.kind') = 'notice'",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// Defect (ADE-270): the window writes a row at the hand-off, so a Mono that
// handed work to a session and ended its turn alarms as wake_missed two
// minutes later, and again after each new hand-off.
#[test]
fn a_mono_monitoring_a_running_session_misses_no_wake() {
    let mut world = World::new("alarm-monitoring");
    world.add_mono_in("alm-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alm-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alm-lead");
    align(&mut world);
    turn(&mut world, &lead, "lead-turn", true);
    hold(&world, &[watching(&mono, "req-1", &lead)]);
    ran_a_turn(&mut world, &mono.chat);
    world.advance(2 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "first hand-off");

    hold(
        &world,
        &[
            watching(&mono, "req-1", &lead),
            watching(&mono, "req-2", &lead),
        ],
    );
    ran_a_turn(&mut world, &mono.chat);
    world.advance(2 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "second hand-off");
}

// Defect (ADE-270): a fix that drops the running rows drops a real miss with
// them: the result of a finished session that no turn of the Mono took, held
// without a notice or in one, opens no alarm. A sibling that still runs is
// not listed with it.
#[test]
fn a_finished_result_no_turn_took_is_wake_missed() {
    for (stage, notice, listed) in [
        ("no notice yet", None, "req-1"),
        ("in a notice", Some("notice-1"), "notice-1"),
    ] {
        let mut world = World::new(&format!("alarm-finished-{}", stage.replace(' ', "-")));
        world.add_mono_in("alr-general", MonoRole::Giga, &[P]);
        let mono = world.add_mono_in("alr-mono", MonoRole::Mono, &[P]);
        let lead = start_lead(&world, &mono, "alr-lead");
        let sibling = start_lead(&world, &mono, "alr-sibling");
        align(&mut world);
        turn(&mut world, &lead, "lead-turn", true);
        turn(&mut world, &sibling, "sibling-turn", true);
        let running = watching(&mono, "req-2", &sibling);
        hold(&world, &[watching(&mono, "req-1", &lead), running.clone()]);
        ran_a_turn(&mut world, &mono.chat);

        turn(&mut world, &lead, "lead-turn", false);
        let mut ended = finished(&mono, "req-1", &lead);
        if let Some(notice) = notice {
            ended["notice"] = json!(notice);
        }
        hold(&world, &[ended, running]);
        world.advance(2 * MINUTE - PASS);
        assert_eq!(episodes(&world), Vec::new(), "{stage}: under two minutes");

        world.advance(3 * PASS);
        let [episode] = <[_; 1]>::try_from(episodes(&world)).unwrap();
        assert_eq!(
            (
                episode.actor.as_str(),
                episode.provisioner.as_str(),
                episode.cause.as_str()
            ),
            ("mono:alr-mono", "mono:alr-general", "wake_missed"),
            "{stage}"
        );
        assert_eq!(listed_notices(&world), [listed], "{stage}");
    }
}
