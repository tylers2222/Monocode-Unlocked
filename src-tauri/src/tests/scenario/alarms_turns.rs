//! Alarms S1: the stall detector's view of turns. Turn records reach the
//! hub late or out of order, background tasks outlive the turn that
//! started them, and the Mac sleeps between a turn's last sign and its
//! alarm; each pass still judges the turn `ControlHost` runs, by when it
//! really went quiet.

use super::alarms_stall::{
    align, claude_tap, episodes, idle_open_work, start_lead, turn, Episode, MINUTE, N, P, PASS,
};
use super::world::World;
use crate::alarms::taps::Pipe;
use crate::alarms::test_taps;
use crate::mono_roster::MonoRole;

// Defect: a kill's turn end that reaches the store after the next turn
// started marks the running agent idle, so its idle clock runs while it
// works; or a repeated authorize restarts the turn's clock.
#[test]
fn a_late_end_of_an_old_turn_leaves_the_next_turn_running() {
    let mut world = World::new("alarm-late-end");
    let mono = world.add_mono_in("all-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "all-lead");
    align(&mut world);
    turn(&mut world, &lead, "turn-4", true);
    world.advance(MINUTE);
    turn(&mut world, &lead, "turn-5", true);
    turn(&mut world, &lead, "turn-4", false);
    world.advance(N);
    turn(&mut world, &lead, "turn-5", true);
    world.advance(N);
    assert_eq!(episodes(&world), Vec::new(), "turn-5 runs, untapped");
    turn(&mut world, &lead, "turn-5", false);
    world.advance(N);
    assert_eq!(
        episodes(&world).len(),
        1,
        "idle with its ask from turn-5's end"
    );
}

// Defect (S1 r3 F3): a stale start of a killed turn and its re-check end
// reach the hub after the next turn's start, so the lead reads idle while
// that turn runs and its Mono is alarmed at N.
#[test]
fn a_stale_turn_record_after_the_next_start_leaves_that_turn_running() {
    let mut world = World::new("alarm-stale-start");
    let mono = world.add_mono_in("ass-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "ass-lead");
    align(&mut world);
    turn(&mut world, &lead, "t1", false);
    turn(&mut world, &lead, "t2", true);
    for edge in [crate::alarms::Edge::Started, crate::alarms::Edge::Ended] {
        world.store().alarms.turn(lead.as_str(), "t1", edge);
    }
    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new(), "t2 runs, untapped");
    turn(&mut world, &lead, "t2", false);
    world.advance(N);
    assert_eq!(episodes(&world).len(), 1, "idle with its ask from t2's end");
}

// Defect (review M2/F2): a kill that lands between a turn's authorize and
// its start record leaves the row running for a dead turn. A tapped
// session then reads as hung at N, an untapped one as working, so its
// real stall is missed. Generic: no pass heals a row whose turn
// ControlHost no longer holds.
#[test]
fn a_turn_row_no_live_turn_holds_is_idle_by_the_next_pass() {
    let mut world = World::new("alarm-stale-turn");
    let mono = world.add_mono_in("als-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "als-lead");
    world.store().alarms.spawned(lead.as_str(), Some("claude"));
    // Past the launch's first pass, so each slice's pass is on its edge.
    world.advance(PASS);
    align(&mut world);
    // The kill took turn t1 before its start was recorded: no turn runs.
    world
        .store()
        .alarms
        .turn(lead.as_str(), "t1", crate::alarms::Edge::Started);
    world.advance(2 * PASS);
    let row = test_taps::row(&world.conn(), lead.as_str()).unwrap();
    assert_eq!(row.0, "idle", "{row:?}");

    world.advance(N - PASS);
    assert_eq!(
        episodes(&world),
        Vec::new(),
        "not hung at N after the start"
    );
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        idle_open_work(&episode.id, "session:als-lead", "mono:als-mono"),
        "the stall from the healed end is not missed"
    );
}

// Defect (lead n3): a Claude background task keeps a silent turn working
// for ever, so a turn hung beside a dev server or a monitor never alarms;
// or the turn is hung at N while it may really wait on that task.
#[test]
fn a_turn_silent_on_a_background_task_is_hung_at_four_n() {
    let mut world = World::new("alarm-background");
    let mono = world.add_mono_in("alb-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alb-lead");
    let claude = claude_tap(&world, &lead);
    align(&mut world);
    turn(&mut world, &lead, "t1", true);
    claude.line(
        Pipe::Stdout,
        r#"{"type":"system","subtype":"task_started","task_id":"bg1"}"#,
    );
    world.advance(4 * N - PASS);
    assert_eq!(episodes(&world), Vec::new(), "it may wait on the task");
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("session:alb-lead", "hung")
    );
}

/// How a lead's background task ends in the idle-background scenario.
#[derive(Debug, Clone, Copy)]
enum Finish {
    Never,
    /// The task's end frame comes at 2N.
    TaskEnds,
    /// The child is killed at 2N: its stdout ends (S1 r4 F2).
    ChildExits,
}

// Defect (S1 r3 M2, lead ruling b): a lead that backgrounds a wait and
// ends its turn with its Mono's ask open alarms at N while the job runs;
// or once the job ends, its clock does not restart from that end; or
// (S1 r4 F2) a killed child's task reads as running, so the lead with
// open work alarms at 4N instead of N after the kill.
#[test]
fn an_idle_lead_on_a_background_task_alarms_at_four_n_or_n_after_it_ends() {
    for finish in [Finish::Never, Finish::TaskEnds, Finish::ChildExits] {
        let mut world = World::new(&format!("alarm-idle-background-{finish:?}"));
        let mono = world.add_mono_in("aib-mono", MonoRole::Mono, &[P]);
        let lead = start_lead(&world, &mono, "aib-lead");
        let claude = claude_tap(&world, &lead);
        align(&mut world);
        turn(&mut world, &lead, "t1", true);
        claude.line(
            Pipe::Stdout,
            r#"{"type":"system","subtype":"task_started","task_id":"bg1"}"#,
        );
        claude.line(Pipe::Stdout, r#"{"type":"result","subtype":"success"}"#);
        turn(&mut world, &lead, "t1", false);
        let ended = world.active_ms();
        world.advance(N);
        assert_eq!(episodes(&world), Vec::new(), "it waits on its job at N");

        let (stalled_from, alarm_at) = match finish {
            Finish::Never => (ended, ended + 4 * N),
            Finish::TaskEnds => {
                world.advance(N);
                claude.line(
                    Pipe::Stdout,
                    r#"{"type":"system","subtype":"task_notification","task_id":"bg1"}"#,
                );
                (world.active_ms(), world.active_ms() + N)
            }
            Finish::ChildExits => {
                world.advance(N);
                claude.exited();
                (world.active_ms(), world.active_ms() + N)
            }
        };
        world.advance(alarm_at - PASS - world.active_ms());
        assert_eq!(episodes(&world), Vec::new(), "{finish:?}: not yet");
        world.advance(PASS);
        let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
        assert_eq!(
            episode,
            idle_open_work(&episode.id, "session:aib-lead", "mono:aib-mono"),
            "{finish:?}"
        );
        let stalled: i64 = world
            .conn()
            .query_row(
                "SELECT stalled_since_active_ms FROM alarm_episodes",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stalled, stalled_from, "{finish:?}");
    }
}

// Defect (lead, S1 r5 item 6): an episode's stalled_since_wall_ms is its
// open's wall time less the active time since, so a stall that began
// before an 8 h sleep reads as 8 h later; or the row keeps no wall time of
// its last progress.
#[test]
fn a_stall_that_began_before_an_eight_hour_sleep_keeps_its_wall_time() {
    for (cause, ends_turn) in [("hung", false), ("idle_open_work", true)] {
        let mut world = World::new(&format!("alarm-sleep-wall-{cause}"));
        let mono = world.add_mono_in("asw-mono", MonoRole::Mono, &[P]);
        let lead = start_lead(&world, &mono, "asw-lead");
        let claude = claude_tap(&world, &lead);
        align(&mut world);
        turn(&mut world, &lead, "t1", true);
        claude.line(Pipe::Stdout, r#"{"type":"assistant"}"#);
        let quiet_from = world.now();
        if ends_turn {
            turn(&mut world, &lead, "t1", false);
        }
        world.advance(MINUTE);
        world.sleep(8 * 60 * MINUTE);
        world.advance(N);
        let row: (String, i64, Option<i64>) = world
            .conn()
            .query_row(
                "SELECT e.cause, e.stalled_since_wall_ms, a.last_progress_wall_ms
                   FROM alarm_episodes e JOIN agent_activity a ON 'session:' || a.session_id = e.actor",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (cause.to_string(), quiet_from, Some(quiet_from)));
    }
}
