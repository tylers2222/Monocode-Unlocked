//! ADE-138: omp runs again by itself when a background job ends. That run
//! gets the app access of the MonoCode turn before it, through the app's own
//! commands, loopback endpoint and token, and loses it when it ends.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::test::MockRuntime;
use tauri::{App, AppHandle, Listener, Manager, WebviewWindow};

use super::s5_scheduler::P;
use super::world::World;
use crate::control::{self, ControlHost};
use crate::mono_messages::tests::invoke_command;
use crate::mono_messages::{CallerArg, MessageId, SessionId, Submitted, Tool};
use crate::mono_roster::MonoRole;
use crate::session_store::SessionStore;

/// The app endpoint and token `session`'s provider process starts with.
fn provider_env(app: &AppHandle<MockRuntime>, session: &str) -> (String, String) {
    let mut cmd = Command::new("true");
    control::configure_child(app, session, &mut cmd);
    let env = |key: &str| {
        cmd.get_envs()
            .find(|(name, _)| *name == key)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap()
    };
    (env("MONOCODE_APP_ENDPOINT"), env("MONOCODE_APP_TOKEN"))
}

/// One `monocode app <action>` call over the loopback endpoint. `web_app`
/// runs each control event the window gets and returns its reply, which
/// goes back through `control_reply`. The CLI's answer, with the events.
fn app_call(
    (endpoint, token): &(String, String),
    action: &str,
    input: Value,
    request_id: &str,
    window: &WebviewWindow<MockRuntime>,
    events: &Receiver<Value>,
    mut web_app: impl FnMut(&Value) -> Value,
) -> (Value, Vec<Value>) {
    let request = json!({
        "token": token, "namespace": "app", "action": action,
        "input": input, "requestId": request_id,
    });
    let endpoint = endpoint.clone();
    let cli = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(&endpoint).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    });
    let mut seen = Vec::new();
    while !cli.is_finished() {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            let reply = json!({"id": event["id"], "response": web_app(&event)});
            invoke_command(window, "control_reply", &reply).unwrap();
            seen.push(event);
        }
    }
    (cli.join().unwrap(), seen)
}

/// The app on `world`'s database with the commands a turn's window calls,
/// its loopback endpoint up, window `main`, and the control events `main`
/// gets.
fn app_with_window(
    world: &World,
) -> (
    App<MockRuntime>,
    WebviewWindow<MockRuntime>,
    Receiver<Value>,
) {
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .invoke_handler(tauri::generate_handler![
            control::control_authorize_turn,
            control::control_authorize_harness_turn,
            control::control_turn_finished,
            control::control_reply
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    control::init(app.handle()).unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .unwrap();
    let (tx, events) = mpsc::channel();
    app.listen_any("monocode-control-request", move |event| {
        let _ = tx.send(serde_json::from_str::<Value>(event.payload()).unwrap());
    });
    (app, main, events)
}

/// What the web app runs for a control event of the lead, as the lead's call
/// in the event's turn (`callerOf`), through the store paths
/// `mono_message_submit` and `sessions.start` take. Adds each message it
/// sends to `sent`; returns its reply.
fn web_app(world: &World, mono: &SessionId, event: &Value, sent: &mut Vec<MessageId>) -> Value {
    let caller = json!({
        "kind": "session", "sessionId": event["sessionId"], "turnId": event["turnId"],
        "access": "started", "cwd": P,
    });
    let request_id = event["requestId"].as_str().unwrap();
    let input = event["input"].clone();
    match event["action"].as_str().unwrap() {
        "messages.send" => {
            let submitted = world
                .session_call(caller, request_id, Tool::SendMessage, input, &[mono])
                .unwrap();
            let Submitted::Accepted { messages, .. } = submitted else {
                panic!("the lead's message was refused: {submitted:?}");
            };
            sent.extend(messages.into_iter().map(|row| row.message.id));
            json!({"ok": true})
        }
        "sessions.start" => {
            let caller = serde_json::from_value::<CallerArg>(caller).unwrap();
            let started = world.start(&caller.parse().unwrap(), "wk-worker", P, request_id, &input);
            json!({"ok": started.is_ok()})
        }
        action => panic!("the lead called {action}"),
    }
}

/// Defect: a turn omp starts by itself has no app access, so a lead woken by
/// its background job cannot message its Mono or start a session
/// (`turn_inactive`); or the fix lets the token work outside any turn, past
/// the run's end, or lets the run take over a MonoCode turn; or Mono
/// delivery treats the woken lead as running a turn the window can steer.
#[test]
fn a_lead_woken_by_its_harness_messages_its_mono_and_starts_a_session() {
    let mut world = World::new("wake-turn");
    let mono = world.add_mono_in("wk-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "wk-lead", P);
    let (app, main, events) = app_with_window(&world);
    let cwd = world.path().parent().unwrap().to_str().unwrap().to_string();
    let authorize = |command: &str, turn_id: &str| {
        let args =
            json!({"sessionId": lead.as_str(), "cwd": cwd, "appAccess": true, "turnId": turn_id});
        invoke_command(&main, command, &args).unwrap();
    };
    let finish = |turn_id: &str| {
        let args = json!({"sessionId": lead.as_str(), "turnId": turn_id});
        invoke_command(&main, "control_turn_finished", &args).unwrap();
    };
    let report = json!({"message": "Job done", "send_to": mono.id.as_str(), "wanting_reply": false, "intent": "new"});
    let start = json!({"prompt": "Review it", "harness": "codex", "model": "codex:default"});

    // The Mono's sessions.start opened the lead's MonoCode turn, which ends.
    authorize("control_authorize_turn", "lead-t1");
    let token = provider_env(app.handle(), lead.as_str());
    finish("lead-t1");
    let mut sent = Vec::new();
    let mut call = |action: &str, input: &Value, id: &str| {
        app_call(&token, action, input.clone(), id, &main, &events, |event| {
            web_app(&world, &mono.chat, event, &mut sent)
        })
    };
    let (refused, _) = call("messages.send", &report, "between");
    assert_eq!(refused["code"], "turn_inactive", "no turn runs");

    // A background job ends; omp runs again with no prompt from MonoCode.
    authorize("control_authorize_harness_turn", "lead-wake");
    assert!(
        app.state::<ControlHost>().running_sessions().is_empty(),
        "Mono delivery sees the woken lead as idle, as before"
    );
    let (answer, seen) = call("messages.send", &report, "wake-send");
    assert_eq!(answer, json!({"ok": true}));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["sessionId"], lead.as_str());
    assert_eq!(
        seen[0]["turnId"], "lead-wake",
        "bound to the harness-started turn"
    );
    let (answer, _) = call("sessions.start", &start, "wake-start");
    assert_eq!(answer, json!({"ok": true}), "the lead starts a session");

    // The run ends.
    finish("lead-wake");
    let (refused, _) = call("messages.send", &report, "after");
    assert_eq!(
        refused["code"], "turn_inactive",
        "the run's end ends its access"
    );

    // A run inside a MonoCode turn stays part of that turn.
    authorize("control_authorize_turn", "lead-t2");
    authorize("control_authorize_harness_turn", "lead-wake-2");
    finish("lead-wake-2");
    let (_, seen) = call("messages.send", &report, "in-t2");
    assert_eq!(seen[0]["turnId"], "lead-t2");
    finish("lead-t2");

    // The Mono's next turn takes the lead's reports.
    let delivered: Vec<MessageId> = world
        .start_turn(&mono.chat, "mono-t1")
        .into_iter()
        .map(|row| row.message.id)
        .collect();
    assert_eq!(delivered, sent, "the reports reach the Mono");
}
