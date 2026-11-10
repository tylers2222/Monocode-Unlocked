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
/// How long an idle-type stall waits while turns leave its item (ADE-276).
const CEILING: i64 = 30 * MINUTE;
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
    // Review r2 M2 (turned_since): no turn since the stall began, so the alarm
    // says its own cause, not that the agent is taking turns.
    let text = first_prompt(&world);
    assert!(text.contains("a wake not taken"), "{text}");
    assert!(!text.contains("taking turns"), "{text}");

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

// Real turns move the stall by the detector's own rule (`wake_missed` counts
// from the turn end), so under the ceiling an agent whose turns keep running
// while one item stays stuck alarms once the turns stop, from the last turn's
// end, and then escalates.
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

// Defect: turns that come before the retry's wait ends, none taking the stuck
// item, hide it for ever: each turn end restarts wake_missed, and the retry
// and re-check read each turn as progress. Past the ceiling the alarm opens
// from the wake's first sight, says the agent is alive, stays open through
// more turns and reaches Tyler.
#[test]
fn turns_that_never_take_a_stuck_item_alarm_at_the_ceiling_from_its_start() {
    let mut world = World::new("alarm-retry-ceiling");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    world.advance(PASS);
    let seen: i64 = world
        .conn()
        .query_row(
            "SELECT wake_pending_since_active_ms FROM agent_activity WHERE session_id = ?1",
            [mono.chat.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let mut flip = 0;
    while episodes(&world).is_empty() {
        assert!(
            world.active_ms() - seen < CEILING + 2 * WAKE,
            "no alarm by the ceiling"
        );
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("flip-{flip}"));
        flip += 1;
    }
    let (since, opened, text): (i64, i64, String) = world
        .conn()
        .query_row(
            "SELECT e.stalled_since_active_ms, e.opened_active_ms, p.text
               FROM alarm_episodes e JOIN alarm_prompts p ON p.episode_id = e.id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(since, seen, "the alarm keeps the stall's original start");
    let gap = opened - since;
    assert!((CEILING..CEILING + WAKE).contains(&gap), "{gap}");
    assert!(
        text.contains("taking turns, but the item has not moved"),
        "{text}"
    );
    assert_eq!(sends(&world), [GIGA]);

    for more in 0..4 {
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("more-{more}"));
    }
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert!(episode.open, "a turn that leaves the item is no progress");
    assert_eq!(sends(&world), [GIGA, "tyler"]);

    // Review r2 M2 (still_stuck): the turn that takes the item ends it.
    save_queue(&world, &mono.chat, json!([]), "active");
    ran_a_turn_named(&mut world, &mono.chat, "takes-it");
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert!(!episode.open, "the item moved, so the alarm closes");
}

// Defect (mutation, item_moved): a ceiling wake_missed alarm stays open when
// a turn takes its stuck item and the next item waits behind it, as the wake
// never stopped waiting. The queue moved, so the alarm closes.
#[test]
fn a_ceiling_alarm_closes_when_its_queue_moves_on() {
    let mut world = World::new("alarm-retry-ceiling-moves");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    world.advance(PASS);
    let seen = wake_seen(&world, &mono.chat);
    let mut flip = 0;
    while episodes(&world).is_empty() {
        assert!(
            world.active_ms() - seen < CEILING + 2 * WAKE,
            "no alarm by the ceiling"
        );
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("flip-{flip}"));
        flip += 1;
    }
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(episode.cause, "wake_missed");
    assert!(episode.open);

    save_queue(&world, &mono.chat, json!([{"id": "next-1"}]), "active");
    ran_a_turn_named(&mut world, &mono.chat, "takes-stuck-1");
    world.advance(PASS);
    let first = episodes(&world).remove(0);
    assert_eq!(first.id, episode.id);
    assert!(!first.open, "the queue moved, so the alarm closes");
}

/// The text of the first alarm prompt written.
fn first_prompt(world: &World) -> String {
    world
        .conn()
        .query_row(
            "SELECT text FROM alarm_prompts ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

// Defect (review r2 M2, still_stuck): a wake_missed alarm under the ceiling
// stays open through a turn while its item waits, as only a ceiling alarm may.
#[test]
fn an_alarm_under_the_ceiling_closes_on_the_next_turn() {
    let mut world = World::new("alarm-retry-under-ceiling");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    world.advance(2 * WAKE + 2 * PASS);
    assert_eq!(sends(&world), [GIGA]);
    ran_a_turn_named(&mut world, &mono.chat, "a-turn");
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert!(!episode.open, "a turn is progress under the ceiling");
}

// Defect (review r2 M2, run_start): a wake that cleared and came back goes on
// the old run, so the ceiling fires early for the new one. A message has no
// item id, so only the run's start bounds it.
#[test]
fn a_wake_that_clears_and_comes_back_starts_a_new_run() {
    let (mut world, giga, mono) = message_waiting("alarm-retry-new-run");
    for flip in 0..5 {
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("old-{flip}"));
    }
    world.start_turn(&mono.chat, "takes-it");
    turn(&mut world, &mono.chat, "takes-it", true);
    world.advance(MINUTE);
    turn(&mut world, &mono.chat, "takes-it", false);
    world.end_turn(&mono.chat, "takes-it", true);
    world.advance(5 * MINUTE);
    assert_eq!(episodes(&world), Vec::new(), "the old run ended");

    align(&mut world);
    send(&world, &giga, &mono, "wake-2");
    world.advance(PASS);
    let seen = wake_seen(&world, &mono.chat);
    let mut flip = 0;
    while world.active_ms() - seen < CEILING - 2 * WAKE {
        world.advance(2 * WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("new-{flip}"));
        flip += 1;
    }
    assert_eq!(
        episodes(&world),
        Vec::new(),
        "no ceiling before the new run's"
    );
}

// Defect (review r2 M2, the turning line): a cause that is not idle-type says
// it is taking turns once its agent ran a turn, though the turn says nothing
// of it. A missing sleep hold stays open through turns, so its escalation is
// the alarm that is read after one.
#[test]
fn an_alarm_that_is_not_idle_type_never_says_taking_turns() {
    let mut world = World::new("alarm-retry-turning-words");
    let (_, mono) = add_monos(&world);
    let lead = start_lead(&world, &mono, "art-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    let since = world.active_ms();
    world
        .conn()
        .execute(
            "INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause, open_work_json,
               stalled_since_active_ms, stalled_since_wall_ms, opened_active_ms, opened_wall_ms,
               level, next_escalate_active_ms)
             VALUES ('al_hold', 'session:art-lead', 'art-mono', 'mono:art-mono', 'hold_missing',
               '[]', ?1, 1, ?1, 1, 1, ?2)",
            rusqlite::params![since, since + 2 * MINUTE],
        )
        .unwrap();
    ran_a_turn_named(&mut world, &lead, "after-it");
    world.advance(2 * MINUTE);
    let text: String = world
        .conn()
        .query_row(
            "SELECT text FROM alarm_prompts WHERE episode_id = 'al_hold'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(text.contains("sleep hold missing"), "{text}");
    assert!(!text.contains("taking turns"), "{text}");
}

// Review r2 probe P10: a stuck item with turns every 90 s alarms by the
// ceiling, from the item's first sight.
#[test]
fn a_stuck_item_with_turns_every_90_s_alarms_by_the_ceiling() {
    let mut world = World::new("alarm-retry-p10");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    world.advance(PASS);
    let seen = wake_seen(&world, &mono.chat);
    let mut flip = 0;
    while episodes(&world).is_empty() && world.active_ms() - seen < CEILING + N {
        world.advance(30 * SECOND);
        ran_a_turn_named(&mut world, &mono.chat, &format!("p10-{flip}"));
        flip += 1;
    }
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(episode.cause, "idle_open_work");
}

// Review r2 probe P11 and m2: a queue whose head item each turn takes, the next
// already queued, 3 idle minutes between turns, is waking, not stuck. No
// ceiling alarm says its item has not moved.
#[test]
fn a_queue_that_moves_every_turn_gets_no_ceiling_alarm() {
    let mut world = World::new("alarm-retry-p11");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    let items: Vec<String> = (0..40).map(|k| format!("item-{k}")).collect();
    save_queue(
        &world,
        &mono.chat,
        json!([{"id": items[0]}, {"id": items[1]}]),
        "active",
    );
    for k in 1..20 {
        world.advance(2 * WAKE - MINUTE);
        turn(&mut world, &mono.chat, &format!("take-{k}"), true);
        save_queue(
            &world,
            &mono.chat,
            json!([{"id": items[k]}, {"id": items[k + 1]}]),
            "active",
        );
        world.advance(MINUTE);
        turn(&mut world, &mono.chat, &format!("take-{k}"), false);
    }
    let texts: Vec<String> = world
        .conn()
        .prepare("SELECT text FROM alarm_prompts")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        !texts.iter().any(|text| text.contains("taking turns")),
        "{texts:?}"
    );
}

// Review r2 probes P8 and P9 (General's ruling B, open work): an agent with
// an open ask and turns 10 min or 90 s apart never stalled, as each turn end
// restarts idle_open_work. The ask's first sight is the ceiling's start, so the
// alarm opens from it while the turns go on.
#[test]
fn open_work_with_turns_10_min_or_90_s_apart_alarms_by_the_ceiling() {
    for gap in [9 * MINUTE, 30 * SECOND] {
        let mut world = World::new(&format!("alarm-retry-p8-{gap}"));
        let (_, mono) = add_monos(&world);
        let lead = start_lead(&world, &mono, "art-lead");
        align(&mut world);
        ran_a_turn(&mut world, &lead);
        let start = world.active_ms();
        let mut flip = 0;
        while episodes(&world).is_empty() && world.active_ms() - start < CEILING + N {
            world.advance(gap);
            ran_a_turn_named(&mut world, &lead, &format!("p8-{flip}"));
            flip += 1;
        }
        let all = episodes(&world);
        assert_eq!(all.len(), 1, "{gap}: one alarm by the ceiling");
        assert_eq!(all[0].cause, "idle_open_work", "{gap}");
        let since: i64 = world
            .conn()
            .query_row(
                "SELECT stalled_since_active_ms FROM alarm_episodes",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            since <= start,
            "{gap}: from the ask's first sight, not a turn end"
        );
        // Its turns go on and the ask stays open: the alarm stays open too.
        for more in 0..3 {
            world.advance(gap);
            ran_a_turn_named(&mut world, &lead, &format!("p8-more-{more}"));
        }
        let all = episodes(&world);
        assert_eq!(all.len(), 1, "{gap}: no new alarm");
        assert!(all[0].open, "{gap}: open while the ask waits");
    }
}

/// When `session`'s wake was first seen waiting.
fn wake_seen(world: &World, session: &SessionId) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT wake_pending_since_active_ms FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

// Defect (condition 3, every turn spacing): turns under the wake threshold
// apart never make wake_missed due, and each turn end restarts idle_open_work,
// so an item they never take waits for ever with nobody told. At the ceiling
// idle_open_work opens from the item's first sight, says the agent is alive,
// stays open through more turns and reaches Tyler.
#[test]
fn turns_under_two_minutes_apart_that_leave_an_item_alarm_at_the_ceiling() {
    let mut world = World::new("alarm-retry-ceiling-quick");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(&world, &mono.chat, json!([{"id": "stuck-1"}]), "active");
    world.advance(PASS);
    let seen = wake_seen(&world, &mono.chat);
    let mut flip = 0;
    while episodes(&world).is_empty() {
        assert!(
            world.active_ms() - seen < CEILING + 2 * MINUTE,
            "no alarm by the ceiling"
        );
        world.advance(WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("quick-{flip}"));
        flip += 1;
    }
    let (cause, since, opened, text): (String, i64, i64, String) = world
        .conn()
        .query_row(
            "SELECT e.cause, e.stalled_since_active_ms, e.opened_active_ms, p.text
               FROM alarm_episodes e JOIN alarm_prompts p ON p.episode_id = e.id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(cause, "idle_open_work");
    assert_eq!(since, seen, "the alarm keeps the item's first sight");
    let gap = opened - since;
    assert!((CEILING..CEILING + 2 * MINUTE).contains(&gap), "{gap}");
    assert!(
        text.contains("taking turns, but the item has not moved"),
        "{text}"
    );
    assert_eq!(sends(&world), [GIGA]);

    for more in 0..8 {
        world.advance(WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("more-{more}"));
    }
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert!(episode.open, "a turn that leaves the item is no progress");
    assert_eq!(sends(&world), [GIGA, "tyler"]);
}

// Control: a queue that drains under quick turns is no stuck item. Each turn
// takes one item, so the queue's wait restarts, though some item always waits.
#[test]
fn a_queue_that_drains_under_quick_turns_opens_no_alarm() {
    let mut world = World::new("alarm-retry-ceiling-drains");
    let (_, mono) = add_monos(&world);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    let items: Vec<String> = (0..40).map(|n| format!("item-{n}")).collect();
    for n in 0..items.len() - 1 {
        let left: Vec<_> = items[n..].iter().map(|id| json!({"id": id})).collect();
        save_queue(&world, &mono.chat, json!(left), "active");
        world.advance(WAKE - MINUTE);
        ran_a_turn_named(&mut world, &mono.chat, &format!("take-{n}"));
    }
    assert_eq!(episodes(&world), Vec::new());
}

// Upgrade (General's ruling): a database from a build before
// `first_stalled_since_active_ms` has retry rows without it. On the first
// start they read null, which starts no run: the agent's next stall gets its
// retry first, then one alarm from its first sight. Nothing opens at once, and
// the other agents' old rows open nothing.
#[test]
fn retry_rows_from_before_the_run_column_read_null_and_open_nothing_at_first_start() {
    let mut world = World::new("alarm-retry-upgrade");
    let (giga, mono) = add_monos(&world);
    align(&mut world);
    // Past the ceiling on the active clock, so a run read as starting at 0
    // would open at once.
    world.advance(CEILING);
    assert_eq!(episodes(&world), Vec::new());
    world
        .conn()
        .execute_batch(
            "DROP TABLE alarm_wake_items;
             DROP TABLE alarm_wake_retries;
             CREATE TABLE alarm_wake_retries (
               actor                   TEXT PRIMARY KEY,
               stalled_since_active_ms INTEGER NOT NULL,
               retried_active_ms       INTEGER NOT NULL
             );",
        )
        .unwrap();
    let old = [
        format!("mono:{}", mono.id.as_str()),
        GIGA.to_string(),
        "session:old-chat".to_string(),
    ];
    for (n, actor) in (1..).zip(&old) {
        world
            .conn()
            .execute(
                "INSERT INTO alarm_wake_retries VALUES (?1, ?2, ?2)",
                rusqlite::params![actor, n * MINUTE],
            )
            .unwrap();
    }
    world.kill();
    world.restart();
    assert_eq!(
        count(
            &world,
            "SELECT COUNT(*) FROM alarm_wake_retries WHERE first_stalled_since_active_ms IS NULL"
        ),
        3,
        "old rows read null"
    );

    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    send(&world, &giga, &mono, "wake-1");
    assert_eq!(
        named(&mut world, WAKE + PASS, &mono.chat),
        1,
        "the retry first"
    );
    assert_eq!(episodes(&world), Vec::new(), "no alarm at first start");
    world.advance(WAKE);
    assert_one_alarm_from_the_first_sight(&world, &mono.chat, WAKE);
    assert_eq!(sends(&world), [GIGA], "one alarm, no storm");
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
