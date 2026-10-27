//! S5.5 (R9): each reminder reaches its chat once. The tick names the chat,
//! one window claims the reminder with the delivery the chat's live turn
//! picks, and the provider taking it marks it sent. A window close or a
//! kill before that leaves it pending for exactly one more delivery.

use std::collections::HashMap;

use rusqlite::params;

use super::agents::{mono_caller, session_caller};
use super::s5_scheduler::{advance_to, ask, due, t5_key, MINUTE, P, T5_LEAD};
use super::world::World;
use crate::ask_scheduler::{self, Claimed};
use crate::mono_messages::{self, MessageId, Notices, SessionId};
use crate::mono_roster::MonoRole;

/// A later stage's row to a Mono (R15 writes `blocked:<ask>:<seq>`) that
/// this build does not send: no window lists or claims it.
const BLOCKED: &str = "blocked:s5d:1";
/// A t5 row to a Mono, so a Mono's reminder is checked to reach its current
/// chat.
const MONO_T5: &str = "reminder:s5d-mono:t5";

/// The exact prompt a claimed t5 reminder of ask `ask` hands the agent.
fn t5_text(ask: &MessageId) -> String {
    let id = ask.as_str();
    format!(
        "<mono_reminder ask_id=\"{id}\" stage=\"t5\">\nAsk {id} is due in 5 active minutes. When its work is done, ack it with this ask_id.\n</mono_reminder>"
    )
}

fn wake(ask: &MessageId) -> Option<Claimed> {
    Some(Claimed {
        text: t5_text(ask),
        wake: true,
    })
}

/// The chats `notices` names, sorted.
fn named(notices: &Notices) -> Vec<String> {
    let value = serde_json::to_value(notices).unwrap();
    let mut chats: Vec<String> = serde_json::from_value(value["sessions"].clone()).unwrap();
    chats.sort();
    chats
}

fn keys(world: &World, chat: &SessionId) -> Vec<String> {
    ask_scheduler::pending(&world.conn(), chat).unwrap()
}

fn claim(
    world: &World,
    key: &str,
    window: &str,
    running: &[&SessionId],
    steerable: bool,
) -> Option<Claimed> {
    let running: HashMap<String, String> = running
        .iter()
        .map(|session| (session.as_str().to_string(), "t1".to_string()))
        .collect();
    ask_scheduler::claim(&world.conn(), key, window, &running, steerable).unwrap()
}

fn sent(world: &World, key: &str, window: &str) -> bool {
    ask_scheduler::sent(&world.conn(), key, window, world.now()).unwrap()
}

/// `(claimed_by, sent_at)` of reminder `key`.
fn row(world: &World, key: &str) -> (Option<String>, Option<i64>) {
    world
        .conn()
        .query_row(
            "SELECT claimed_by, sent_at FROM mono_outbox WHERE key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// S5.5 "a kill in the middle of sending a reminder: after restart it is
/// sent exactly once". Defect: a tick or a turn end that leaves a reminder's
/// chat unnamed; a busy chat that cannot steer gets the reminder in its
/// turn, or one that can is made to wait; a claim that hands the agent
/// another text, or a stage this build does not send; two windows both claim one
/// reminder; a window that does not hold a reminder marks it sent, or one
/// reminder is marked twice; a closed window or a killed app keeps its
/// unsent claim, so the reminder never goes out, or frees a sent one, so it
/// goes out twice.
#[test]
fn s5_reminders_are_claimed_once_sent_once_and_freed_by_close_and_boot() {
    let mut world = World::new("s5d");
    let mono = world.add_mono_in("s5d-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "s5d-lead", P);
    let worker = world.add_session("s5d-worker", P, Some("s5d-lead"));
    let live = [&lead, &worker];
    let a = ask(
        &world,
        &live,
        &mono_caller(&mono, "m1"),
        "s5d-a",
        lead.as_str(),
        10,
    );
    let b = ask(
        &world,
        &live,
        &session_caller(&lead, P),
        "s5d-b",
        worker.as_str(),
        10,
    );
    let (ka, kb) = (t5_key(&a), t5_key(&b));
    world
        .conn()
        .execute(
            "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
             VALUES (?1, NULL, 'blocked', ?3, 0), (?2, ?4, 't5', ?3, 0)",
            params![
                BLOCKED,
                MONO_T5,
                format!("mono:{}", mono.id.as_str()),
                a.as_str()
            ],
        )
        .unwrap();

    // The tick that enqueues the reminders names their chats.
    let ta = due(&world, &a) - T5_LEAD;
    assert_eq!(due(&world, &b) - T5_LEAD, ta);
    advance_to(&mut world, ta - 1);
    let tick = world.try_advance(1).unwrap();
    assert_eq!(named(&tick), [lead.as_str(), worker.as_str()]);
    assert_eq!(keys(&world, &lead), [ka.as_str()]);
    assert_eq!(keys(&world, &worker), [kb.as_str()]);
    assert_eq!(
        keys(&world, &mono.chat),
        [MONO_T5],
        "a stage not sent waits unlisted"
    );
    assert_eq!(claim(&world, BLOCKED, "main", &[], false), None);
    delivery_follows_the_chat(&mut world, (&ka, &a), (&lead, &worker, &mono.chat));
    only_the_holder_marks_it_sent(&world, &ka);
    close_and_boot_free_unsent_claims(&mut world, (&ka, &kb), (&lead, &worker, &mono.chat));
}

/// A busy chat that cannot take a steer waits and its turn end names it; a
/// busy Mono chat that can steer takes it in its turn; an idle chat wakes,
/// and of two windows only the first gets the reminder.
fn delivery_follows_the_chat(
    world: &mut World,
    (ka, a): (&str, &MessageId),
    (lead, worker, mono_chat): (&SessionId, &SessionId, &SessionId),
) {
    assert_eq!(claim(world, ka, "main", &[lead], false), None);
    assert_eq!(
        row(world, ka),
        (None, None),
        "an after-turn reminder stays unclaimed"
    );
    assert_eq!(named(&world.end_turn(lead, "t1", false)), [lead.as_str()]);
    assert_eq!(
        claim(world, MONO_T5, "second", &[mono_chat], true),
        Some(Claimed {
            text: t5_text(a),
            wake: false
        }),
        "a steer joins the running turn"
    );
    assert_eq!(claim(world, ka, "main", &[worker], true), wake(a));
    assert_eq!(claim(world, ka, "second", &[], false), None, "one claim");
    assert!(keys(world, lead).is_empty());
    assert!(
        named(&world.end_turn(lead, "t2", false)).is_empty(),
        "nothing waits"
    );
}

fn only_the_holder_marks_it_sent(world: &World, ka: &str) {
    assert!(!sent(world, ka, "second"));
    assert!(sent(world, ka, "main"));
    assert!(!sent(world, ka, "main"), "sent once");
    assert_eq!(row(world, ka), (Some("main".into()), Some(world.now())));
}

/// The window closes before the provider took b: b waits again, a stays
/// sent, and the Mono's row stays with the window `second` that holds it.
/// Then the app is killed after the claims of b and the Mono's row and
/// before their sent marks: after the restart both wait, and each goes out
/// exactly once.
fn close_and_boot_free_unsent_claims(
    world: &mut World,
    (ka, kb): (&str, &str),
    (lead, worker, mono_chat): (&SessionId, &SessionId, &SessionId),
) {
    assert!(claim(world, kb, "main", &[], false).is_some());
    let closed = mono_messages::release_window_slots(&world.conn(), "main", world.now()).unwrap();
    assert_eq!(named(&closed), [worker.as_str()], "only main's claims");
    assert_eq!(keys(world, worker), [kb]);
    assert!(
        !sent(world, kb, "main"),
        "a freed claim is not the window's"
    );
    assert_eq!(row(world, ka), (Some("main".into()), Some(world.now())));
    assert_eq!(
        row(world, MONO_T5),
        (Some("second".into()), None),
        "another window's claim stays"
    );
    assert!(keys(world, mono_chat).is_empty());

    assert!(claim(world, kb, "main", &[], false).is_some());
    world.down(MINUTE);
    assert_eq!(keys(world, worker), [kb]);
    assert_eq!(keys(world, mono_chat), [MONO_T5]);
    assert!(keys(world, lead).is_empty(), "a sent reminder stays sent");
    for key in [kb, MONO_T5] {
        assert!(claim(world, key, "main", &[], false).is_some());
        assert!(sent(world, key, "main"));
    }
    let sent_rows: i64 = world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_outbox WHERE sent_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sent_rows, 3, "each reminder sent once");
}

/// The three R9 commands as windows call them, through Tauri's argument
/// binding on the World's database. Defect: a command that drops what the
/// store answers: the due reminder is not listed, no window gets the claim,
/// or the sent mark is taken always or never.
#[cfg(not(windows))]
#[test]
fn windows_list_claim_and_mark_a_reminder_through_the_commands() {
    use crate::mono_messages::tests::invoke_command;
    use crate::session_store::SessionStore;
    use serde_json::{json, Value};

    let mut world = World::new("s5d-commands");
    let mono = world.add_mono_in("s5dc-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "s5dc-lead", P);
    let worker = world.add_session("s5dc-worker", P, Some("s5dc-lead"));
    let b = ask(
        &world,
        &[&lead, &worker],
        &session_caller(&lead, P),
        "s5dc-b",
        worker.as_str(),
        10,
    );
    let kb = t5_key(&b);
    let at = due(&world, &b) - T5_LEAD;
    advance_to(&mut world, at);

    let app = tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .manage(crate::control::tests::host())
        .invoke_handler(tauri::generate_handler![
            ask_scheduler::mono_outbox_pending,
            ask_scheduler::mono_outbox_claim,
            ask_scheduler::mono_outbox_sent
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = |label: &str| {
        tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::default())
            .build()
            .unwrap()
    };
    let (main, other) = (window("main"), window("window-1"));
    let call = |window, command: &str, args: Value| invoke_command(window, command, &args).unwrap();
    let pending = |chat: &SessionId| {
        call(
            &main,
            "mono_outbox_pending",
            json!({ "sessionId": chat.as_str() }),
        )
    };
    let claim = |window| {
        call(
            window,
            "mono_outbox_claim",
            json!({ "key": kb, "steerable": false }),
        )
    };
    let sent = |window| call(window, "mono_outbox_sent", json!({ "key": kb }));

    assert_eq!(pending(&worker), json!([kb]));
    assert_eq!(
        pending(&lead),
        json!([]),
        "only its recipient's chat lists it"
    );
    assert_eq!(
        claim(&main),
        json!({ "text": t5_text(&b), "wake": true }),
        "the idle chat wakes with the reminder's prompt"
    );
    assert_eq!(claim(&other), Value::Null, "one claim");
    assert_eq!(sent(&other), json!(false), "only the holder marks it sent");
    assert_eq!(sent(&main), json!(true));
    assert_eq!(sent(&main), json!(false), "sent once");
}

/// The app's own launch path (`active_clock::init`) on a killed World's data
/// folder. Defect: a launch that skips the active clock: the lease stays
/// free for a second process, and the stored clock and the downtime log
/// stay as the killed app left them.
#[cfg(not(windows))]
#[test]
fn the_app_launch_takes_the_lease_and_carries_the_clock_on() {
    use crate::session_store::SessionStore;
    use tauri::Manager;

    let mut world = World::new("s5d-init");
    world.advance(10_000);
    let clock = |conn: &rusqlite::Connection| -> (i64, i64) {
        conn.query_row(
            "SELECT active_ms, last_wall_ms FROM mono_clock WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    let downtime = |conn: &rusqlite::Connection| -> Vec<(i64, i64)> {
        let mut rows = conn
            .prepare("SELECT start_wall_ms, active_ms FROM mono_downtime ORDER BY id")
            .unwrap();
        rows.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let (active_ms, last_wall_ms) = clock(&world.conn());
    assert!(active_ms > 0, "the killed app stored its active time");
    let mut logged = downtime(&world.conn());
    world.kill();

    // An absolute identifier makes the app's data folder the World's folder.
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = world.path().parent().unwrap().display().to_string();
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .build(context)
        .unwrap();
    crate::active_clock::init(app.handle()).unwrap();

    let store = app.state::<SessionStore>();
    let conn = store.lock_conn().unwrap();
    assert_eq!(clock(&conn).0, active_ms, "the clock goes on, not from 0");
    logged.push((last_wall_ms, active_ms));
    assert_eq!(downtime(&conn), logged, "the time down is logged once");
    drop(conn);
    assert!(!world.second_launch(), "the launched app holds the lease");
}
