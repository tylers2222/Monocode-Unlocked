//! The active clock (R2) across kills, downtime, sleep, wall jumps, a
//! stalled tick and a second process.

use super::world::World;

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;

/// The stored row: what a crash keeps.
fn stored(world: &World) -> i64 {
    world
        .conn()
        .query_row("SELECT active_ms FROM mono_clock WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// The downtime rows as `(cause, end - start)`, oldest first.
fn downtime(world: &World) -> Vec<(String, i64)> {
    world
        .conn()
        .prepare("SELECT cause, end_wall_ms - start_wall_ms FROM mono_downtime ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Defect: downtime, a reboot, sleep or a wall-clock change adds active
/// time; a kill loses more than the 5 s persist interval; a stalled tick
/// adds its whole gap; or a second process on the same data folder ticks
/// the clock too.
#[test]
fn clock_counts_only_awake_running_time() {
    let mut world = World::new("clock");
    let base = world.active_ms();
    assert!(
        downtime(&world).is_empty(),
        "a first launch logs no downtime"
    );

    // A kill keeps what the store wrote: every 5 s of active time.
    world.advance(12_500);
    assert_eq!(world.active_ms(), base + 12_500);
    world.down(40 * MINUTE);
    assert_eq!(
        world.active_ms(),
        base + 10_000,
        "at most 5 s lost, none added"
    );
    assert_eq!(stored(&world), base + 10_000);
    assert_eq!(
        downtime(&world),
        [("app_down".into(), 40 * MINUTE + 2_500)],
        "the gap since the last write is logged, not counted"
    );

    // A reboot while the app is down adds nothing and is told apart.
    world.reboot(10 * MINUTE);
    assert_eq!(world.active_ms(), base + 10_000);
    assert_eq!(downtime(&world)[1], ("reboot".into(), 10 * MINUTE));

    // A 1 h sleep adds nothing, is logged, and the row is written.
    world.advance(3 * SECOND);
    world.sleep(HOUR);
    assert_eq!(world.active_ms(), base + 13_000);
    assert_eq!(downtime(&world)[2], ("sleep".into(), HOUR));
    assert_eq!(stored(&world), base + 13_000);

    // Wall jumps of +/- 2 h add nothing.
    world.jump_wall(2 * HOUR);
    world.advance(SECOND);
    world.jump_wall(-2 * HOUR);
    world.advance(SECOND);
    assert_eq!(world.active_ms(), base + 15_000);
    assert_eq!(downtime(&world).len(), 3, "a wall jump is no sleep");

    // A tick that stalls for 90 s adds at most 30 s.
    world.stall(90 * SECOND);
    assert_eq!(world.active_ms(), base + 45_000);

    // A second process on the same data folder gets no lease and writes
    // nothing; once the first is gone, a launch takes it.
    let rows = downtime(&world).len();
    assert!(!world.second_launch(), "a second process ticks nothing");
    assert_eq!(downtime(&world).len(), rows);
    assert_eq!(stored(&world), base + 45_000);
    world.kill();
    world.restart();
    assert_eq!(world.active_ms(), base + 45_000);
}
