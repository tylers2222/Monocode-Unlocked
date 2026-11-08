//! Alarms S1: the stall detector runs on the active clock's own tick over
//! real rows. The turn and stdio taps record into the store's hub as
//! `control.rs` and `harness.rs` do in the app, and the next pass writes.

use std::collections::HashMap;

use serde_json::json;

use super::agents::{mono_caller, Mono};
use super::world::World;
use crate::alarms::taps::Pipe;
use crate::alarms::{test_taps, ChildTap};
use crate::ask_scheduler;
use crate::mono_messages::{SessionId, Tool};
use crate::mono_roster::MonoRole;

const P: &str = "/p/alarms";
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
/// The default stall threshold N (design section 3).
const N: i64 = 15 * MINUTE;
/// The detector runs once per 5 s of active time.
const PASS: i64 = 5 * SECOND;

/// `mono` starts lead `id` with the longest deadline, so no ask reminder
/// waits for it during a scenario.
fn start_lead(world: &World, mono: &Mono, id: &str) -> SessionId {
    let input = json!({
        "prompt": "Lead this", "harness": "codex", "model": "codex:default", "due_minutes": 480,
    });
    world
        .start(
            &mono_caller(mono, "start"),
            id,
            P,
            &format!("start-{id}"),
            &input,
        )
        .unwrap();
    SessionId::parse(id).unwrap()
}

/// Active time moves to the start of the next detector slice, so a
/// threshold counted from now ends on a pass.
fn align(world: &mut World) {
    let rest = world.active_ms() % PASS;
    if rest != 0 {
        world.advance(PASS - rest);
    }
}

fn turn(world: &mut World, session: &SessionId, turn_id: &str, started: bool) {
    if started {
        world.alarm_turn_started(session, turn_id);
    } else {
        world.alarm_turn_ended(session, turn_id);
    }
}

/// A turn of `session` that ends at an aligned active time.
fn ran_a_turn(world: &mut World, session: &SessionId) {
    let turn_id = format!("turn-{}", world.active_ms());
    turn(world, session, &turn_id, true);
    world.advance(MINUTE);
    turn(world, session, &turn_id, false);
}

/// The stdio tap `harness_spawn` gives a Claude child of `session`.
fn claude_tap(world: &World, session: &SessionId) -> ChildTap {
    world
        .store()
        .alarms
        .tap(session.as_str(), Some("claude"))
        .unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Episode {
    id: String,
    actor: String,
    provisioner: String,
    cause: String,
    dialog_text: Option<String>,
    open: bool,
}

fn episodes(world: &World) -> Vec<Episode> {
    world
        .conn()
        .prepare(
            "SELECT id, actor, provisioner, cause, dialog_text, closed_active_ms IS NULL
               FROM alarm_episodes ORDER BY opened_active_ms, id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(Episode {
                id: row.get(0)?,
                actor: row.get(1)?,
                provisioner: row.get(2)?,
                cause: row.get(3)?,
                dialog_text: row.get(4)?,
                open: row.get(5)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// An outbox alarm row as `(key, recipient)`.
type OutboxRow = (String, String);
/// A send as `(episode, level, recipient)`.
type Send = (String, i64, String);

/// The alarm rows of the outbox, and the send log.
fn alarm_rows(world: &World) -> (Vec<OutboxRow>, Vec<Send>) {
    let conn = world.conn();
    let outbox = conn
        .prepare("SELECT key, recipient FROM mono_outbox WHERE stage = 'alarm' ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let sends = conn
        .prepare("SELECT episode_id, level, recipient FROM alarm_sends ORDER BY episode_id, level")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    (outbox, sends)
}

fn idle_open_work(id: &str, actor: &str, provisioner: &str) -> Episode {
    Episode {
        id: id.into(),
        actor: actor.into(),
        provisioner: provisioner.into(),
        cause: "idle_open_work".into(),
        dialog_text: None,
        open: true,
    }
}

// Defect: a lead idle with its Mono's open ask alarms before N, never, or
// more than once; or the alarm never reaches the Mono's chat.
#[test]
fn an_idle_lead_with_an_open_ask_alarms_its_mono_once_at_n() {
    let mut world = World::new("alarm-idle-ask");
    world.add_mono_in("al-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("al-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "al-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);

    world.advance(N - SECOND);
    assert_eq!(episodes(&world), Vec::new(), "no alarm at N - 1 s");
    world.advance(SECOND);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        idle_open_work(&episode.id, "session:al-lead", "mono:al-mono")
    );
    let key = test_taps::outbox_key(&episode.id, 1);
    assert_eq!(
        alarm_rows(&world),
        (
            vec![(key.clone(), "mono:al-mono".into())],
            vec![(episode.id.clone(), 1, "mono:al-mono".into())]
        )
    );

    let conn = world.conn();
    assert!(ask_scheduler::pending(&conn, &mono.chat)
        .unwrap()
        .contains(&key));
    let claimed = ask_scheduler::claim(&conn, &key, "main", &HashMap::new(), true)
        .unwrap()
        .expect("the Mono's idle chat takes the alarm as a wake");
    assert!(claimed.wake);
    assert!(
        claimed.text.starts_with(&format!(
            "<mono_alarm alarm=\"{}\" level=\"1\" cause=\"idle_open_work\">",
            episode.id
        )),
        "{}",
        claimed.text
    );
    drop(conn);

    world.advance(2 * HOUR);
    assert_eq!(episodes(&world), vec![episode], "still one after 2 h");
    assert_eq!(alarm_rows(&world).1.len(), 1);
}

// Defect: an agent with nothing open, or a lead waiting on its running
// worker, alarms; or the lead's clock starts before its worker ended.
#[test]
fn no_open_work_and_a_running_worker_raise_no_alarm() {
    let mut world = World::new("alarm-waiting");
    let mono = world.add_mono_in("alw-mono", MonoRole::Mono, &[P]);
    let solo = world.add_session("alw-solo", P, None);
    let lead = start_lead(&world, &mono, "alw-lead");
    let worker = world.add_session("alw-worker", P, Some("alw-lead"));
    align(&mut world);
    ran_a_turn(&mut world, &solo);
    turn(&mut world, &lead, "t1", true);
    turn(&mut world, &worker, "t1", true);
    world.advance(MINUTE);
    turn(&mut world, &lead, "t1", false);

    world.advance(4 * N);
    assert_eq!(episodes(&world), Vec::new());

    turn(&mut world, &worker, "t1", false);
    world.advance(N - PASS);
    assert_eq!(
        episodes(&world),
        Vec::new(),
        "counted from the worker's end"
    );
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        idle_open_work(&episode.id, "session:alw-lead", "mono:alw-mono")
    );
}

const PERMISSION: &str = r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf target"}}}"#;
const ANSWER: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{"behavior":"allow"}}}"#;
const PERMISSION_2: &str = r#"{"type":"control_request","request_id":"req_2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"git push"}}}"#;

// Defect: a dialog answered at 2:59 alarms, or one left 3 minutes does not,
// or its alarm does not carry the dialog.
#[test]
fn a_dialog_alarms_at_three_minutes_and_not_when_answered_at_two_fifty_nine() {
    let mut world = World::new("alarm-dialog");
    let mono = world.add_mono_in("ald-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "ald-lead");
    let claude = claude_tap(&world, &lead);
    align(&mut world);
    turn(&mut world, &lead, "t1", true);
    claude.line(Pipe::Stdout, PERMISSION);
    world.advance(3 * MINUTE - SECOND);
    claude.line(Pipe::Stdin, ANSWER);
    world.advance(2 * MINUTE);
    assert_eq!(episodes(&world), Vec::new(), "answered at 2:59");

    align(&mut world);
    claude.line(Pipe::Stdout, PERMISSION_2);
    world.advance(3 * MINUTE - PASS);
    assert_eq!(episodes(&world), Vec::new());
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        Episode {
            id: episode.id.clone(),
            actor: "session:ald-lead".into(),
            provisioner: "mono:ald-mono".into(),
            cause: "dialog".into(),
            dialog_text: Some("Bash git push".into()),
            open: true,
        }
    );
}

// Defect: a message the Mono's provider never takes leaves it stalled with
// no alarm, or the alarm goes to someone other than its root Giga.
#[test]
fn a_message_not_taken_for_two_minutes_is_wake_missed() {
    let mut world = World::new("alarm-wake");
    let giga = world.add_mono_in("alk-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alk-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    let input = json!({
        "message": "Is CI green?", "send_to": mono.id.as_str(),
        "wanting_reply": false, "intent": "new",
    });
    world.call(&giga, "g1", "wake-1", Tool::SendMessage, input, &mono);
    // First seen at the next pass, one slice on.
    world.advance(2 * MINUTE);
    assert_eq!(episodes(&world), Vec::new());
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        Episode {
            id: episode.id.clone(),
            actor: "mono:alk-mono".into(),
            provisioner: "mono:alk-general".into(),
            cause: "wake_missed".into(),
            dialog_text: None,
            open: true,
        }
    );
}

// Defect: a kill and restart mid-episode opens a second episode or writes
// a second alarm row, or a turn the killed app left running counts as hung.
#[test]
fn a_restart_mid_episode_keeps_its_rows_and_ends_the_turn_it_left_running() {
    let mut world = World::new("alarm-restart");
    let mono = world.add_mono_in("alr-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alr-lead");
    let worker = world.add_session("alr-worker", P, None);
    let claude = claude_tap(&world, &worker);
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(N - 5 * MINUTE);
    turn(&mut world, &worker, "t1", true);
    claude.line(Pipe::Stdout, r#"{"type":"system","subtype":"init"}"#);
    world.advance(5 * MINUTE);
    let before = episodes(&world);
    assert_eq!(before.len(), 1);
    let rows = alarm_rows(&world);

    world.kill();
    world.restart();
    world.advance(N);
    assert_eq!(
        episodes(&world),
        before,
        "same episode, and the worker not hung"
    );
    assert_eq!(alarm_rows(&world), rows, "no second alarm row");
}

// Defect: app downtime or a sleep counts toward N.
#[test]
fn app_downtime_and_sleep_do_not_count_toward_n() {
    let mut world = World::new("alarm-down");
    let mono = world.add_mono_in("alx-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "alx-lead");
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(10 * MINUTE);
    world.down(2 * HOUR);
    world.sleep(HOUR);
    world.advance(5 * MINUTE - SECOND);
    assert_eq!(episodes(&world), Vec::new(), "14:59 of active time");
    world.advance(SECOND);
    assert_eq!(episodes(&world).len(), 1);
}

// Defect: a database from before the alarms keeps the old stage CHECK, so
// the first alarm row fails, or the rebuild loses a pending reminder.
#[test]
fn an_old_outbox_takes_alarm_rows_and_keeps_its_reminders() {
    let dir = std::env::temp_dir().join(format!(
        "monocode-alarm-outbox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("monocode.db");
    drop(crate::session_store::SessionStore::open_at(path.clone(), 0).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE mono_outbox;
             CREATE TABLE mono_outbox (
               key          TEXT PRIMARY KEY,
               ask_id       TEXT REFERENCES mono_asks(id),
               stage        TEXT NOT NULL CHECK (stage IN
                              ('t5', 'deadline', 'why', 'blocked', 'resume')),
               recipient    TEXT NOT NULL,
               at_active_ms INTEGER NOT NULL,
               claimed_by   TEXT,
               sent_at      INTEGER
             );
             INSERT INTO mono_outbox (key, stage, recipient, at_active_ms)
               VALUES ('resume:s:1', 'resume', 'session:s', 7);",
        )
        .unwrap();
    let store = crate::session_store::SessionStore::open_at(path, 0).unwrap();
    let conn = store.lock_conn().unwrap();
    let kept: (String, i64) = conn
        .query_row(
            "SELECT stage, at_active_ms FROM mono_outbox WHERE key = 'resume:s:1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(kept, ("resume".into(), 7));
    for stage in ["alarm", "hold", "nudge"] {
        conn.execute(
            "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms) VALUES (?1, ?1, 'mono:m', 1)",
            [stage],
        )
        .unwrap();
    }
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

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

/// `episode`'s close reason, `None` while it is open.
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

/// The window saves `items` as `session`'s queue, as its session save does.
fn save_queue(world: &World, session: &SessionId, items: serde_json::Value, status: &str) {
    world
        .conn()
        .execute(
            "UPDATE sessions SET queued_messages_json = ?2, queue_status = ?3 WHERE id = ?1",
            rusqlite::params![session.as_str(), items.to_string(), status],
        )
        .unwrap();
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

// Defect (review M1): WAITING looks only at direct children, so a Mono
// whose lead ended its turn and waits on a running worker alarms at the
// lead's end + N while the worker builds.
#[test]
fn a_mono_and_its_idle_lead_wait_while_the_leads_worker_runs() {
    let mut world = World::new("alarm-transitive");
    world.add_mono_in("alt-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alt-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    let lead = start_lead(&world, &mono, "alt-lead");
    let worker = world.add_session("alt-worker", P, Some("alt-lead"));
    ran_a_turn(&mut world, &lead);
    turn(&mut world, &worker, "w1", true);
    world.advance(4 * N);
    assert_eq!(episodes(&world), Vec::new(), "the Mono and the lead wait");

    turn(&mut world, &worker, "w1", false);
    world.advance(N - PASS);
    assert_eq!(
        episodes(&world),
        Vec::new(),
        "the branch went quiet at the worker's end"
    );
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        idle_open_work(&episode.id, "session:alt-lead", "mono:alt-mono"),
        "the lead's episode is the one; its Mono waits on it"
    );
}

/// A turn of `session` that takes what waits for it, as the window starts
/// one, and ends after a minute.
fn took_a_turn(world: &mut World, session: &SessionId) {
    let turn_id = format!("turn-{}", world.active_ms());
    world.start_turn(session, &turn_id);
    turn(world, session, &turn_id, true);
    world.advance(MINUTE);
    world.end_turn(session, &turn_id, true);
    turn(world, session, &turn_id, false);
}

/// `from` asks `to` with the longest deadline, and `to`'s chat takes it.
fn ask_and_take(world: &mut World, from: &Mono, to: &Mono, key: &str) {
    let input =
        json!({"message": format!("Do {key}"), "send_to": to.id.as_str(), "due_minutes": 480});
    world.tool(&mono_caller(from, key), key, "ask", input, &[&to.chat]);
    took_a_turn(world, &to.chat);
}

const TODO_PENDING: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_t","name":"TodoWrite","input":{"todos":[{"content":"Ship","status":"pending"}]}}]}}"#;

// Defect (S1 r2 M1, lead cascade ruling): one worker's stall opens an
// episode at its lead, the lead's Mono and the Mono's Giga in the same
// pass (the Giga's alarm skipping the ladder), as each holds the open ask
// of the one above it, instead of one episode at the worker.
#[test]
fn a_stalled_worker_under_a_giga_mono_and_lead_is_one_episode() {
    let mut world = World::new("alarm-cascade");
    let general = world.add_mono_in("alc-general", MonoRole::Giga, &[P]);
    let giga = world.add_mono_in("alc-giga", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alc-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ask_and_take(&mut world, &general, &giga, "ask-giga");
    ask_and_take(&mut world, &giga, &mono, "ask-mono");
    let lead = start_lead(&world, &mono, "alc-lead");
    ran_a_turn(&mut world, &lead);
    let worker = world.add_session("alc-worker", P, Some("alc-lead"));
    let claude = claude_tap(&world, &worker);
    turn(&mut world, &worker, "w1", true);
    claude.line(Pipe::Stdout, TODO_PENDING);
    world.advance(2 * N);
    turn(&mut world, &worker, "w1", false);
    align(&mut world);
    world.advance(N - PASS);
    assert_eq!(episodes(&world), Vec::new());

    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        episode,
        idle_open_work(&episode.id, "session:alc-worker", "session:alc-lead")
    );
    world.advance(PASS);
    assert_eq!(episodes(&world), vec![episode.clone()], "and the next pass");

    // Defect (S1 r3 M1, lead ruling a): no window takes the alarm, and a
    // hold and a nudge wait claimed, so each recipient opens wake_missed and
    // the one stall climbs to Tyler in minutes.
    world
        .conn()
        .execute_batch(
            "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms, claimed_by)
             VALUES ('hold-alc', 'hold', 'session:alc-lead', 0, 'main'),
                    ('nudge-alc', 'nudge', 'mono:alc-mono', 0, 'main')",
        )
        .unwrap();
    world.advance(2 * N);
    assert_eq!(episodes(&world), vec![episode.clone()]);
    let (outbox, _) = alarm_rows(&world);
    assert_eq!(
        outbox,
        vec![(
            test_taps::outbox_key(&episode.id, 1),
            "session:alc-lead".to_string()
        )],
        "the alarm waits untaken"
    );
}

/// Sets `session`'s queue status as the window saves it, keeping its items.
fn set_queue_status(world: &World, session: &SessionId, status: &str) {
    world
        .conn()
        .execute(
            "UPDATE sessions SET queue_status = ?2 WHERE id = ?1",
            rusqlite::params![session.as_str(), status],
        )
        .unwrap();
}

/// One wake of each kind for `mono`'s chat (design section 4).
fn wake(world: &mut World, kind: &str, giga: &Mono, mono: &Mono) {
    match kind {
        "message" => {
            let input = json!({
                "message": "Is CI green?", "send_to": mono.id.as_str(),
                "wanting_reply": false, "intent": "new",
            });
            world.call(giga, "g1", "wake-1", Tool::SendMessage, input, mono);
        }
        "reminder" => {
            world
                .conn()
                .execute(
                    "INSERT INTO mono_outbox (key, stage, recipient, at_active_ms, claimed_by)
                     VALUES ('resume:alz:1', 'resume', ?1, 1, 'main')",
                    [format!("mono:{}", mono.id.as_str())],
                )
                .unwrap();
        }
        "notice" => {
            let saved = json!({
                "request": {"requestId": "req-1"}, "origin": {"monoId": mono.chat.as_str()},
                "notice": "notice-1",
            });
            world
                .conn()
                .execute(
                    "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
                     VALUES ('req-1', ?1, 'closed', 'old-run')",
                    [saved.to_string()],
                )
                .unwrap();
        }
        "queued" => save_queue(world, &mono.chat, json!([{"id": "queued-1"}]), "active"),
        other => panic!("{other}"),
    }
}

// Defect (S1 r2 F1): only a paused queue's own items are exempt, so a
// paused chat's claimed reminder, untaken message or completion result
// still alarms its Giga as wake_missed.
#[test]
fn a_paused_chat_misses_no_wake_on_any_path() {
    for kind in ["message", "reminder", "notice", "queued"] {
        let mut world = World::new(&format!("alarm-paused-{kind}"));
        let giga = world.add_mono_in("alz-general", MonoRole::Giga, &[P]);
        let mono = world.add_mono_in("alz-mono", MonoRole::Mono, &[P]);
        align(&mut world);
        ran_a_turn(&mut world, &mono.chat);
        wake(&mut world, kind, &giga, &mono);
        set_queue_status(&world, &mono.chat, "paused");
        world.advance(N);
        assert_eq!(episodes(&world), Vec::new(), "{kind}: paused on purpose");

        set_queue_status(&world, &mono.chat, "active");
        world.advance(2 * MINUTE + 2 * PASS);
        let causes: Vec<String> = episodes(&world)
            .into_iter()
            .map(|episode| episode.cause)
            .collect();
        assert_eq!(causes, ["wake_missed"], "{kind}: waits once resumed");
    }
}

/// Writes an open episode of `cause` for `actor`, as another row or a
/// newer build would.
fn foreign_episode(world: &World, actor: &str, cause: &str) {
    world
        .conn()
        .execute_batch(&format!(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO alarm_episodes (id, actor, provisioner, cause, open_work_json,
               stalled_since_active_ms, stalled_since_wall_ms, opened_active_ms, opened_wall_ms)
             VALUES ('al_foreign', '{actor}', 'mono:x', '{cause}', '[]', 1, 1, 1, 1);
             PRAGMA ignore_check_constraints = OFF;"
        ))
        .unwrap();
}

// Defect (S1 r2 F2): an open episode whose cause the detector does not
// parse (the sleep hold's `hold_missing`, or a newer build's) is dropped
// from the pass, so every pass tries a second open episode for the agent,
// fails on the unique index and skips the agent; the episode never closes.
#[test]
fn an_episode_of_a_cause_the_detector_does_not_judge_stays_until_its_agent_goes() {
    for cause in ["hold_missing", "a_newer_cause"] {
        let mut world = World::new(&format!("alarm-foreign-{cause}"));
        let mono = world.add_mono_in("alf-mono", MonoRole::Mono, &[P]);
        let lead = start_lead(&world, &mono, "alf-lead");
        align(&mut world);
        ran_a_turn(&mut world, &lead);
        foreign_episode(&world, "session:alf-lead", cause);
        world.advance(N + PASS);
        let open: Vec<(String, String)> = episodes(&world)
            .into_iter()
            .filter(|episode| episode.open)
            .map(|episode| (episode.id, episode.cause))
            .collect();
        assert_eq!(open, [("al_foreign".to_string(), cause.to_string())]);
        assert_eq!(
            world.store().alarms.last_fault(),
            None,
            "{cause}: no agent skipped"
        );

        world
            .conn()
            .execute("UPDATE sessions SET archived = 1 WHERE id = 'alf-lead'", [])
            .unwrap();
        world.advance(PASS);
        assert_eq!(
            close_reason(&world, "al_foreign").as_deref(),
            Some("agent_gone"),
            "{cause}"
        );
    }
}

// Defect (review F4): a notice or message the window saved into the
// chat's queue and no turn took never alarms; or (lead N5) the same item
// alarms again after every turn of its chat; or a queue the user paused
// alarms.
#[test]
fn a_queued_item_no_turn_took_is_wake_missed_once() {
    let mut world = World::new("alarm-queued");
    world.add_mono_in("alq-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alq-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    save_queue(
        &world,
        &mono.chat,
        json!([{"id": "notice-1", "text": "The lead finished"}]),
        "active",
    );
    world.advance(2 * MINUTE);
    assert_eq!(episodes(&world), Vec::new());
    world.advance(PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        (
            episode.actor.as_str(),
            episode.provisioner.as_str(),
            episode.cause.as_str()
        ),
        ("mono:alq-mono", "mono:alq-general", "wake_missed")
    );

    ran_a_turn(&mut world, &mono.chat);
    world.advance(N);
    assert_eq!(
        close_reason(&world, &episode.id).as_deref(),
        Some("progress")
    );
    assert_eq!(episodes(&world).len(), 1, "notice-1 alarmed once");

    save_queue(&world, &mono.chat, json!([{"id": "queued-2"}]), "paused");
    world.advance(N);
    assert_eq!(episodes(&world).len(), 1, "a paused queue waits on purpose");
}

// Defect (lead N5): a completion result that no window delivers alarms the
// Giga again after every turn of the Mono, or counts for a session that is
// no live Mono's chat.
#[test]
fn a_completion_result_alarms_once_and_only_for_a_live_mono_chat() {
    let mut world = World::new("alarm-notice");
    world.add_mono_in("aln-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("aln-mono", MonoRole::Mono, &[P]);
    let solo = world.add_session("aln-solo", P, None);
    align(&mut world);
    ran_a_turn(&mut world, &mono.chat);
    ran_a_turn(&mut world, &solo);
    for (request, chat) in [("req-1", mono.chat.as_str()), ("req-2", solo.as_str())] {
        let saved = json!({
            "request": {"requestId": request}, "origin": {"monoId": chat},
            "notice": format!("notice-{request}"),
        });
        world
            .conn()
            .execute(
                "INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
                 VALUES (?1, ?2, 'closed', 'old-run')",
                rusqlite::params![request, saved.to_string()],
            )
            .unwrap();
    }
    world.advance(2 * MINUTE + PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(
        (episode.actor.as_str(), episode.cause.as_str()),
        ("mono:aln-mono", "wake_missed")
    );
    for _ in 0..3 {
        ran_a_turn(&mut world, &mono.chat);
        world.advance(N);
    }
    assert_eq!(episodes(&world).len(), 1, "one alarm for the notice");
}

// Defect (review F5): a second process on the data folder runs migrate and
// idles the lease holder's running turns, so a working lead with an open
// ask alarms its Mono at N.
#[test]
fn a_second_process_leaves_the_lease_holders_running_turns() {
    let mut world = World::new("alarm-second");
    let mono = world.add_mono_in("al2-mono", MonoRole::Mono, &[P]);
    let lead = start_lead(&world, &mono, "al2-lead");
    align(&mut world);
    turn(&mut world, &lead, "t1", true);
    world.advance(PASS);
    assert!(!world.second_launch(), "the World holds the lease");
    let row = test_taps::row(&world.conn(), lead.as_str()).unwrap();
    assert_eq!(row.0, "running", "{row:?}");
    world.advance(2 * N);
    assert_eq!(episodes(&world), Vec::new());
}

// Defect (review F6): an episode under a Mono that no row resolves to any
// more (its chat was reset) stays open forever, and its unsent alarm keeps
// waiting for the Giga.
#[test]
fn an_episode_whose_mono_chat_was_reset_closes_as_agent_gone() {
    let mut world = World::new("alarm-gone");
    world.add_mono_in("alg-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alg-mono", MonoRole::Mono, &[P]);
    let claude = claude_tap(&world, &mono.chat);
    align(&mut world);
    turn(&mut world, &mono.chat, "t1", true);
    claude.line(
        Pipe::Stdout,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_t","name":"TodoWrite","input":{"todos":[{"content":"Ship","status":"pending"}]}}]}}"#,
    );
    world.advance(MINUTE);
    turn(&mut world, &mono.chat, "t1", false);
    world.advance(N + PASS);
    let [episode] = <[Episode; 1]>::try_from(episodes(&world)).unwrap();
    assert_eq!(episode.actor, "mono:alg-mono");
    assert_eq!(alarm_rows(&world).0.len(), 1);

    world.reset_mono_chat(&mono, "alg-mono-chat-2");
    world.advance(PASS);
    assert_eq!(
        close_reason(&world, &episode.id).as_deref(),
        Some("agent_gone")
    );
    assert_eq!(alarm_rows(&world).0, Vec::new(), "its unsent alarm goes");
}

// Defect (review M3): the pass scans per agent, so hundreds of watched
// sessions and a long sent history hold the store lock for much of every
// 5 s tick. Bounds: watched rows, open asks, unsent outbox rows, untaken
// messages; the sent outbox and message history are not read.
#[test]
fn a_pass_over_300_watched_sessions_stays_well_under_the_tick() {
    let mut world = World::new("alarm-perf");
    world.add_mono_in("alp-general", MonoRole::Giga, &[P]);
    let mono = world.add_mono_in("alp-mono", MonoRole::Mono, &[P]);
    align(&mut world);
    let mut sessions = Vec::new();
    for lead in 0..30 {
        let lead = start_lead(&world, &mono, &format!("alp-lead-{lead}"));
        for worker in 0..9 {
            let id = format!("{}-w{worker}", lead.as_str());
            sessions.push(world.add_session(&id, P, Some(lead.as_str())));
        }
        sessions.push(lead);
    }
    assert_eq!(sessions.len(), 300);
    for (n, session) in sessions.iter().enumerate() {
        let turn_id = format!("t{n}");
        turn(&mut world, session, &turn_id, true);
        if n % 3 != 0 {
            turn(&mut world, session, &turn_id, false);
        }
    }
    // A long history: sent outbox rows, acked asks and old start rows.
    let digits = "replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(
          printf('%012d', n), '0', 'a'), '1', 'b'), '2', 'c'), '3', 'd'), '4', 'e'), '5', 'f'),
          '6', 'g'), '7', 'h'), '8', 'i'), '9', 'j')";
    world
        .conn()
        .execute_batch(
            "WITH RECURSIVE k(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM k WHERE n < 30000)
             INSERT INTO mono_outbox (key, stage, recipient, at_active_ms, claimed_by, sent_at)
               SELECT 'resume:old:' || n, 'resume', 'session:alp-lead-' || (n % 30), n, 'main', n FROM k;
             WITH RECURSIVE k(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM k WHERE n < 8000)
             INSERT INTO mono_asks (id, parent_actor, child_actor, parent_level, child_level, state,
                 due_active_ms, stage, created_wall_ms)
               SELECT 'old-ask-' || n, parent_actor, child_actor, parent_level, child_level, 'acked',
                 due_active_ms, stage, created_wall_ms
               FROM mono_asks, k WHERE mono_asks.id = (SELECT min(id) FROM mono_asks);",
        )
        .unwrap();
    clone_messages(&world, 60_000, digits);
    world.advance(PASS);

    let mut slowest = std::time::Duration::ZERO;
    let running = world.running();
    for _ in 0..3 {
        let conn = world.conn();
        let sources = crate::alarms::Sources {
            live: &world.store().alarms,
            running: Some(&running),
        };
        let started = std::time::Instant::now();
        crate::alarms::run(&conn, world.active_ms(), world.now(), &sources).unwrap();
        slowest = slowest.max(started.elapsed());
    }
    eprintln!("alarm pass at 300 watched sessions: slowest of 3 {slowest:?}");
    assert!(
        slowest < std::time::Duration::from_millis(500),
        "{slowest:?} of a 5 s tick"
    );
}

/// Copies the oldest start row `count` times under new ids, as a long
/// message history.
fn clone_messages(world: &World, count: i64, digits: &str) {
    let conn = world.conn();
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('mono_messages')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let select: Vec<String> = columns
        .iter()
        .map(|column| match column.as_str() {
            "id" => format!("'m_' || {digits}"),
            "request_id" => "'old-' || n".to_string(),
            other => format!("m.{other}"),
        })
        .collect();
    conn.execute_batch(&format!(
        "WITH RECURSIVE k(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM k WHERE n < {count})
         INSERT INTO mono_messages ({}) SELECT {} FROM mono_messages m, k
          WHERE m.id = (SELECT id FROM mono_messages WHERE tool = 'start_session'
                         ORDER BY created_at LIMIT 1)",
        columns.join(", "),
        select.join(", ")
    ))
    .unwrap();
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

// Defect (S1 r3 M2, lead ruling b): a lead that backgrounds a wait and
// ends its turn with its Mono's ask open alarms at N while the job runs;
// or once the job ends, its clock does not restart from that end.
#[test]
fn an_idle_lead_on_a_background_task_alarms_at_four_n_or_n_after_it_ends() {
    for finish in [None, Some(2 * N)] {
        let mut world = World::new(&format!("alarm-idle-background-{}", finish.is_some()));
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

        let stalled_from = match finish {
            None => ended,
            Some(after) => {
                world.advance(after - N);
                claude.line(
                    Pipe::Stdout,
                    r#"{"type":"system","subtype":"task_notification","task_id":"bg1"}"#,
                );
                world.active_ms()
            }
        };
        let alarm_at = match finish {
            None => ended + 4 * N,
            Some(_) => stalled_from + N,
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
