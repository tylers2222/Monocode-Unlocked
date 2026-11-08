//! Alarms P2: what a spawn's sleep hold install came to reaches its
//! `agent_activity` row in order, a respawn starts it over, and an omp
//! session whose extension stays silent into a turn is missing its hold.

use serde_json::json;

use super::sleep_hold::{chain, hold_state_of, ping, PASS};
use super::world::World;
use crate::mono_messages::SessionId;
use crate::sleep_hold::install::{HoldState, PING_GRACE_MS};

const SECOND: i64 = 1_000;

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
