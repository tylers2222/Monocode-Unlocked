//! ADE-140/141: one window holds a chat. Harness children are keyed by
//! session id for the whole process, so a second window's turn on a chat
//! would install a second child and end the first window's
//! (`HarnessHost::install_spawn`). Each scenario drives the real commands
//! through Tauri's argument binding, as the web app calls them, with a
//! stand-in fx CLI that stays running like a provider between turns.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::{json, Value};
use tauri::{Listener, Manager};

use super::agent_sandbox::script;
use super::world::World;
use crate::harness::HarnessHost;
use crate::mono_messages::tests::invoke_command;

type Window = tauri::WebviewWindow<tauri::test::MockRuntime>;
type App = tauri::App<tauri::test::MockRuntime>;

/// The app's commands on `world`'s database, its app data folder `data`
/// (Tauri names it after the identifier, and an absolute one is the path).
fn launch(world: &World, data: &Path) -> App {
    use crate::session_store::{self as store, SessionStore};
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = data.to_string_lossy().into_owned();
    tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .manage(HarnessHost::new())
        .manage(crate::control::tests::host())
        .invoke_handler(tauri::generate_handler![
            crate::harness::harness_spawn,
            crate::control::control_loaded_sessions,
            crate::control::control_authorize_turn,
            crate::control::control_window_booted,
            crate::control::control_turn_finished,
            store::session_set_in_flight,
            store::session_list_in_flight,
            store::workspace_set_snapshot,
            store::workspace_get_snapshot
        ])
        .build(context)
        .unwrap()
}

fn window(app: &App, label: &str) -> Window {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .build()
        .unwrap()
}

fn call(window: &Window, command: &str, args: Value) -> Result<Value, Value> {
    invoke_command(window, command, &args)
}

/// A provider CLI that answers the resolver's probes and then stays up.
struct Rig {
    root: PathBuf,
    fx: String,
}

impl Rig {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let fx = script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) exec sleep 120 ;;
esac"#,
        );
        Self {
            root,
            fx: fx.to_string_lossy().into_owned(),
        }
    }

    /// `window` starts the harness child of chat `session`: its pid, or the
    /// refusal.
    fn spawn(&self, window: &Window, session: &str) -> Result<u32, String> {
        let args = json!({
            "sessionId": session,
            "command": self.fx,
            "args": ["acp"],
            "cwd": self.root.to_string_lossy(),
            "binaryProvider": "fx",
            "binaryPath": self.fx,
        });
        call(window, "harness_spawn", args)
            .map(|pid| u32::try_from(pid.as_u64().unwrap()).unwrap())
            .map_err(|error| error.as_str().unwrap().to_owned())
    }

    /// `window` starts a turn of `session`, as a send or a queued message
    /// does before it writes to the child.
    fn turn(&self, window: &Window, session: &str) -> Result<Value, Value> {
        let args = json!({
            "sessionId": session,
            "cwd": self.root.to_string_lossy(),
            "appAccess": false,
            "turnId": format!("turn-{session}"),
        });
        call(window, "control_authorize_turn", args)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Whether process `pid` still runs: present and not a zombie.
fn running(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
}

/// The chats `window` reports it has loaded, as its sessions change: the
/// ones another window holds, with that window.
fn load(window: &Window, chats: &[&str]) -> Value {
    call(
        window,
        "control_loaded_sessions",
        json!({ "sessions": chats }),
    )
    .unwrap()
}

const HELD_ELSEWHERE: &str = "Another window has this chat open.";

/// ADE-140. A relaunch with a Mono open: main and window-1 boot. main
/// restores the earlier run (ADE-93): its snapshot tabs, its running chat and
/// the Mono chat. window-1 copies the tabs, reopens the open Mono
/// (`loadOpenMono`), and reports before main does. main holds every chat it
/// restored; window-1 binds none of them, and its turn on any is refused.
#[test]
fn a_relaunch_with_a_mono_open_gives_its_chats_to_the_restoring_window() {
    let world = World::new("ade140-relaunch");
    let chat = world
        .add_mono("ade140-mono", crate::mono_roster::MonoRole::Mono)
        .chat;
    let chat = chat.as_str();
    let rig = Rig::new("ade140-relaunch");
    let data = rig.root.join("app-data");

    let app = launch(&world, &data);
    let before = window(&app, "main");
    let saved = json!({
        "tabs": [{ "id": "tab-work", "layout": { "type": "leaf", "id": "work" } }],
        "sessions": [{ "id": chat }, { "id": "work" }],
        "activeTabId": "tab-work",
    });
    let read_at = crate::session_store::now_millis() - 1_000;
    call(
        &before,
        "workspace_set_snapshot",
        json!({ "snapshot": saved, "readAt": read_at }),
    )
    .unwrap();
    let in_flight = json!([{ "sessionId": "running", "cwd": "/p" }]);
    call(
        &before,
        "session_set_in_flight",
        json!({ "sessions": in_flight }),
    )
    .unwrap();
    drop((before, app));

    let app = launch(&world, &data);
    let (main, second) = (window(&app, "main"), window(&app, "window-1"));
    call(&main, "workspace_get_snapshot", json!({})).unwrap();
    call(&main, "session_list_in_flight", json!({})).unwrap();
    call(&second, "workspace_get_snapshot", json!({})).unwrap();
    assert_eq!(
        load(&second, &[chat, "work", "running"]),
        json!({ chat: "main", "work": "main", "running": "main" }),
        "the restoring window holds the Mono chat, its snapshot chats and its running chats before it reports"
    );
    assert_eq!(load(&main, &[chat, "work", "running"]), json!({}));

    assert_eq!(
        rig.spawn(&second, chat),
        Err(HELD_ELSEWHERE.into()),
        "the other window starts no harness child of the Mono chat"
    );
    assert_eq!(
        rig.turn(&second, "work"),
        Err(json!(HELD_ELSEWHERE)),
        "nor a turn of a chat the restoring window holds"
    );
    let pid = rig.spawn(&main, chat).expect("the holding window spawns");
    assert!(running(pid));

    call(&main, "control_window_booted", json!({})).unwrap();
    assert_eq!(
        load(&second, &[chat, "work", "running"]),
        json!({}),
        "a reload lets go of the window's chats, and the other window that has them loaded takes them"
    );
    call(&main, "workspace_get_snapshot", json!({})).unwrap();
    assert_eq!(
        load(&main, &[chat, "work", "running"]),
        json!({ chat: "window-1", "work": "window-1", "running": "window-1" }),
        "the reloaded window restores the earlier run again but takes back no chat"
    );
    crate::control::window_closed(app.handle(), "window-1");
    assert_eq!(
        load(&main, &[chat, "work", "running"]),
        json!({}),
        "a closed window's chats pass to the window that has them loaded"
    );
    app.state::<HarnessHost>().kill_all();
}

/// ADE-141. window-3 runs tab chat S; New Window (window-12, a label that
/// sorts first) copies S. Its turn on S is refused and window-3's child
/// keeps running. window-3 drops S while its turn runs: when the turn ends,
/// window-12 takes S by itself and its spawn starts.
#[test]
fn a_copied_chat_stays_with_its_window_until_that_window_lets_go() {
    let world = World::new("ade141-copied");
    let rig = Rig::new("ade141-copied");
    let app = launch(&world, &rig.root.join("app-data"));
    let first = window(&app, "window-3");
    call(&first, "workspace_get_snapshot", json!({})).unwrap();
    assert_eq!(load(&first, &["tab-chat"]), json!({}));
    let pid = rig.spawn(&first, "tab-chat").unwrap();

    let opened = window(&app, "window-12");
    call(&opened, "workspace_get_snapshot", json!({})).unwrap();
    assert_eq!(
        load(&opened, &["tab-chat"]),
        json!({ "tab-chat": "window-3" }),
        "the first window to load a chat holds it, whatever its label"
    );
    rig.turn(&first, "tab-chat").unwrap();
    assert_eq!(rig.spawn(&opened, "tab-chat"), Err(HELD_ELSEWHERE.into()));
    assert_eq!(
        rig.spawn(&opened, "tab-chat#1"),
        Err(HELD_ELSEWHERE.into()),
        "nor a child keyed under the chat"
    );
    assert!(running(pid), "the holding window's child keeps running");

    assert_eq!(
        load(&first, &[]),
        json!({}),
        "window-3 closes the tab mid-turn"
    );
    assert_eq!(
        load(&opened, &["tab-chat"]),
        json!({ "tab-chat": "window-3" }),
        "the window of a running turn holds its chat"
    );
    let (moved, changed) = mpsc::channel();
    app.listen_any("control-holds-changed", move |_| {
        let _ = moved.send(());
    });
    let finished = json!({ "sessionId": "tab-chat", "turnId": "turn-tab-chat" });
    call(&first, "control_turn_finished", finished).unwrap();
    changed
        .try_recv()
        .expect("the other windows hear that the turn's end passed the chat on");
    assert_eq!(
        load(&opened, &["tab-chat"]),
        json!({}),
        "the window that has the chat loaded now holds it"
    );
    let taken = rig.spawn(&opened, "tab-chat").expect("its turn starts");
    assert!(running(taken));
    app.state::<HarnessHost>().kill_all();
}
