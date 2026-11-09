//! ADE-276: a missed wake is retried once with no call to the provisioner,
//! and alarms only if no turn starts; no alarm opens for an agent the hub
//! already shows moving. No retry or re-check may hide a stall: each path that
//! holds an alarm back here ends in the alarm and its escalation to Tyler.

use std::collections::HashMap;

use serde_json::json;

use super::agents::Mono;
use super::alarms_stall::{
    align, claude_tap, episodes, ran_a_turn, save_queue, start_lead, turn, Episode, MINUTE, N, P,
    PASS, PERMISSION, SECOND,
};
use super::world::World;
use crate::alarms::taps::Pipe;
use crate::alarms::wake_retry::{moved_on, pass_before_record, Cause};
use crate::alarms::Sources;
use crate::mono_messages::{self, SessionId, Tool};
use crate::mono_roster::MonoRole;

/// The default wake threshold, which the retry also waits.
const WAKE: i64 = 2 * MINUTE;
/// The default escalation clock of a level.
const ESCALATE: i64 = 15 * MINUTE;
const GIGA: &str = "mono:art-general";
const ASSISTANT: &str = r#"{"type":"assistant"}"#;
const ANSWER: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{"behavior":"allow"}}}"#;

fn add_monos(world: &World) -> (Mono, Mono) {
    (
        world.add_mono_in("art-general", MonoRole::Giga, &[P]),
        world.add_mono_in("art-mono", MonoRole::Mono, &[P]),
    )
}

fn send(world: &World, giga: &Mono, mono: &Mono, request: &str) {
    let input = json!({
        "message": "Is CI green?", "send_to": mono.id.as_str(),
        "wanting_reply": false, "intent": "new",
    });
    world.call(giga, "g1", request, Tool::SendMessage, input, mono);
}

/// An idle Mono chat that ran a turn, with a message the next pass first sees
/// waiting. Active time sits on a pass, so the stall is due `WAKE + PASS` on.
fn message_waiting(name: &str) -> (World, Mono, Mono) {
    let mut world = World::new(name);
    let (giga, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    send(&world, &giga, &mono, "wake-1");
    (world, giga, mono)
}

/// `ms` of active time pass; how many ticks named `chat`.
fn named(world: &mut World, ms: i64, chat: &SessionId) -> usize {
    let mut count = 0;
    let mut left = ms;
    while left > 0 {
        let step = left.min(SECOND);
        let notices = world.try_advance(step).unwrap();
        let notices = serde_json::to_value(&notices).unwrap();
        if notices["sessions"]
            .as_array()
            .is_some_and(|sessions| sessions.iter().any(|name| name == chat.as_str()))
        {
            count += 1;
        }
        left -= step;
    }
    count
}

/// The messages the alarms put in the outbox.
fn told(world: &World) -> i64 {
    count(
        world,
        "SELECT COUNT(*) FROM mono_outbox WHERE stage = 'alarm'",
    )
}

fn count(world: &World, sql: &str) -> i64 {
    world.conn().query_row(sql, [], |row| row.get(0)).unwrap()
}

/// Who each level of the episodes went to, in order.
fn sends(world: &World) -> Vec<String> {
    world
        .conn()
        .prepare("SELECT recipient FROM alarm_sends ORDER BY created_active_ms, level")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The open wake_missed alarm of `world`'s one episode, and that it stalled
/// from when `session`'s wake was first seen, not from the retry or the open.
fn assert_one_alarm_from_the_first_sight(world: &World, session: &SessionId, retry_wait: i64) {
    let [episode] = <[Episode; 1]>::try_from(episodes(world)).unwrap();
    assert_eq!(episode.cause, "wake_missed");
    assert!(episode.open);
    let (since, opened): (i64, i64) = world
        .conn()
        .query_row(
            "SELECT stalled_since_active_ms, opened_active_ms FROM alarm_episodes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let seen: i64 = world
        .conn()
        .query_row(
            "SELECT wake_pending_since_active_ms FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(since, seen, "the alarm keeps the stall's start");
    assert_eq!(opened - since, WAKE + retry_wait);
}

/// One pass at the next slice, which took the hub before `record` landed: its
/// turn list was read before the record too, as the store lock's order has it.
fn pass_racing(world: &mut World, record: impl FnOnce(&World)) {
    let running: HashMap<String, String> = world.running();
    record(world);
    let (active, wall) = (world.active_ms() + PASS, world.now() + PASS);
    let store = world.store();
    let sources = Sources {
        live: &store.alarms,
        running: Some(&running),
    };
    let conn = store.lock_conn().unwrap();
    pass_before_record(&conn, active, wall, &sources).unwrap();
}

// Defect: the provisioner gets a model call for a wake the app clears itself,
// the retry writes a second copy of the item, or the finished item can run
// again.
#[test]
fn a_stuck_wake_the_retry_clears_opens_no_alarm() {
    let (mut world, _, mono) = message_waiting("alarm-retry-clears");
    assert_eq!(named(&mut world, WAKE, &mono.chat), 0, "not due yet");
    assert_eq!(named(&mut world, PASS, &mono.chat), 1, "the retry");
    assert_eq!(episodes(&world), Vec::new());
    assert_eq!(sends(&world), Vec::<String>::new());
    assert_eq!(
        count(&world, "SELECT COUNT(*) FROM mono_outbox"),
        0,
        "no prompt to anyone"
    );

    // The window reconciles the chat it was named, and its pump opens the turn.
    let waiting = mono_messages::unqueued(&world.conn(), &mono.chat).unwrap();
    assert_eq!(waiting.len(), 1, "the retry wrote no second row");
    let handed = world.start_turn(&mono.chat, "retried");
    assert_eq!(handed.len(), 1);
    turn(&mut world, &mono.chat, "retried", true);
    world.advance(MINUTE);
    turn(&mut world, &mono.chat, "retried", false);
    world.end_turn(&mono.chat, "retried", true);
    world.advance(N);

    assert_eq!(episodes(&world), Vec::new(), "the retry cleared it");
    assert_eq!(sends(&world), Vec::<String>::new());
    let waiting = mono_messages::unqueued(&world.conn(), &mono.chat).unwrap();
    assert_eq!(waiting.len(), 0, "a finished item does not run again");
}

// Defect: a retry that starts no turn goes silent, alarms before its wait,
// retries each pass, loses the stall's start, or the alarm stops short of Tyler.
#[test]
fn a_retry_that_starts_no_turn_opens_one_alarm_and_does_not_retry_again() {
    let (mut world, _, mono) = message_waiting("alarm-retry-no-turn");
    assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "the retry");
    assert_eq!(named(&mut world, WAKE - PASS, &mono.chat), 0);
    assert_eq!(episodes(&world), Vec::new(), "the retry still waits");

    assert_eq!(named(&mut world, PASS, &mono.chat), 0);
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    assert_eq!(sends(&world), [GIGA]);

    assert_eq!(
        named(&mut world, ESCALATE, &mono.chat),
        0,
        "no second retry"
    );
    assert_eq!(episodes(&world).len(), 1, "one alarm");
    assert_eq!(sends(&world), [GIGA, "tyler"], "it reaches Tyler");
}

// Defect: the retry mark lives in memory, so a restart retries the stall again
// and pushes its alarm back each time.
#[test]
fn a_restart_does_not_retry_the_same_stall_again() {
    let (mut world, _, mono) = message_waiting("alarm-retry-restart");
    assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "the retry");
    world.kill();
    world.restart();
    assert_eq!(named(&mut world, WAKE + 2 * PASS, &mono.chat), 0);
    assert_eq!(episodes(&world).len(), 1, "the alarm after the restart");
    assert_eq!(sends(&world), [GIGA]);
}

// Defect: "already retried" is kept per agent, so an agent's later stall gets
// no retry and alarms its provisioner at once.
#[test]
fn a_new_stall_gets_a_new_retry() {
    let (mut world, giga, mono) = message_waiting("alarm-retry-new-stall");
    assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "first stall");
    world.start_turn(&mono.chat, "retried");
    turn(&mut world, &mono.chat, "retried", true);
    world.advance(MINUTE);
    turn(&mut world, &mono.chat, "retried", false);
    world.end_turn(&mono.chat, "retried", true);
    world.advance(N);
    assert_eq!(episodes(&world), Vec::new());

    align(&mut world);
    send(&world, &giga, &mono, "wake-2");
    assert_eq!(
        named(&mut world, WAKE + PASS, &mono.chat),
        1,
        "second stall"
    );
    assert_eq!(episodes(&world), Vec::new(), "its own retry first");
    world.advance(WAKE);
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
}

// Defect: a retry that errors holds the alarm back, so the stall goes silent.
#[test]
fn a_retry_that_errors_still_alarms() {
    let (mut world, _, mono) = message_waiting("alarm-retry-error");
    world
        .conn()
        .execute_batch("DROP TABLE alarm_wake_retries")
        .unwrap();
    assert_eq!(
        named(&mut world, WAKE + PASS, &mono.chat),
        0,
        "no retry ran"
    );
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, 0);
    assert_eq!(sends(&world), [GIGA]);
    assert_eq!(world.store().alarms.last_fault(), None);
}

// Defect (review M1): a removed worktree with items queued goes silent. The
// window saves that queue paused, so a row with only `worktree_removed` set
// hides it.
#[test]
fn a_removed_worktree_with_a_queue_still_alarms() {
    let (mut world, _, mono) = message_waiting("alarm-retry-no-start");
    save_queue(&world, &mono.chat, json!([{"id": "left-1"}]), "paused");
    world
        .conn()
        .execute(
            "UPDATE sessions SET worktree_removed = 1 WHERE id = ?1",
            [mono.chat.as_str()],
        )
        .unwrap();
    world.advance(2 * WAKE + PASS);
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    assert_eq!(sends(&world), [GIGA]);
}

/// What `session.error` leaves after the retried turn: the rest of the queue
/// paused, with reason `error`, or with the failed item's own error.
fn the_provider_failed_the_turn(world: &mut World, chat: &SessionId, items: serde_json::Value) {
    world.start_turn(chat, "retried");
    turn(world, chat, "retried", true);
    world.advance(PASS);
    save_queue(world, chat, items, "paused");
    turn(world, chat, "retried", false);
    world.end_turn(chat, "retried", true);
}

// Defect (review B1): the retry's turn fails, the window pauses what is left,
// and the detector reads that as a pause on purpose: no wake, no alarm, nobody
// told, where the old code alarmed at 2 minutes.
#[test]
fn a_queue_a_failed_turn_paused_still_alarms_and_escalates() {
    for (stage, reason, items) in [
        ("error pause", Some("error"), json!([{"id": "left-1"}])),
        (
            "failed item",
            None,
            json!([{"id": "left-1", "error": "no"}]),
        ),
    ] {
        let name = format!("alarm-retry-failed-{}", stage.replace(' ', "-"));
        let (mut world, _, mono) = message_waiting(&name);
        assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "{stage}");
        the_provider_failed_the_turn(&mut world, &mono.chat, items);
        world
            .conn()
            .execute(
                "UPDATE sessions SET queue_pause = ?2 WHERE id = ?1",
                rusqlite::params![mono.chat.as_str(), reason],
            )
            .unwrap();
        assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "{stage}");
        assert_eq!(episodes(&world), Vec::new(), "{stage}: its own retry first");
        world.advance(WAKE);
        assert_eq!(sends(&world), [GIGA], "{stage}");
        world.advance(ESCALATE);
        assert_eq!(sends(&world), [GIGA, "tyler"], "{stage}");
    }
}

// Limit, by ruling: a queue the user stopped waits on purpose and opens no
// alarm, even with items in it. An agent with open work still alarms at N.
#[test]
fn a_queue_the_user_stopped_opens_no_alarm() {
    let (mut world, _, mono) = message_waiting("alarm-retry-user-stop");
    save_queue(&world, &mono.chat, json!([{"id": "left-1"}]), "paused");
    world.advance(4 * N);
    assert_eq!(episodes(&world), Vec::new());
}

// Defect: the detector cannot see a chat with no activity row, and a queue the
// window paused keeps it from getting one (`unseen` repeats the paused rule).
#[test]
fn a_chat_with_no_row_and_an_error_paused_queue_still_alarms() {
    let mut world = World::new("alarm-retry-unseen-error");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    save_queue(&world, &mono.chat, json!([{"id": "left-1"}]), "paused");
    world
        .conn()
        .execute(
            "UPDATE sessions SET queue_pause = 'error' WHERE id = ?1",
            [mono.chat.as_str()],
        )
        .unwrap();
    world.advance(2 * WAKE + 2 * PASS);
    assert_eq!(sends(&world), [GIGA]);
}

// Defect: an agent whose turn started just before the send still alarms its
// provisioner, which then pays to read a stall that is over.
#[test]
fn an_agent_that_goes_busy_before_the_send_gets_no_alarm() {
    let (mut world, _, mono) = message_waiting("alarm-retry-busy");
    world.advance(2 * WAKE);
    assert_eq!(episodes(&world), Vec::new(), "the next pass is the alarm");

    pass_racing(&mut world, |world| {
        world.store().alarms.turn(
            mono.chat.as_str(),
            "just-started",
            crate::alarms::Edge::Started,
        );
    });
    assert_eq!(episodes(&world), Vec::new(), "dropped before the send");
    assert_eq!(sends(&world), Vec::<String>::new());
    assert_eq!(told(&world), 0);
    world.advance(PASS);
    assert_eq!(episodes(&world), Vec::new(), "its turn runs");
}

// Defect: a dropped alarm ends detection: the record that dropped it was lost,
// no turn ever ran, and the agent stays stalled with nobody told.
#[test]
fn a_dropped_alarm_comes_back_and_escalates_when_the_agent_is_still_stalled() {
    let (mut world, _, mono) = message_waiting("alarm-retry-comes-back");
    world.advance(2 * WAKE);
    pass_racing(&mut world, |world| {
        world.store().alarms.turn(
            mono.chat.as_str(),
            "never-ran",
            crate::alarms::Edge::Started,
        );
    });
    assert_eq!(episodes(&world), Vec::new(), "dropped");
    world.store().alarms.take();

    assert_eq!(named(&mut world, PASS, &mono.chat), 0, "no second retry");
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    assert_eq!(sends(&world), [GIGA]);
    world.advance(ESCALATE);
    assert_eq!(sends(&world), [GIGA, "tyler"]);
}

// Defect: repeated drops silence a stall for ever (the orchestrator's third
// condition). A drop keeps no state, so however many pass while the rows do not
// move, the next pass with nothing new opens from the original start.
#[test]
fn repeated_drops_on_a_stall_that_never_moves_still_open_from_its_start() {
    let (mut world, _, mono) = message_waiting("alarm-retry-repeated-drops");
    world.advance(2 * WAKE);
    for flip in 0..5 {
        pass_racing(&mut world, |world| {
            world.store().alarms.turn(
                mono.chat.as_str(),
                &format!("flip-{flip}"),
                crate::alarms::Edge::Started,
            );
        });
        assert_eq!(episodes(&world), Vec::new(), "dropped {flip}");
        world.store().alarms.take();
    }
    world.advance(PASS);
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    world.advance(ESCALATE);
    assert_eq!(sends(&world), [GIGA, "tyler"]);
}

// Limit, stated in the PR body: real turns move the stall by the detector's own
// rule (`wake_missed` counts from the turn end), so an agent whose turns keep
// running while one item stays stuck alarms only once the turns stop, from the
// last turn's end, and then escalates.
#[test]
fn an_agent_that_flips_busy_and_idle_alarms_from_its_last_turn_end() {
    let mut world = World::new("alarm-retry-flips");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    for flip in 0..3 {
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("flip-{flip}"));
        assert_eq!(episodes(&world), Vec::new(), "turn {flip} moved the stall");
    }
    world.advance(2 * WAKE + 2 * PASS);
    assert_eq!(sends(&world), [GIGA], "the item alarms once the turns stop");
    world.advance(ESCALATE);
    assert_eq!(sends(&world), [GIGA, "tyler"]);
}

fn ran_a_turn_named(world: &mut World, session: &SessionId, name: &str) {
    turn(world, session, name, true);
    world.advance(MINUTE);
    turn(world, session, name, false);
}

// The climb after a drop reaches each provisioner on its own clock.
#[test]
fn a_dropped_alarm_escalates_to_each_next_provisioner_on_schedule() {
    let mut world = World::new("alarm-recheck-chain");
    let (_, mono) = add_monos(&world);
    let lead = start_lead(&world, &mono, "art-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(N - PASS);
    pass_racing(&mut world, |world| {
        world
            .store()
            .alarms
            .turn(lead.as_str(), "never-ran", crate::alarms::Edge::Started);
    });
    world.store().alarms.take();
    assert_eq!(episodes(&world), Vec::new(), "dropped");

    world.advance(PASS);
    assert_eq!(sends(&world), ["mono:art-mono"]);
    world.advance(ESCALATE);
    assert_eq!(sends(&world), ["mono:art-mono", GIGA]);
    world.advance(ESCALATE);
    assert_eq!(sends(&world), ["mono:art-mono", GIGA, "tyler"]);
}

// Defect: a dropped alarm uses up its stuck item: the turn that dropped it ran
// without taking the item, and each item counts toward one episode only, so
// the item never alarms again.
#[test]
fn an_item_a_quick_turn_left_behind_still_alarms_after_a_drop() {
    let mut world = World::new("alarm-retry-left-behind");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(
        &world,
        &mono.chat,
        json!([{"id": "stuck-1", "text": "The lead finished"}]),
        "active",
    );
    world.advance(2 * WAKE);
    pass_racing(&mut world, |world| {
        world
            .store()
            .alarms
            .turn(mono.chat.as_str(), "quick", crate::alarms::Edge::Started);
    });
    assert_eq!(episodes(&world), Vec::new(), "dropped");
    turn(&mut world, &mono.chat, "quick", false);
    world.advance(PASS);

    assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "a new retry");
    assert_eq!(episodes(&world), Vec::new());
    world.advance(WAKE);
    assert_eq!(episodes(&world).len(), 1, "the item alarms");
    assert_eq!(sends(&world), [GIGA]);
}

// Defect: the re-check drops a dialog alarm because the agent is busy (a
// dialog's turn runs), or sends one for a dialog already answered.
#[test]
fn a_dialog_alarm_drops_only_when_the_dialog_was_answered() {
    for (line, pipe, alarms) in [(ASSISTANT, Pipe::Stdout, 1), (ANSWER, Pipe::Stdin, 0)] {
        let mut world = World::new(&format!("alarm-recheck-dialog-{alarms}"));
        let (_, mono) = add_monos(&world);
        let lead = start_lead(&world, &mono, "art-lead");
        let claude = claude_tap(&world, &lead);
        align(&mut world);
        turn(&mut world, &lead, "t1", true);
        claude.line(Pipe::Stdout, PERMISSION);
        world.advance(3 * MINUTE - PASS);
        assert_eq!(episodes(&world), Vec::new());

        pass_racing(&mut world, |world| {
            claude.line(pipe, line);
            assert!(
                !world.store().alarms.pending(&lead).is_empty(),
                "the tap recorded {line}"
            );
        });
        assert_eq!(told(&world), alarms, "{line}");
        assert_eq!(episodes(&world).len(), alarms as usize, "{line}");
    }
}

// Defect: the re-check covers only a missed wake, so a hung turn that just
// printed, or an idle agent whose turn just started, still alarms; or it drops a
// hung agent for being busy, which is the stall: a record that moved nothing
// must not drop it.
#[test]
fn the_re_check_drops_a_hung_turn_only_if_it_moved_and_covers_idle_open_work() {
    for (stage, moved) in [("it just printed", true), ("nothing moved", false)] {
        let mut world = World::new(&format!("alarm-recheck-hung-{moved}"));
        let (_, mono) = add_monos(&world);
        let lead = start_lead(&world, &mono, "art-lead");
        let claude = claude_tap(&world, &lead);
        align(&mut world);
        turn(&mut world, &lead, "t1", true);
        claude.line(Pipe::Stdout, ASSISTANT);
        world.advance(N - PASS);
        assert_eq!(episodes(&world), Vec::new());
        pass_racing(&mut world, |world| {
            if moved {
                claude.line(Pipe::Stdout, ASSISTANT);
            } else {
                // The same turn's start again moves nothing.
                world
                    .store()
                    .alarms
                    .turn(lead.as_str(), "t1", crate::alarms::Edge::Started);
            }
            assert!(!world.store().alarms.pending(&lead).is_empty(), "{stage}");
        });
        let causes: Vec<String> = episodes(&world)
            .into_iter()
            .map(|episode| episode.cause)
            .collect();
        let expected: &[&str] = if moved { &[] } else { &["hung"] };
        assert_eq!(causes, expected, "{stage}");
    }

    let mut world = World::new("alarm-recheck-idle");
    let (_, mono) = add_monos(&world);
    let lead = start_lead(&world, &mono, "art-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(N - PASS);
    assert_eq!(episodes(&world), Vec::new());
    pass_racing(&mut world, |world| {
        world
            .store()
            .alarms
            .turn(lead.as_str(), "t2", crate::alarms::Edge::Started);
    });
    assert_eq!(episodes(&world), Vec::new(), "its turn just started");
}

// Defect: the re-check drops a `hold_missing` alarm because a turn started,
// though no turn says the sleep hold was fixed, or a stall cause stops dropping.
// The detector opens `hold_missing` only from #546 on, so this calls the re-check
// on one hub record for each cause. A stored unknown cause never reaches it: only
// `next_state`'s open does, and that carries a cause this build knows.
#[test]
fn the_re_check_drops_only_a_known_stall_cause() {
    let mut world = World::new("alarm-recheck-causes");
    let (_, mono) = add_monos(&world);
    let lead = start_lead(&world, &mono, "art-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(PASS);
    let since = world.active_ms();
    world.advance(MINUTE);
    let store = world.store();
    store
        .alarms
        .turn(lead.as_str(), "t2", crate::alarms::Edge::Started);
    let now = (world.active_ms(), world.now());
    let conn = store.lock_conn().unwrap();
    for (cause, drops) in [
        (Cause::IdleOpenWork, true),
        (Cause::WakeMissed, true),
        (Cause::HoldMissing, false),
    ] {
        assert_eq!(
            moved_on(&conn, &store.alarms, &lead, (cause, since), now),
            drops,
            "{cause:?}"
        );
    }
}

// Defect: a pass with nothing new in the hub drops the alarm anyway, so the
// re-check hides a stall the rows still show.
#[test]
fn nothing_new_in_the_hub_drops_nothing() {
    let (mut world, _, mono) = message_waiting("alarm-recheck-nothing");
    world.advance(2 * WAKE);
    pass_racing(&mut world, |_| {});
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    assert_eq!(sends(&world), [GIGA]);
    assert_eq!(told(&world), 1);
}

// Ruling (orchestrator, m_sujg5vuyiwp7): a live agent that cannot start a turn
// alarms; an archived one does not, as someone closed it on purpose. The backend
// cannot see a usage limit or a dead provider: each reads as no turn in the wait,
// and a limit hit with items queued as a queue paused by the error.
#[test]
fn a_limited_or_dead_provider_session_alarms_and_an_archived_one_does_not() {
    for (stage, sql, alarms) in [
        ("usage limit, nothing queued", None, true),
        (
            "usage limit, items queued",
            Some("queue_status = 'paused', queue_pause = 'error'"),
            true,
        ),
        ("dead provider", None, true),
        ("archived", Some("archived = 1"), false),
    ] {
        let name = format!("alarm-retry-ruling-{}", stage.replace([' ', ','], "-"));
        let (mut world, _, mono) = message_waiting(&name);
        if let Some(sql) = sql {
            world
                .conn()
                .execute(
                    &format!("UPDATE sessions SET {sql} WHERE id = ?1"),
                    [mono.chat.as_str()],
                )
                .unwrap();
        }
        world.advance(2 * WAKE + 2 * PASS);
        let expected: &[&str] = if alarms { &[GIGA] } else { &[] };
        assert_eq!(sends(&world), expected, "{stage}");
    }
}

// Limit, as the rest of the detector rules it (`next_state`): an archived
// session is gone on purpose, and a gone agent opens nothing. This test holds
// that the retry changes neither way, and that the activity row goes with the
// agent. Its retry row stays: a returning agent has a new stalled-since.
#[test]
fn an_agent_archived_during_the_wait_keeps_the_gone_rule() {
    let (mut world, _, mono) = message_waiting("alarm-retry-gone");
    assert_eq!(named(&mut world, WAKE + PASS, &mono.chat), 1, "the retry");
    world
        .conn()
        .execute(
            "UPDATE sessions SET archived = 1 WHERE id = ?1",
            [mono.chat.as_str()],
        )
        .unwrap();
    assert_eq!(named(&mut world, N, &mono.chat), 0);
    assert_eq!(episodes(&world), Vec::new());
    assert_eq!(count(&world, "SELECT COUNT(*) FROM agent_activity"), 0);
}
