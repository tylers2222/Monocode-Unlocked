//! The stall detector judges only sessions with an `agent_activity` row, and
//! a row comes only from a turn or a tapped child on a build with the
//! detector. So a Mono chat that ran no turn since the detector shipped is
//! never judged, whatever waits for it. Each scenario places its work before
//! the ship, as the old build left it, and no turn runs on the new build
//! unless the scenario says so.

use serde_json::json;

use super::agents::{mono_caller, Mono};
use super::alarms_stall::{
    align, episodes, ran_a_turn, save_queue, start_lead, Episode, MINUTE, N, P, PASS,
};
use super::world::World;
use crate::alarms::test_taps;
use crate::mono_messages::{SessionId, Tool};
use crate::mono_roster::MonoRole;

/// Past the wake threshold and whatever retry a missed wake gets first.
const PAST_WAKE: i64 = 5 * MINUTE;

fn fleet(name: &str) -> (World, Mono, Mono) {
    let mut world = World::new(name);
    let giga = world.add_mono_in("alu-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alu-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    (world, giga, mono)
}

/// The app restarts on a build with the detector.
fn ship(world: &mut World) {
    world.kill();
    world.restart();
    align(world);
}

fn has_row(world: &World, session: &SessionId) -> bool {
    test_taps::row(&world.conn(), session.as_str()).is_some()
}

/// `giga` sends `mono` a message no provider has taken.
fn send(world: &World, giga: &Mono, mono: &Mono, request: &str) {
    let input = json!({
        "message": "Is CI green?", "send_to": mono.id.as_str(),
        "wanting_reply": false, "intent": "new",
    });
    world.call(giga, "g1", request, Tool::SendMessage, input, mono);
}

/// A turn on the old build took what waited for `session`: it wrote no row.
fn old_build_turn(world: &mut World, session: &SessionId) {
    world.start_turn(session, "old-turn");
    world.end_turn(session, "old-turn", true);
}

/// `giga` asks `mono`, with no reminder due in a scenario, and an old build's
/// turn takes the ask, which stays open.
fn stale_ask(world: &mut World, giga: &Mono, mono: &Mono) {
    let input = json!({
        "message": "Build it", "send_to": mono.id.as_str(), "due_minutes": 480,
    });
    world.tool(
        &mono_caller(giga, "g1"),
        "stale-ask",
        "ask",
        input,
        &[&mono.chat],
    );
    old_build_turn(world, &mono.chat);
}

fn the_episode(world: &World) -> Episode {
    let [episode] = <[Episode; 1]>::try_from(episodes(world)).unwrap();
    episode
}

fn close_reason(world: &World, episode: &str) -> Option<String> {
    world
        .conn()
        .query_row(
            "SELECT close_reason FROM alarm_episodes WHERE id = ?1",
            [episode],
            |row| row.get(0),
        )
        .unwrap()
}

/// Everyone `episode` was sent to, in level order.
fn recipients(world: &World, episode: &str) -> Vec<String> {
    world
        .conn()
        .prepare("SELECT recipient FROM alarm_sends WHERE episode_id = ?1 ORDER BY level")
        .unwrap()
        .query_map([episode], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The kinds of what `episode` lists.
fn listed_kinds(world: &World, episode: &str) -> Vec<String> {
    world
        .conn()
        .prepare(
            "SELECT DISTINCT json_extract(w.value, '$.kind')
               FROM alarm_episodes e, json_each(e.open_work_json) w
              WHERE e.id = ?1 ORDER BY 1",
        )
        .unwrap()
        .query_map([episode], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// Defect (9 Nov): a chat sat idle with 7 queued messages after the detector
// shipped, and with no row it was never judged. Or the alarm opens and never
// reaches Tyler, or it outlives the turn that takes the work, or it repeats.
#[test]
fn queued_items_and_no_row_after_a_ship_alarm_and_reach_tyler() {
    let (mut world, _, mono) = fleet("alarm-unseen-queued");
    let items: Vec<_> = (1..=7).map(|n| json!({"id": format!("q-{n}")})).collect();
    save_queue(&world, &mono.chat, json!(items), "active");
    ship(&mut world);

    world.advance(2 * MINUTE - PASS);
    assert_eq!(episodes(&world), Vec::new(), "under the wake threshold");
    world.advance(PAST_WAKE);
    let episode = the_episode(&world);
    assert_eq!(
        (
            episode.actor.as_str(),
            episode.provisioner.as_str(),
            episode.cause.as_str(),
            episode.open
        ),
        ("mono:alu-mono", "mono:alu-general", "wake_missed", true)
    );
    assert_eq!(listed_kinds(&world, &episode.id), ["queued"]);

    world.advance(3 * N);
    let chain = recipients(&world, &episode.id);
    assert_eq!(chain.first().map(String::as_str), Some("mono:alu-general"));
    assert_eq!(
        chain.last().map(String::as_str),
        Some("tyler"),
        "an open alarm escalates"
    );

    ran_a_turn(&mut world, &mono.chat);
    world.advance(N);
    assert_eq!(
        close_reason(&world, &episode.id).as_deref(),
        Some("progress")
    );
    assert_eq!(episodes(&world).len(), 1, "the items alarmed once");
}

// Defect: a message no provider took, in a chat with no row, is never judged.
#[test]
fn undelivered_work_and_no_row_after_a_ship_alarms() {
    let (mut world, giga, mono) = fleet("alarm-unseen-undelivered");
    send(&world, &giga, &mono, "undelivered-1");
    ship(&mut world);

    world.advance(PAST_WAKE);
    let episode = the_episode(&world);
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("mono:alu-mono", "wake_missed")
    );
    assert_eq!(listed_kinds(&world, &episode.id), ["message"]);
    assert!(!has_row(&world, &giga.chat), "only the chat with work");
}

// Defect: the two other wakes, a completion result and a due reminder, are
// not read for a chat with no row, though `wake_missed` counts them once it
// has one. A launch releases every claim, so a reminder waits from a claim
// after the ship.
#[test]
fn a_result_or_a_reminder_and_no_row_after_a_ship_alarm() {
    let (mut world, _, mono) = fleet("alarm-unseen-notice");
    let saved = json!({
        "request": {"requestId": "req-1"}, "origin": {"monoId": mono.chat.as_str()},
        "result": {"status": "completed"}, "notice": "notice-1",
    });
    world
        .conn()
        .execute(
            "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
             VALUES ('req-1', ?1, 'closed', 'old-run')",
            [saved.to_string()],
        )
        .unwrap();
    ship(&mut world);
    world.advance(PAST_WAKE);
    assert_eq!(the_episode(&world).cause, "wake_missed", "notice");

    for prefix in ["mono", "session"] {
        let (mut world, _, mono) = fleet(&format!("alarm-unseen-reminder-{prefix}"));
        ship(&mut world);
        let id = match prefix {
            "mono" => mono.id.as_str(),
            _ => mono.chat.as_str(),
        };
        world
            .conn()
            .execute(
                "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms, claimed_by)
                 VALUES ('resume:alu:1', 'resume', ?1, 1, 'main')",
                [format!("{prefix}:{id}")],
            )
            .unwrap();
        world.advance(PAST_WAKE);
        assert_eq!(the_episode(&world).cause, "wake_missed", "{prefix}");
    }
}

// Defect: a Giga's chat is left out, so the top of a tree stalls unseen.
#[test]
fn a_giga_chat_with_queued_items_and_no_row_alarms_tyler() {
    let (mut world, giga, _) = fleet("alarm-unseen-giga");
    save_queue(&world, &giga.chat, json!([{"id": "q-1"}]), "active");
    ship(&mut world);
    world.advance(PAST_WAKE);
    let episode = the_episode(&world);
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("mono:alu-general", "wake_missed")
    );
    world.advance(N);
    assert_eq!(
        recipients(&world, &episode.id).last().map(String::as_str),
        Some("tyler")
    );
}

// Defect: every old chat gets a row on the first pass. Nothing alarms for a
// chat with no work, but it becomes a watched agent for no reason, on every
// data folder, at once.
#[test]
fn an_old_chat_with_no_open_work_and_no_row_stays_silent() {
    let (mut world, giga, mono) = fleet("alarm-unseen-silent");
    send(&world, &giga, &mono, "old-message");
    old_build_turn(&mut world, &mono.chat);
    ship(&mut world);

    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new());
    assert!(!has_row(&world, &mono.chat), "no work, no row");
    assert!(!has_row(&world, &giga.chat), "no work, no row");
}

// Defect (the flood): an open ask alone seeds a row, so every old chat with a
// stale ask alarms N after a ship. The ask is not forgotten: once the chat has
// a wake or runs a turn it is judged as any watched chat, and the episode lists
// the ask.
#[test]
fn a_stale_open_ask_alone_stays_silent_until_the_chat_has_a_wake() {
    let (mut world, giga, mono) = fleet("alarm-unseen-stale-ask");
    stale_ask(&mut world, &giga, &mono);
    ship(&mut world);

    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new(), "the ask alone");
    assert!(!has_row(&world, &mono.chat), "the ask alone");

    send(&world, &giga, &mono, "new-work");
    world.advance(PAST_WAKE);
    let episode = the_episode(&world);
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("mono:alu-mono", "wake_missed")
    );
    assert_eq!(listed_kinds(&world, &episode.id), ["ask", "message"]);
}

// Defect: the chat's alarm outlives its session: an episode for a chat archived
// or reset stays open and climbs for ever, or the chat is seeded again.
#[test]
fn a_chat_that_goes_or_resets_closes_its_alarm_and_stays_unseen() {
    for reset in [false, true] {
        let (mut world, _, mono) = fleet(&format!("alarm-unseen-gone-{reset}"));
        save_queue(&world, &mono.chat, json!([{"id": "q-1"}]), "active");
        ship(&mut world);
        world.advance(PAST_WAKE);
        let episode = the_episode(&world);

        let chat = if reset {
            world.reset_mono_chat(&mono, "alu-mono-chat-2").chat
        } else {
            world
                .conn()
                .execute(
                    "UPDATE sessions SET archived = 1 WHERE id = ?1",
                    [mono.chat.as_str()],
                )
                .unwrap();
            mono.chat.clone()
        };
        world.advance(PASS);
        assert_eq!(
            close_reason(&world, &episode.id).as_deref(),
            Some("agent_gone"),
            "reset: {reset}"
        );
        world.advance(2 * N);
        assert_eq!(episodes(&world).len(), 1, "reset: {reset}: no new alarm");
        assert!(!has_row(&world, &chat), "reset: {reset}: nothing to judge");
        assert!(!has_row(&world, &mono.chat), "reset: {reset}: forgotten");
    }
}

// Defect: a chat archived before its first pass, or one the user paused, is
// seeded, though `wakes` says a paused queue waits on purpose. Resuming it must
// still alarm.
#[test]
fn a_chat_archived_or_paused_before_its_first_pass_gets_no_row() {
    let (mut world, _, mono) = fleet("alarm-unseen-archived");
    save_queue(&world, &mono.chat, json!([{"id": "q-1"}]), "active");
    world
        .conn()
        .execute(
            "UPDATE sessions SET archived = 1 WHERE id = ?1",
            [mono.chat.as_str()],
        )
        .unwrap();
    ship(&mut world);
    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new(), "archived");
    assert!(!has_row(&world, &mono.chat), "archived");

    let (mut world, _, mono) = fleet("alarm-unseen-paused");
    save_queue(&world, &mono.chat, json!([{"id": "q-1"}]), "paused");
    ship(&mut world);
    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new(), "paused");
    assert!(!has_row(&world, &mono.chat), "paused");

    save_queue(&world, &mono.chat, json!([{"id": "q-1"}]), "active");
    world.advance(PAST_WAKE);
    assert_eq!(the_episode(&world).cause, "wake_missed", "resumed");
}

// Defect: the alarms' own queue item seeds its recipient, so one stall climbs;
// or a queue that does not parse fails the seed, and another chat's work goes
// unseen with it.
#[test]
fn an_alarm_item_or_a_bad_queue_seeds_nothing_and_hides_no_other_chat() {
    let (mut world, giga, mono) = fleet("alarm-unseen-not-work");
    let other = world.add_mono_in("alu-other", MonoRole::Mono, &[P]);
    save_queue(
        &world,
        &mono.chat,
        json!([{"id": "alarm-al_x-1"}]),
        "active",
    );
    save_queue(&world, &other.chat, json!([{"id": "q-1"}]), "active");
    world
        .conn()
        .execute(
            "UPDATE sessions SET queued_messages_json = 'not json' WHERE id = ?1",
            [giga.chat.as_str()],
        )
        .unwrap();
    ship(&mut world);
    world.advance(PAST_WAKE);
    assert!(!has_row(&world, &mono.chat), "an alarm's item is no work");
    assert!(!has_row(&world, &giga.chat), "a bad queue is no work");
    assert_eq!(the_episode(&world).actor, "mono:alu-other");
}

// The stated limit: only Mono chats. A lead or worker with queued work and no
// row stays unjudged until its first turn. On the measured database a rule that
// covered them would alarm 12 old sessions at the first pass.
#[test]
fn a_session_that_is_no_mono_chat_gets_no_row() {
    let (mut world, _, mono) = fleet("alarm-unseen-lead");
    let lead = start_lead(&world, &mono, "alu-lead");
    save_queue(&world, &lead, json!([{"id": "q-1"}]), "active");
    ship(&mut world);
    world.advance(2 * N);
    assert!(!has_row(&world, &lead));
    assert_eq!(episodes(&world), Vec::new());
}
