//! ADE-306: a Mono that works in habit runs is not idle. Each run is its own
//! session (`app-<chat>-habit-<uuid>`, as the front end names it), and its
//! turns count as activity for the Mono whose chat it names.

use super::agents::Mono;
use super::alarms_stall::{align, ask_and_take, episodes, turn, Episode, MINUTE, N, P, PASS};
use super::world::World;
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;

/// Habit run `n` of `mono`, named as the front end names it (a uuid).
fn habit_run(world: &World, mono: &Mono, n: u32) -> SessionId {
    let run = format!("00000000-0000-4000-8000-{n:012}");
    world.add_session(&format!("app-{}-habit-{run}", mono.chat.as_str()), P, None)
}

/// A habit run of `mono` whose turn takes a minute.
fn ran_a_habit(world: &mut World, mono: &Mono, n: u32) {
    let session = habit_run(world, mono, n);
    turn(world, &session, "t1", true);
    world.advance(MINUTE);
    turn(world, &session, "t1", false);
}

/// The open and closed episodes of `actor`.
fn of(world: &World, actor: &str) -> Vec<Episode> {
    episodes(world)
        .into_iter()
        .filter(|episode| episode.actor == actor)
        .collect()
}

/// `episode` as its row reads once it closed.
fn closed(episode: &Episode) -> Episode {
    Episode {
        open: false,
        ..episode.clone()
    }
}

/// `(stalled since, opened, close reason)` of episode `id`.
fn times(world: &World, id: &str) -> (i64, i64, Option<String>) {
    world
        .conn()
        .query_row(
            "SELECT stalled_since_active_ms, opened_active_ms, close_reason
               FROM alarm_episodes WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

// Defect (ADE-306): the detector reads only a Mono's chat, so a Mono that
// works its open asks in habit runs alarmed idle_open_work every N all night
// (Mono Reviewer, 11 Nov). Rows: (a) a Mono working only in habit runs gets
// no alarm; (b) a Mono with stuck open work and no runs still gets one at N,
// while the other's runs go on; (c) a run that never ends hides nothing past
// the background wait; (d) a run's end is progress, and N after the last run
// the open work alarms again.
#[test]
fn a_mono_working_in_habit_runs_is_not_idle_and_a_stuck_mono_still_alarms() {
    let mut world = World::new("alarm-habit-runs");
    let general = world.add_mono_in("ah-general", MonoRole::Giga, &[P]);
    let busy = world.add_mono_in("ah-busy", MonoRole::Mono, &[P]);
    let stuck = world.add_mono_in("ah-stuck", MonoRole::Mono, &[P]);
    align(&mut world);
    ask_and_take(&mut world, &general, &busy, "ask-busy");
    ask_and_take(&mut world, &general, &stuck, "ask-stuck");
    // Both chats are idle now, each with the General's open ask.
    let stuck_ended = world.active_ms();

    // (a) + (b): two hours of habit runs every 14 minutes for the busy Mono.
    for run in 0..9 {
        ran_a_habit(&mut world, &busy, run);
        world.advance(13 * MINUTE);
    }
    assert_eq!(
        of(&world, "mono:ah-busy"),
        Vec::new(),
        "(a) busy in habit runs"
    );
    let [stuck_episode] = <[Episode; 1]>::try_from(of(&world, "mono:ah-stuck")).unwrap();
    assert_eq!(
        (stuck_episode.cause.as_str(), stuck_episode.open),
        ("idle_open_work", true),
        "(b) the stuck Mono"
    );
    assert_eq!(
        times(&world, &stuck_episode.id),
        (stuck_ended, stuck_ended + N, None),
        "(b) opened once, at N after its chat's turn"
    );

    // (c) A run that hangs is waited on as a background task, and as long.
    let hang = habit_run(&world, &busy, 99);
    let hang_started = world.active_ms();
    turn(&mut world, &hang, "hang", true);
    world.advance(4 * N - PASS);
    assert_eq!(
        of(&world, "mono:ah-busy"),
        Vec::new(),
        "(c) waits on its run"
    );
    world.advance(PASS);
    let [busy_episode] = <[Episode; 1]>::try_from(of(&world, "mono:ah-busy")).unwrap();
    assert_eq!(
        (busy_episode.cause.as_str(), busy_episode.open),
        ("idle_open_work", true)
    );
    assert_eq!(
        times(&world, &busy_episode.id).0,
        hang_started,
        "(c) stalled since the run started"
    );
    world.advance(PASS);
    assert_eq!(
        of(&world, "mono:ah-busy"),
        vec![busy_episode.clone()],
        "(c) the run's own start, where the stall began, is no progress"
    );

    // (d) The run's end is progress; with no run after it, N later the open
    // work alarms again.
    turn(&mut world, &hang, "hang", false);
    let hang_ended = world.active_ms();
    world.advance(PASS);
    assert_eq!(
        times(&world, &busy_episode.id).2.as_deref(),
        Some("progress"),
        "(d) a run's end closes the episode"
    );
    world.advance(N - 2 * PASS);
    assert_eq!(of(&world, "mono:ah-busy").len(), 1, "(d) none before N");
    world.advance(PASS);
    let busy_now = of(&world, "mono:ah-busy");
    let [_, again] = <[Episode; 2]>::try_from(busy_now).unwrap();
    assert!(again.open);
    assert_eq!(
        times(&world, &again.id).0,
        hang_ended,
        "(d) stalled since the last run ended"
    );
    world.advance(PASS);
    assert_eq!(
        of(&world, "mono:ah-busy"),
        vec![closed(&busy_episode), again.clone()],
        "(d) the run's own end, where the stall began, is no progress"
    );
    assert_eq!(
        of(&world, "mono:ah-stuck"),
        vec![stuck_episode],
        "the stuck Mono's one episode stays"
    );
}
