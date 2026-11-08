//! Alarms P2: what a spawn's sleep hold install came to reaches its
//! `agent_activity` row in order, a respawn starts it over, and an omp
//! session whose extension stays silent into a turn is missing its hold.

use serde_json::json;

use super::sleep_hold::{chain, hold_state_of, ping, PASS};
use super::world::World;
use crate::mono_messages::SessionId;
use crate::sleep_hold::install::{HoldState, PING_GRACE_MS};

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;

const SILENT: &str = "the omp hold extension did not report in";

/// A `provider` child of `session` spawned now with sleep hold `hold`, read
/// by the tap `harness_spawn` gives it.
fn spawn(world: &World, session: &SessionId, provider: &str, hold: Option<HoldState>) {
    let alarms = &world.store().alarms;
    let tap = alarms.tap(session.as_str(), Some(provider));
    alarms.spawned(session.as_str(), tap.as_ref(), hold);
}

// Defect (H risk 3): a respawn keeps the last child's ping, so a new omp
// whose extension never loaded reads as installed; or a ping that comes
// before the spawn's pass is wiped by the spawn's reset.
#[test]
fn an_omp_respawn_waits_for_its_own_ping() {
    let mut world = World::new("hold-respawn");
    let chain = chain(&world, "hr");
    let awaiting = || Some(HoldState::AwaitingPing);
    spawn(&world, &chain.worker, "omp", awaiting());
    world.alarm_turn_started(&chain.worker, "turn-1");
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(PING_GRACE_MS + PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );

    spawn(&world, &chain.worker, "omp", awaiting());
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "still in its grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: an omp session idle before its first turn reads as missing,
// though the app takes a ping only inside a turn.
#[test]
fn an_omp_session_has_its_grace_from_its_first_turn() {
    let mut world = World::new("hold-first-turn");
    let chain = chain(&world, "hf");
    spawn(&world, &chain.worker, "omp", Some(HoldState::AwaitingPing));
    world.advance(5 * 60 * SECOND);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "idle, never asked"
    );
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "in the turn's grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: a Claude or Codex spawn's install result is not what the row
// shows, so a dropped hook never reaches the provisioner.
#[test]
fn a_spawn_records_how_its_hold_went_in() {
    let mut world = World::new("hold-spawn-state");
    let chain = chain(&world, "hs");
    let missing = HoldState::Missing("hooks disabled".into());
    spawn(&world, &chain.worker, "claude", Some(missing));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some("hooks disabled".into()))
    );
    spawn(&world, &chain.worker, "codex", Some(HoldState::Installed));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );
    spawn(&world, &chain.worker, "cursor", None);
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "no hold for this harness"
    );
}

/// Each `hold_missing` episode of `session`, oldest first: its close reason.
fn hold_episodes(world: &World, session: &SessionId) -> Vec<Option<String>> {
    world
        .conn()
        .prepare(
            "SELECT close_reason FROM alarm_episodes
              WHERE actor = ?1 AND cause = 'hold_missing' ORDER BY opened_active_ms, id",
        )
        .unwrap()
        .query_map([format!("session:{}", session.as_str())], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// Defect: a session whose hold did not go in is never reported, is
// reported again each pass, or its alarm outlives the hold going in.
#[test]
fn a_missing_hold_alarms_its_provisioner_once_per_spawn() {
    let mut world = World::new("hold-missing-alarm");
    let chain = chain(&world, "hm");
    let disabled = || Some(HoldState::Missing("hooks disabled".into()));
    spawn(&world, &chain.worker, "claude", disabled());
    world.advance(10 * PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    let text: String = world
        .conn()
        .query_row(
            "SELECT p.text FROM alarm_prompts p JOIN alarm_episodes e ON e.id = p.episode_id
              WHERE e.actor = ?1",
            [format!("session:{}", chain.worker.as_str())],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        text.contains(
            "runs without the sleep hold (hooks disabled). Its foreground waits are not held."
        ),
        "{text}"
    );

    spawn(&world, &chain.worker, "claude", Some(HoldState::Installed));
    world.advance(PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into())],
        "closed once the hold is in"
    );
    world.advance(10 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker).len(),
        1,
        "none while installed"
    );

    spawn(&world, &chain.worker, "claude", disabled());
    world.advance(10 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into()), None],
        "the next spawn without the hold is told again, once"
    );
}

// Defect: an omp session whose extension never reports in is not
// reported, or one that reports in late keeps its alarm.
#[test]
fn a_silent_omp_extension_alarms_and_its_late_ping_closes_the_alarm() {
    let mut world = World::new("hold-missing-omp");
    let chain = chain(&world, "ho");
    spawn(&world, &chain.worker, "omp", Some(HoldState::AwaitingPing));
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        Vec::<Option<String>>::new()
    );
    world.advance(3 * PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into())]
    );
}

// Defect: a hold_missing episode holds the agent's one episode while it
// really stalls, or the spawn's missing hold is told again once the stall
// it gave way to is over.
#[test]
fn a_stall_supersedes_the_missing_hold_alarm_and_the_spawn_is_not_told_again() {
    let mut world = World::new("hold-missing-stall");
    let chain = chain(&world, "hx");
    spawn(
        &world,
        &chain.worker,
        "codex",
        Some(HoldState::Missing("flags refused".into())),
    );
    world.advance(PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    // The worker holds its lead's open ask: idle with open work for N.
    world.advance(15 * MINUTE);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("superseded".into())]
    );
    let open: String = world
        .conn()
        .query_row(
            "SELECT cause FROM alarm_episodes WHERE actor = ?1 AND closed_active_ms IS NULL",
            [format!("session:{}", chain.worker.as_str())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open, "idle_open_work");
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(4 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("superseded".into())],
        "the stall is over; this spawn was told already"
    );
}
