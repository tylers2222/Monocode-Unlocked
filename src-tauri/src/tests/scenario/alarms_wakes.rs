//! Alarms S1, lead ruling S1 r3 a: the alarms' own rows (alarm, hold,
//! nudge) wait for no recipient on any path a wake is read from. Messages
//! and completion results are written by tools and turns, never by these
//! rows, so the outbox and the saved queue are the paths they travel.
//!
//! ADE-270: a row of `mono_completions` is a wake only once it holds a result,
//! or (ADE-271) once its session stopped and no result came.

use serde_json::{json, Value};

use super::agents::Mono;
use super::alarms_stall::{
    align, claude_tap, episodes, ran_a_turn, save_queue, start_lead, turn, MINUTE, N, P, PASS,
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
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "first hand-off");

    hold(
        &world,
        &[
            watching(&mono, "req-1", &lead),
            watching(&mono, "req-2", &lead),
        ],
    );
    ran_a_turn(&mut world, &mono.chat);
    world.advance(4 * MINUTE + 2 * PASS);
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

        // Past the threshold and the wait of its one retry (ADE-276).
        world.advance(2 * MINUTE + 3 * PASS);
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

// Defect (review r1b M2): the user stopped the Mono's queue, and the paused
// rule hid the result a finished session owes it with the rest of the queue,
// so no turn and no alarm ever takes it (ADE-271).
#[test]
fn a_result_owed_to_a_mono_the_user_stopped_still_alarms() {
    let mut world = World::new("alarm-finished-user-stop");
    world.add_mono_in("als-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("als-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "als-lead");
    align(&mut world);
    turn(&mut world, &lead, "lead-turn", true);
    hold(&world, &[watching(&mono, "req-1", &lead)]);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "left-1"}]), "paused");

    turn(&mut world, &lead, "lead-turn", false);
    hold(&world, &[finished(&mono, "req-1", &lead)]);
    world.advance(4 * MINUTE + 3 * PASS);
    let [episode] = <[_; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("mono:als-mono", "wake_missed")
    );
    assert_eq!(listed_notices(&world), ["req-1"], "the result only");
}

/// Mono `alb-mono` handed work to `alb-lead`, whose turn runs, and ended its
/// own turn: the row is bare.
fn monitoring_a_running_session(name: &str) -> (World, Mono, SessionId) {
    let mut world = World::new(name);
    world.add_mono_in("alb-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alb-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alb-lead");
    align(&mut world);
    turn(&mut world, &lead, "lead-turn", true);
    hold(&world, &[watching(&mono, "req-1", &lead)]);
    ran_a_turn(&mut world, &mono.chat);
    (world, mono, lead)
}

/// The Mono's own wake_missed opens four minutes (the threshold and its retry)
/// after the first pass that saw the stop, not before, and lists the request.
fn alarms_after_the_stop_and_its_retry(world: &mut World, stage: &str) {
    world.advance(4 * MINUTE - PASS);
    assert_eq!(episodes(world), Vec::new(), "{stage}: under four minutes");
    world.advance(3 * PASS);
    let [episode] = <[_; 1]>::try_from(episodes(world)).unwrap();
    assert_eq!(
        (
            episode.actor.as_str(),
            episode.provisioner.as_str(),
            episode.cause.as_str()
        ),
        ("mono:alb-mono", "mono:alb-general", "wake_missed"),
        "{stage}"
    );
    assert_eq!(listed_notices(world), ["req-1"], "{stage}");
}

/// The Mono's next turn closes the episode, and the row that stays bare never
/// opens another.
fn does_not_alarm_again(world: &mut World, mono: &Mono, stage: &str) {
    ran_a_turn(world, &mono.chat);
    world.advance(4 * MINUTE + 2 * PASS);
    let causes: Vec<String> = episodes(world)
        .into_iter()
        .map(|episode| episode.cause)
        .collect();
    assert_eq!(causes, ["wake_missed"], "{stage}: once");
}

/// A live window's normal end: the row takes its result in a notice, the Mono
/// chat's save holds the notice and settles the row, and a Mono turn takes it.
fn window_delivers(world: &mut World, owner: Owner, mono: &Mono, lead: &SessionId) {
    let notice = "mono-completion-req-1";
    let mut row = finished(mono, "req-1", lead);
    row["notice"] = json!(notice);
    mono_completions::hold(&world.conn(), owner, &[row]).unwrap();
    let item = json!({"id": notice, "text": "..", "appRequestId": notice, "waitForIdle": true});
    save_queue(world, &mono.chat, json!([item]), "active");
    let now = world.now();
    mono_completions::settle(&world.conn(), Some(mono.chat.as_str()), now).unwrap();
    turn(world, &mono.chat, "took-it", true);
    save_queue(world, &mono.chat, json!([]), "active");
    world.advance(MINUTE);
    turn(world, &mono.chat, "took-it", false);
}

// Defect (ADE-271): the window that holds the watcher closes while the session
// runs in another window, then the session stops. No window writes a result,
// so the row stays bare and the Mono is never told. A row that does not parse
// must not silence the alarm.
#[test]
fn a_session_that_stops_after_its_watcher_closed_alarms_once() {
    let (mut world, mono, lead) = monitoring_a_running_session("alarm-closed-watcher");
    world.advance(5 * MINUTE);
    assert_eq!(episodes(&world), Vec::new(), "still running");
    world
        .conn()
        .execute(
            "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
             VALUES ('req-bad', 'not json', 'main', 'run-1')",
            [],
        )
        .unwrap();

    turn(&mut world, &lead, "lead-turn", false);
    alarms_after_the_stop_and_its_retry(&mut world, "closed window");
    does_not_alarm_again(&mut world, &mono, "closed window");
}

const NEXT_RUN: Owner<'static> = Owner {
    window: "main",
    run: "run-2",
};

/// The app dies with the session's turn running, comes back, and the restorer
/// takes the bare row. `restore` keeps it bare while a Continue may pick the
/// session up, and writes it back (`closeInactive`).
fn relaunched(world: &mut World) {
    world.kill();
    world.restart();
    align(world);
    let now = world.now();
    let kept = mono_completions::claim(&world.conn(), NEXT_RUN, true, &["main"], now).unwrap();
    assert_eq!(kept.len(), 1, "the row survives the relaunch");
    mono_completions::hold(&world.conn(), NEXT_RUN, &kept).unwrap();
}

// Defect (ADE-271): a Continue after a quit is refused, so `resume` ignores its
// outcome and the restored row stays bare in a live window of this run. The
// relaunch reports the session as stopped by itself: no turn outlives the app.
#[test]
fn a_refused_continue_alarms_once() {
    let (mut world, mono, _lead) = monitoring_a_running_session("alarm-refused-continue");
    relaunched(&mut world);
    alarms_after_the_stop_and_its_retry(&mut world, "refused Continue");
    does_not_alarm_again(&mut world, &mono, "refused Continue");
}

// Defect (a false alarm, ADE-270's bug through ADE-271's rule): a relaunch
// reports every session as stopped until its Continue opens the turn, so the
// time before the Continue, or the running turn, alarms.
#[test]
fn a_continue_that_opens_its_turn_does_not_alarm() {
    let (mut world, mono, lead) = monitoring_a_running_session("alarm-continue-opens");
    relaunched(&mut world);
    world.advance(6 * PASS);
    turn(&mut world, &lead, "continue-turn", true);
    world.advance(5 * MINUTE);
    assert_eq!(episodes(&world), Vec::new(), "the Continue runs");

    turn(&mut world, &lead, "continue-turn", false);
    world.advance(2 * PASS);
    window_delivers(&mut world, NEXT_RUN, &mono, &lead);
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "the window delivered");
}

// Defect (a false alarm): the turn ends a moment before its live watcher writes
// the result, so the row is bare while the session is idle. It must not alarm
// however late inside the threshold the delivery lands.
#[test]
fn a_stop_its_live_watcher_delivers_does_not_alarm() {
    for (stage, wait) in [
        ("the next pass", PASS),
        ("just under four minutes", 4 * MINUTE - 2 * PASS),
    ] {
        let name = format!("alarm-live-watcher-{}", wait / PASS);
        let (mut world, mono, lead) = monitoring_a_running_session(&name);
        turn(&mut world, &lead, "lead-turn", false);
        world.advance(wait);
        assert_eq!(episodes(&world), Vec::new(), "{stage}: bare");
        window_delivers(&mut world, WINDOW, &mono, &lead);
        world.advance(4 * MINUTE + 2 * PASS);
        assert_eq!(episodes(&world), Vec::new(), "{stage}: delivered");
    }
}

// Defect (a false alarm): a row exists before anything stopped. A session whose
// child has spawned but that never ran a turn, or a request that waits in its
// session's paused queue, has a live watcher waiting. Another item in the queue,
// or a queue that does not parse, hides nothing.
#[test]
fn a_request_no_turn_took_yet_is_not_a_stop() {
    let mut world = World::new("alarm-not-started");
    world.add_mono_in("alb-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alb-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alb-lead");
    align(&mut world);
    let _child = claude_tap(&world, &lead);
    hold(&world, &[watching(&mono, "req-1", &lead)]);
    ran_a_turn(&mut world, &mono.chat);
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "no turn yet");

    ran_a_turn(&mut world, &lead);
    let own = json!({"id": "q-1", "text": "..", "appRequestId": "req-1"});
    save_queue(&world, &lead, json!([own]), "paused");
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(episodes(&world), Vec::new(), "queued");

    let other = json!({"id": "q-2", "text": "..", "appRequestId": "req-9"});
    save_queue(&world, &lead, json!([other]), "paused");
    alarms_after_the_stop_and_its_retry(&mut world, "another item queued");
}

// Defect: a queue that does not parse fails the whole pass, so no agent alarms.
#[test]
fn a_queue_that_does_not_parse_hides_no_stop() {
    let (mut world, _mono, lead) = monitoring_a_running_session("alarm-bad-queue");
    world
        .conn()
        .execute(
            "UPDATE sessions SET queued_messages_json = 'not json', queue_status = 'paused'
              WHERE id = ?1",
            [lead.as_str()],
        )
        .unwrap();
    turn(&mut world, &lead, "lead-turn", false);
    alarms_after_the_stop_and_its_retry(&mut world, "bad queue");
}

/// A closed episode as a build before #559 wrote it: every bare row counted, so
/// `request` is counted though its session still ran.
fn counted_before_this_build(world: &World, request: &str) {
    let listed = json!([{"kind": "notice", "id": request}]).to_string();
    world
        .conn()
        .execute(
            "INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause, open_work_json,
               stalled_since_active_ms, stalled_since_wall_ms, opened_active_ms, opened_wall_ms,
               closed_active_ms, closed_wall_ms, close_reason)
             VALUES ('al_old', 'mono:alb-mono', 'alb-mono', 'mono:alb-general', 'wake_missed', ?1,
               0, 0, 0, 0, 0, 0, 'progress')",
            [listed],
        )
        .unwrap();
}

/// The Mono's own episodes. The waits here take the lead's open ask past N, and
/// that lead's correct `idle_open_work` alarm is not what these scenarios count.
fn monos_episodes(world: &World) -> Vec<super::alarms_stall::Episode> {
    episodes(world)
        .into_iter()
        .filter(|episode| episode.actor == "mono:alb-mono")
        .collect()
}

fn listed_sorted(world: &World) -> Vec<String> {
    let mut listed = listed_notices(world);
    listed.sort();
    listed
}

// Decision (ADE-271, no migration): a row a build before #559 counted while its
// session ran stays silent when that session stops. Its effect ends there: a new
// request's row alarms once, and a bare row of a running session is no longer
// counted by an episode that closes around it, so it still alarms when it stops.
#[test]
fn a_row_counted_before_this_build_stays_silent_and_a_new_request_alarms_once() {
    let (mut world, mono, lead) = monitoring_a_running_session("alarm-counted-before");
    counted_before_this_build(&world, "req-1");
    turn(&mut world, &lead, "lead-turn", false);
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(
        monos_episodes(&world).len(),
        1,
        "the counted row stays silent"
    );

    let new_session = start_lead(&world, &mono, "alb-new");
    let still_running = start_lead(&world, &mono, "alb-running");
    turn(&mut world, &new_session, "new-turn", true);
    turn(&mut world, &still_running, "running-turn", true);
    let rows = [
        watching(&mono, "req-1", &lead),
        watching(&mono, "req-2", &new_session),
        watching(&mono, "req-3", &still_running),
    ];
    hold(&world, &rows);
    ran_a_turn(&mut world, &mono.chat);
    world.advance(2 * PASS);
    turn(&mut world, &new_session, "new-turn", false);
    world.advance(4 * MINUTE + 2 * PASS);
    let all = monos_episodes(&world);
    assert_eq!(all.len(), 2, "the new request alarms");
    assert!(all[1].open && all[1].cause == "wake_missed", "{all:?}");
    assert_eq!(
        listed_sorted(&world),
        ["req-1", "req-2"],
        "not the running row"
    );

    ran_a_turn(&mut world, &mono.chat);
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(
        monos_episodes(&world).len(),
        2,
        "the new request alarms once"
    );

    turn(&mut world, &still_running, "running-turn", false);
    world.advance(4 * MINUTE + 2 * PASS);
    assert_eq!(
        monos_episodes(&world).len(),
        3,
        "the running row was not counted"
    );
    assert_eq!(
        listed_sorted(&world),
        ["req-1", "req-2", "req-3"],
        "and it alarms for itself"
    );
}
