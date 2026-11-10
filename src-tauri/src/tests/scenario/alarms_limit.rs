//! ADE-278: one level-1 alarm per stalled agent per 15 active minutes unless
//! its cause changes. Repeats here are `wake_missed` episodes that reopen,
//! which is what 9 Nov's repeats were. The rule under test throughout: an
//! alarm that stays open still reaches Tyler on its schedule.

use std::collections::HashMap;

use serde_json::json;

use super::agents::{mono_caller, session_caller, Mono};
use super::alarms_stall::{
    align, claude_tap, ran_a_turn, save_queue, turn, MINUTE, P, PASS, PERMISSION,
};
use super::asks::written;
use super::world::World;
use crate::alarms::act::{self, Outcome};
use crate::alarms::taps::Pipe;
use crate::alarms::{test_taps, TylerAlarm};
use crate::ask_scheduler;
use crate::mono_messages::SessionId;
use crate::mono_roster::MonoRole;

const WINDOW: i64 = 15 * MINUTE;
const ESCALATE: i64 = 15 * MINUTE;
const HOUR: i64 = 60 * MINUTE;
/// A queued item waits two minutes from the pass that first sees it.
const WAKE_STALL: i64 = 4 * MINUTE + 2 * PASS;

/// A Giga, its Mono, the Mono's lead and two workers of the lead, each with
/// the longest deadline on its ask.
struct Chain {
    giga: Mono,
    mono: Mono,
    lead: SessionId,
    worker: SessionId,
    other: SessionId,
}

fn chain(world: &World, name: &str) -> Chain {
    let giga = world.add_mono_in(&format!("{name}-giga"), MonoRole::Giga, &[P]);
    let mono = world.add_mono_in(&format!("{name}-mono"), MonoRole::Mono, &[P]);
    let start = |caller, id: String| {
        let input = json!({
            "prompt": "Do this", "harness": "codex", "model": "codex:default", "due_minutes": 480,
        });
        world
            .start(caller, &id, P, &format!("start-{id}"), &input)
            .unwrap();
        SessionId::parse(&id).unwrap()
    };
    let mono_start = mono_caller(&mono, "start");
    let lead = start(&mono_start, format!("{name}-lead"));
    let lead_start = session_caller(&lead, P);
    let worker = start(&lead_start, format!("{name}-worker"));
    let other = start(&lead_start, format!("{name}-other"));
    Chain {
        giga,
        mono,
        lead,
        worker,
        other,
    }
}

/// `chain`'s worker acks the ask its lead started it with.
fn acks_its_ask(world: &World, chain: &Chain) {
    let ask: String = world
        .conn()
        .query_row(
            "SELECT id FROM mono_asks WHERE child_actor = ?1 AND state = 'open'",
            [actor(&chain.worker)],
            |row| row.get(0),
        )
        .unwrap();
    written(world.tool(
        &session_caller(&chain.worker, P),
        "worker-ack",
        "ack",
        json!({"ask_id": ask, "message": "Done"}),
        &[&chain.lead, &chain.worker],
    ));
}

fn actor(session: &SessionId) -> String {
    format!("session:{}", session.as_str())
}

/// An item waits in `session`'s saved queue until it stalls: its episode.
/// Each item counts toward one episode, so a new one reopens as 9 Nov did.
fn wake_stall(world: &mut World, actor_text: &str, session: &SessionId, item: &str) -> String {
    align(world);
    save_queue(world, session, json!([{ "id": item }]), "active");
    world.advance(WAKE_STALL);
    open_episode(world, actor_text).expect("the wake stalled")
}

fn open_episode(world: &World, actor_text: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    world
        .conn()
        .query_row(
            "SELECT id FROM alarm_episodes WHERE actor = ?1 AND closed_active_ms IS NULL",
            [actor_text],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

/// `session` runs a turn that takes its queue: its open episode closes with
/// progress.
fn recovers(world: &mut World, actor_text: &str, session: &SessionId) {
    save_queue(world, session, json!([]), "active");
    ran_a_turn(world, session);
    assert_eq!(open_episode(world, actor_text), None, "closed by the turn");
}

fn one_i64(world: &World, sql: &str, param: &str) -> i64 {
    world
        .conn()
        .query_row(sql, [param], |row| row.get(0))
        .unwrap()
}

/// The text of `episode`'s level-1 message, if it was written.
fn message(world: &World, episode: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    world
        .conn()
        .query_row(
            "SELECT p.text FROM alarm_prompts p JOIN mono_outbox o ON o.key = p.outbox_key
              WHERE p.outbox_key = ?1",
            [test_taps::outbox_key(episode, 1)],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

/// The recipient's window takes outbox row `key`, as it does in the app.
fn take(world: &World, key: &str) -> String {
    let conn = world.conn();
    let claimed = ask_scheduler::claim(&conn, key, "main", &HashMap::new(), true)
        .unwrap()
        .unwrap_or_else(|| panic!("{key} is claimable"));
    assert!(ask_scheduler::sent(&conn, key, "main", world.now()).unwrap());
    claimed.text
}

/// The episode's levels as `(level, recipient, active ms after it opened)`.
fn schedule(world: &World, episode: &str) -> Vec<(i64, String, i64)> {
    world
        .conn()
        .prepare(
            "SELECT s.level, s.recipient, s.created_active_ms - e.opened_active_ms
               FROM alarm_sends s JOIN alarm_episodes e ON e.id = s.episode_id
              WHERE e.id = ?1 ORDER BY s.level",
        )
        .unwrap()
        .query_map([episode], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn notify(world: &World) -> Vec<TylerAlarm> {
    let mut shown = Vec::new();
    crate::alarms::notify_tyler(world.store(), (world.active_ms(), world.now()), |alarm| {
        shown.push(alarm.clone());
        Ok(())
    })
    .unwrap();
    shown
}

fn held_line(count: i64) -> String {
    format!("Held back since this agent's last alarm: {count} (limit one per 15 min).")
}

// Defect: every reopened episode messages the provisioner, a held alarm opens
// no episode, a held alarm's count is lost or sent twice, or an episode that
// closed and reopened with the same cause starts a new window.
#[test]
fn repeats_with_the_same_cause_send_once_and_the_next_alarm_carries_the_count() {
    let mut world = World::new("alarm-limit-repeats");
    let chain = chain(&world, "lir");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    // Held through 30 min of turns, the worker's ask would open its own
    // idle_open_work alarm by the ceiling (General's r3 GO, term 2). This test
    // is about wake_missed repeats, so the worker acks its ask first.
    acks_its_ask(&world, &chain);

    let first = wake_stall(&mut world, &who, &worker, "q1");
    let first_text = message(&world, &first).expect("the first alarm goes at once");
    assert!(!first_text.contains("Held back"), "{first_text}");
    let window_ends = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &first,
    ) + WINDOW;
    recovers(&mut world, &who, &worker);

    let second = wake_stall(&mut world, &who, &worker, "q2");
    assert_eq!(message(&world, &second), None, "held in the window");
    assert_eq!(schedule(&world, &second).len(), 1, "its send row is logged");
    recovers(&mut world, &who, &worker);

    let third = wake_stall(&mut world, &who, &worker, "q3");
    assert_eq!(message(&world, &third), None, "still held: same cause");
    recovers(&mut world, &who, &worker);
    assert_ne!(second, third, "each repeat is its own episode");

    // Both closed: nothing goes when the window ends, and the count waits.
    world.advance(window_ends - world.active_ms() + 2 * PASS);
    for episode in [&second, &third] {
        assert_eq!(message(&world, episode), None, "a closed alarm is not sent");
    }

    let fourth = wake_stall(&mut world, &who, &worker, "q4");
    let text = take(&world, &test_taps::outbox_key(&fourth, 1));
    assert!(text.contains(&held_line(2)), "{text}");

    // That send started a new window and spent the count.
    recovers(&mut world, &who, &worker);
    let fifth = wake_stall(&mut world, &who, &worker, "q5");
    assert_eq!(message(&world, &fifth), None, "the new window holds");
    recovers(&mut world, &who, &worker);
    // A turn midway, as the window's last quiet stretch.
    world.advance(10 * MINUTE);
    ran_a_turn(&mut world, &worker);
    world.advance(5 * MINUTE);
    let sixth = wake_stall(&mut world, &who, &worker, "q6");
    let text = message(&world, &sixth).expect("after the window");
    assert!(text.contains("cause=\"wake_missed\""), "{text}");
    assert!(text.contains(&held_line(1)), "only the fifth: {text}");
}

// Defect: a changed cause waits for the window, loses the held count, or
// causes that take turns defeat the limit by each counting as changed.
#[test]
fn a_changed_cause_sends_at_once_and_causes_taking_turns_do_not_defeat_the_limit() {
    let mut world = World::new("alarm-limit-cause");
    let chain = chain(&world, "lic");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    let claude = claude_tap(&world, &worker);
    align(&mut world);
    ran_a_turn(&mut world, &worker);

    let first = wake_stall(&mut world, &who, &worker, "q1");
    assert!(message(&world, &first).is_some());
    recovers(&mut world, &who, &worker);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    assert_eq!(message(&world, &held), None);
    recovers(&mut world, &who, &worker);

    align(&mut world);
    turn(&mut world, &worker, "dialog-turn", true);
    claude.line(Pipe::Stdout, PERMISSION);
    world.advance(3 * MINUTE);
    let dialog = open_episode(&world, &who).expect("the dialog stalled");
    let text = message(&world, &dialog).expect("a changed cause sends at once");
    assert!(text.contains("cause=\"dialog\""), "{text}");
    assert!(text.contains(&held_line(1)), "it carries the count: {text}");
    turn(&mut world, &worker, "dialog-turn", false);
    world.advance(PASS);
    assert_eq!(open_episode(&world, &who), None);

    // wake_missed again, still inside its own window: not a changed cause.
    let back = wake_stall(&mut world, &who, &worker, "q3");
    assert_eq!(message(&world, &back), None, "the cause came back");
}

/// The levels went to `recipients` in order, each one escalation period after
/// the last, which a pass may add to.
fn assert_on_schedule(levels: &[(i64, String, i64)], recipients: &[String]) {
    let got: Vec<&String> = levels.iter().map(|(_, recipient, _)| recipient).collect();
    assert_eq!(got, recipients.iter().collect::<Vec<_>>(), "{levels:?}");
    for (level, _, after) in levels {
        let late = after - (level - 1) * ESCALATE;
        assert!((0..=PASS).contains(&late), "level {level}: {levels:?}");
    }
}

/// Runs the clock past the whole chain: Tyler is told, once.
fn runs_up_to_tyler(world: &mut World, episode: &str) {
    world.advance(3 * ESCALATE + 2 * PASS);
    let shown = notify(world);
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].episode, episode);
    world.advance(2 * HOUR);
    assert_eq!(notify(world), Vec::new(), "shown once");
}

fn expected_chain(chain: &Chain) -> Vec<String> {
    vec![
        actor(&chain.lead),
        format!("mono:{}", chain.mono.id.as_str()),
        format!("mono:{}", chain.giga.id.as_str()),
        "tyler".to_string(),
    ]
}

// Defect (the one rule): a held alarm for a stall that stays open reaches no
// one, its release restarts the escalation clock so it climbs late, or its
// message overstates the time left before the next level.
#[test]
fn an_open_stall_is_told_when_the_window_ends_and_escalates_on_the_unheld_schedule() {
    let mut world = World::new("alarm-limit-open");
    let chain = chain(&world, "lio");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    // Held through 30 min of turns, the worker's ask would open its own
    // idle_open_work alarm by the ceiling (General's r3 GO, term 2). This test
    // is about wake_missed repeats, so the worker acks its ask first.
    acks_its_ask(&world, &chain);

    let first = wake_stall(&mut world, &who, &worker, "q1");
    let window_ends = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &first,
    ) + WINDOW;
    recovers(&mut world, &who, &worker);

    let held = wake_stall(&mut world, &who, &worker, "q2");
    let due = |world: &World| {
        one_i64(
            world,
            "SELECT next_escalate_active_ms FROM alarm_episodes WHERE id = ?1",
            &held,
        )
    };
    let opened = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &held,
    );
    assert_eq!(due(&world), opened + ESCALATE, "its clock runs while held");

    world.advance(window_ends - world.active_ms() - PASS);
    assert_eq!(message(&world, &held), None, "the window has not ended");
    world.advance(2 * PASS);
    let key = test_taps::outbox_key(&held, 1);
    let released = one_i64(
        &world,
        "SELECT at_active_ms FROM mono_outbox WHERE key = ?1",
        &key,
    );
    assert!(
        (window_ends..=window_ends + PASS).contains(&released),
        "{released}"
    );
    let text = take(&world, &key);
    assert!(text.contains(&held_line(1)), "{text}");
    let left = (opened + ESCALATE - released) / MINUTE;
    assert!(left < 15, "{left}");
    assert!(
        text.contains(&format!("No act in {left} min goes to")),
        "{left}: {text}"
    );
    assert_eq!(due(&world), opened + ESCALATE, "the release moves no clock");

    // record_sent marks the released row taken, as any level.
    world.advance(PASS);
    assert_eq!(
        one_i64(
            &world,
            "SELECT sent_wall_ms IS NOT NULL FROM alarm_sends WHERE episode_id = ?1 AND level = 1",
            &held,
        ),
        1
    );

    world.advance(opened + ESCALATE + PASS - world.active_ms());
    assert_eq!(
        schedule(&world, &held).len(),
        2,
        "level 2 on the unheld clock"
    );
    runs_up_to_tyler(&mut world, &held);
    assert_on_schedule(&schedule(&world, &held), &expected_chain(&chain));

    // The twin no limit held climbs the same chain at the same offsets.
    let mut twin = World::new("alarm-limit-twin");
    let twin_chain = self::chain(&twin, "lio");
    let (who, worker) = (actor(&twin_chain.worker), twin_chain.worker.clone());
    align(&mut twin);
    ran_a_turn(&mut twin, &worker);
    let unheld = wake_stall(&mut twin, &who, &worker, "q1");
    runs_up_to_tyler(&mut twin, &unheld);
    assert_eq!(schedule(&twin, &unheld), schedule(&world, &held));
}

// Defect (the main risk): escalation assumes level 1 went out. A hold with
// no send row shifts `chain` by one so level 2 skips the provisioner, and
// nothing reaches Tyler. Here the message is never written at all (the state
// that releases it is lost, as a re-check's drop leaves it).
#[test]
fn a_stall_whose_level_one_was_never_sent_still_climbs_every_level_on_schedule() {
    let mut world = World::new("alarm-limit-never");
    let chain = chain(&world, "lin");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);

    wake_stall(&mut world, &who, &worker, "q1");
    recovers(&mut world, &who, &worker);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    world
        .conn()
        .execute_batch("UPDATE alarm_repeat_limit SET held_episode = NULL")
        .unwrap();

    world.advance(ESCALATE + PASS);
    assert_eq!(message(&world, &held), None, "level 1 never went");
    let level_two = take(&world, &test_taps::outbox_key(&held, 2));
    assert!(
        level_two.contains(&format!("{} (", chain.lead.as_str())),
        "level 2 names the provisioner below it: {level_two}"
    );
    runs_up_to_tyler(&mut world, &held);
    assert_on_schedule(&schedule(&world, &held), &expected_chain(&chain));
    assert_eq!(message(&world, &held), None, "and never does");
}

// Defect: with a short escalation the provisioner is skipped: level 2 goes
// while level 1 waits for the window, and the alarm it missed never reached it.
#[test]
fn a_held_alarm_goes_before_an_escalation_that_comes_first() {
    let mut world = World::new("alarm-limit-short");
    let chain = chain(&world, "lis");
    world
        .conn()
        .execute_batch("INSERT INTO alarm_settings VALUES ('escalate_minutes', '5')")
        .unwrap();
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    wake_stall(&mut world, &who, &worker, "q1");
    recovers(&mut world, &who, &worker);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    assert_eq!(message(&world, &held), None);
    let due = one_i64(
        &world,
        "SELECT next_escalate_active_ms FROM alarm_episodes WHERE id = ?1",
        &held,
    );
    assert!(
        due < world.active_ms() + WINDOW,
        "it comes before the window ends"
    );

    world.advance(due - world.active_ms() - PASS);
    assert_eq!(message(&world, &held), None, "not yet due");
    world.advance(2 * PASS);
    let text = message(&world, &held).expect("told before it goes up");
    assert!(text.contains("No act in 1 min goes to"), "{text}");
    assert_eq!(schedule(&world, &held).len(), 2, "and it went up on time");
}

// Defect: the window is lost on a restart, so a restart re-opens the
// floodgate, or it counts downtime and sleep, so it ends while nothing ran.
#[test]
fn the_window_survives_a_restart_and_runs_on_the_active_clock() {
    let mut world = World::new("alarm-limit-restart");
    let chain = chain(&world, "lit");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    wake_stall(&mut world, &who, &worker, "q1");
    recovers(&mut world, &who, &worker);

    world.kill();
    world.restart();
    world.sleep(2 * HOUR);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    assert_eq!(
        message(&world, &held),
        None,
        "held across restart and sleep"
    );

    world.kill();
    world.restart();
    world.sleep(2 * HOUR);
    world.advance(PASS);
    assert_eq!(
        message(&world, &held),
        None,
        "the sleep did not end the window"
    );
    world.advance(WINDOW);
    let text = message(&world, &held).expect("the hold survived the restart");
    assert!(text.contains(&held_line(1)), "{text}");
}

// Defect: the limit is global or per provisioner, so one agent's alarm holds
// another's; or it reaches Tyler, whose notification is never limited.
#[test]
fn the_limit_is_per_stalled_agent_and_never_holds_tyler() {
    let mut world = World::new("alarm-limit-agent");
    let chain = chain(&world, "lia");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    let (other_who, other) = (actor(&chain.other), chain.other.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    ran_a_turn(&mut world, &other);
    let first = wake_stall(&mut world, &who, &worker, "q1");
    assert!(message(&world, &first).is_some());
    let second = wake_stall(&mut world, &other_who, &other, "q1");
    assert!(
        message(&world, &second).is_some(),
        "another agent under the same provisioner"
    );

    let mut world = World::new("alarm-limit-tyler");
    let giga = world.add_mono_in("lit-giga", MonoRole::Giga, &[P]);
    let who = format!("mono:{}", giga.id.as_str());
    let chat = giga.chat.clone();
    align(&mut world);
    ran_a_turn(&mut world, &chat);
    let first = wake_stall(&mut world, &who, &chat, "q1");
    assert_eq!(notify(&world).len(), 1, "level 1 is Tyler's");
    recovers(&mut world, &who, &chat);
    let second = wake_stall(&mut world, &who, &chat, "q2");
    assert_ne!(first, second);
    assert_eq!(notify(&world).len(), 1, "a repeat is not held from Tyler");
}

// Defect: the provisioner that already acted on a held alarm is told of it
// when the window ends.
#[test]
fn a_held_alarm_acted_on_is_not_sent() {
    let mut world = World::new("alarm-limit-acted");
    let chain = chain(&world, "lid");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    let first = wake_stall(&mut world, &who, &worker, "q1");
    let window_ends = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &first,
    ) + WINDOW;
    recovers(&mut world, &who, &worker);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    let acted = act::call(
        &world.conn(),
        chain.lead.as_str(),
        "alarms.dismiss",
        &json!({"alarm": held, "reason": "seen"}),
        "dismiss-1",
        (world.active_ms(), world.now()),
    );
    assert!(
        matches!(acted, Ok(Outcome::Done { .. })),
        "the dismiss was taken"
    );
    // Past the window, before the act's own N minutes would close the episode
    // and drop any row it wrote.
    world.advance(window_ends - world.active_ms() + 2 * PASS);
    assert_eq!(open_episode(&world, &who).as_ref(), Some(&held));
    assert_eq!(message(&world, &held), None);
}

// Defect: a send stamped ahead of the clock (a database moved between clocks)
// holds every alarm until the clock catches up.
#[test]
fn a_send_stamped_ahead_of_the_clock_holds_nothing() {
    let mut world = World::new("alarm-limit-ahead");
    let chain = chain(&world, "lif");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    wake_stall(&mut world, &who, &worker, "q1");
    recovers(&mut world, &who, &worker);
    world
        .conn()
        .execute(
            "UPDATE alarm_repeat_limit SET sent_active_ms = ?1",
            [world.active_ms() + 10 * HOUR],
        )
        .unwrap();
    let next = wake_stall(&mut world, &who, &worker, "q2");
    assert!(message(&world, &next).is_some());
}

/// A `hold_missing` episode of `who` opens through routing, as #546's detector
/// path opens one; nothing on main does yet.
fn hold_missing_opens(world: &World, id: &str, who: &str, provisioner: &str) {
    let active = world.active_ms();
    world
        .conn()
        .execute(
            "INSERT INTO alarm_episodes (id, actor, provisioner, cause, open_work_json,
               stalled_since_active_ms, stalled_since_wall_ms, opened_active_ms, opened_wall_ms,
               next_escalate_active_ms)
             VALUES (?1, ?2, ?3, 'hold_missing', '[]', ?4, ?5, ?4, ?5, ?6)",
            rusqlite::params![id, who, provisioner, active, world.now(), active + ESCALATE],
        )
        .unwrap();
    test_taps::opened(&world.conn(), id, active);
}

/// The detector leaves a `hold_missing` episode open, so the test closes it.
fn closes(world: &World, id: &str) {
    world
        .conn()
        .execute(
            "UPDATE alarm_episodes SET closed_active_ms = ?2, closed_wall_ms = ?3,
               close_reason = 'progress' WHERE id = ?1",
            rusqlite::params![id, world.active_ms(), world.now()],
        )
        .unwrap();
}

// Defect (ruling on ADE-278): a missing sleep hold is not a stall, yet the
// limit holds its repeats, gives it a window, or spends the stall count on it,
// so the next stall alarm loses what was held.
#[test]
fn a_missing_sleep_hold_is_never_limited_and_leaves_the_stall_count_alone() {
    let mut world = World::new("alarm-limit-hold");
    let chain = chain(&world, "lih");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    let lead = actor(&chain.lead);
    align(&mut world);
    ran_a_turn(&mut world, &worker);

    let first = wake_stall(&mut world, &who, &worker, "q1");
    let window_ends = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &first,
    ) + WINDOW;
    recovers(&mut world, &who, &worker);
    let held = wake_stall(&mut world, &who, &worker, "q2");
    assert_eq!(message(&world, &held), None, "a stall repeat is held");
    recovers(&mut world, &who, &worker);

    for id in ["al_hold1", "al_hold2"] {
        hold_missing_opens(&world, id, &who, &lead);
        let text = message(&world, id).expect("never held, a repeat included");
        assert!(text.contains("cause=\"hold_missing\""), "{text}");
        assert!(!text.contains("Held back"), "not a stall's count: {text}");
        closes(&world, id);
    }
    assert_eq!(
        one_i64(
            &world,
            "SELECT count(*) FROM alarm_repeat_limit WHERE cause = ?1",
            "hold_missing",
        ),
        0,
        "it uses no window"
    );

    world.advance(window_ends - world.active_ms() + 2 * PASS);
    let next = wake_stall(&mut world, &who, &worker, "q3");
    let text = message(&world, &next).expect("after the window");
    assert!(text.contains(&held_line(1)), "the count is intact: {text}");
}

// Defect (the one rule): a held episode closes unsent and its item counts as
// already alarmed, so an item that stays stuck never alarms and never escalates.
#[test]
fn an_item_held_and_closed_unsent_alarms_when_it_stays_stuck() {
    let mut world = World::new("alarm-limit-unsent");
    let chain = chain(&world, "liu");
    let (who, worker) = (actor(&chain.worker), chain.worker.clone());
    align(&mut world);
    ran_a_turn(&mut world, &worker);
    // Held through 30 min of turns, the worker's ask would open its own
    // idle_open_work alarm by the ceiling (General's r3 GO, term 2). This test
    // is about wake_missed repeats, so the worker acks its ask first.
    acks_its_ask(&world, &chain);

    let first = wake_stall(&mut world, &who, &worker, "q1");
    let window_ends = one_i64(
        &world,
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        &first,
    ) + WINDOW;
    recovers(&mut world, &who, &worker);

    // A turn ran, but it did not take the item: the held episode closes unsent.
    let held = wake_stall(&mut world, &who, &worker, "stuck");
    assert_eq!(message(&world, &held), None);
    ran_a_turn(&mut world, &worker);
    assert_eq!(open_episode(&world, &who), None, "closed unsent");

    world.advance(WAKE_STALL);
    let again = open_episode(&world, &who).expect("the stuck item stalls again");
    assert_ne!(again, held);
    world.advance(window_ends - world.active_ms() + 2 * PASS);
    let text = take(&world, &test_taps::outbox_key(&again, 1));
    assert!(text.contains(&held_line(2)), "{text}");

    // Now someone was told, so the item counts once, as any other.
    ran_a_turn(&mut world, &worker);
    world.advance(WAKE_STALL + MINUTE);
    assert_eq!(open_episode(&world, &who), None, "told once");
}
