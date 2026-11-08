//! Alarms S1: the stall detector runs on the active clock's own tick over
//! real rows. The turn and stdio taps are driven at the World's active
//! time, as `control.rs` and `harness.rs` drive them in the app.

use std::collections::HashMap;

use serde_json::json;

use super::agents::{mono_caller, Mono};
use super::world::World;
use crate::alarms::taps::{Harness, Pipe, Tap};
use crate::alarms::test_taps;
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

fn turn(world: &World, session: &SessionId, turn_id: &str, started: bool) {
    let conn = world.conn();
    let (at, wall) = (world.active_ms(), world.now());
    if started {
        test_taps::turn_started(&conn, session, turn_id, at, wall).unwrap();
    } else {
        test_taps::turn_ended(&conn, session, turn_id, at, wall).unwrap();
    }
}

/// A turn of `session` that ends at an aligned active time.
fn ran_a_turn(world: &mut World, session: &SessionId) {
    let turn_id = format!("turn-{}", world.active_ms());
    turn(world, session, &turn_id, true);
    world.advance(MINUTE);
    turn(world, session, &turn_id, false);
}

/// Line `line` reaches `session`'s tap at the World's active time, and the
/// row it hands back is written, as `alarms::tap_line` does.
fn tap(world: &World, session: &SessionId, tap: &mut Tap, pipe: Pipe, line: &str) {
    let now = world.active_ms();
    if let Some(row) = tap.line(pipe, line, now) {
        test_taps::write_tap(&world.conn(), session, &row, world.now()).unwrap();
        tap.wrote(row, now);
    }
}

#[derive(Debug, PartialEq, Eq)]
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
    turn(&world, &lead, "t1", true);
    turn(&world, &worker, "t1", true);
    world.advance(MINUTE);
    turn(&world, &lead, "t1", false);

    world.advance(4 * N);
    assert_eq!(episodes(&world), Vec::new());

    turn(&world, &worker, "t1", false);
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
    let mut claude = Tap::new(Harness::Claude);
    align(&mut world);
    turn(&world, &lead, "t1", true);
    tap(&world, &lead, &mut claude, Pipe::Stdout, PERMISSION);
    world.advance(3 * MINUTE - SECOND);
    tap(&world, &lead, &mut claude, Pipe::Stdin, ANSWER);
    world.advance(2 * MINUTE);
    assert_eq!(episodes(&world), Vec::new(), "answered at 2:59");

    align(&mut world);
    tap(&world, &lead, &mut claude, Pipe::Stdout, PERMISSION_2);
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
    let mut claude = Tap::new(Harness::Claude);
    align(&mut world);
    ran_a_turn(&mut world, &lead);
    world.advance(N - 5 * MINUTE);
    turn(&world, &worker, "t1", true);
    tap(
        &world,
        &worker,
        &mut claude,
        Pipe::Stdout,
        r#"{"type":"system","subtype":"init"}"#,
    );
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
    turn(&world, &lead, "turn-4", true);
    world.advance(MINUTE);
    turn(&world, &lead, "turn-5", true);
    turn(&world, &lead, "turn-4", false);
    world.advance(N);
    turn(&world, &lead, "turn-5", true);
    world.advance(N);
    assert_eq!(episodes(&world), Vec::new(), "turn-5 runs, untapped");
    turn(&world, &lead, "turn-5", false);
    world.advance(N);
    assert_eq!(
        episodes(&world).len(),
        1,
        "idle with its ask from turn-5's end"
    );
}
