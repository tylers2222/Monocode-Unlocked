//! ADE-89: a Mono completion result a window owes survives a kill, a reload
//! and a closed window, reaches one window only, and is delivered once.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::world::World;
use crate::mono_completions::{self as completions, Owner};

fn saved(request: &str, result: Option<&str>) -> Value {
    let mut entry = json!({
        "origin": { "monoId": "mono-chat", "turn": 3 },
        "request": { "requestId": request, "sessionId": request, "project": "/p", "prompt": "Build it." },
    });
    if let Some(text) = result {
        entry["result"] = json!({ "sessionId": request, "status": "completed", "result": text });
    }
    entry
}

fn ids(held: &[Value]) -> Vec<&str> {
    held.iter()
        .map(|entry| {
            entry
                .pointer("/request/requestId")
                .and_then(Value::as_str)
                .unwrap()
        })
        .collect()
}

fn rows(world: &World) -> i64 {
    world
        .conn()
        .query_row("SELECT count(*) FROM mono_completions", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// `live` is the labels of the windows open at the boot.
fn claim(world: &World, window: &str, run: &str, live: &[&str]) -> Vec<Value> {
    completions::claim(&world.conn(), Owner { window, run }, live, world.now()).unwrap()
}

fn hold(world: &World, window: &str, run: &str, entries: &[Value]) {
    completions::hold(&world.conn(), Owner { window, run }, entries).unwrap();
}

const BOTH: &[&str] = &["main", "window-2"];

fn deliver(world: &World, window: &str, run: &str, requests: &[&str]) -> bool {
    let requests: Vec<String> = requests.iter().map(ToString::to_string).collect();
    let group = format!("mono-completion-{}", requests.join("+"));
    completions::deliver(
        &world.conn(),
        Owner { window, run },
        world.now(),
        &group,
        &requests,
    )
    .unwrap()
}

fn marks(world: &World) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_completion_deliveries",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_waiting_result_reaches_one_window_once_across_kills_and_reloads() {
    let mut world = World::new("ade89-completions");
    assert!(
        claim(&world, "main", "run-1", &["main"]).is_empty(),
        "a new database holds nothing"
    );
    hold(
        &world,
        "main",
        "run-1",
        &[saved("a", None), saved("b", Some("done"))],
    );

    world.kill();
    world.restart();
    let first = claim(&world, "window-2", "run-2", BOTH);
    assert_eq!(
        ids(&first),
        ["a", "b"],
        "the first window to boot takes the killed run's results"
    );
    assert!(
        claim(&world, "main", "run-2", BOTH).is_empty(),
        "the second window gets nothing"
    );
    let reloaded = claim(&world, "window-2", "run-2", BOTH);
    assert_eq!(
        ids(&reloaded),
        ["a", "b"],
        "a reloaded window takes back what its earlier boot held"
    );

    hold(&world, "main", "run-2", &[saved("a", Some("stolen"))]);
    let held = claim(&world, "window-2", "run-2", BOTH);
    assert!(
        held[0].get("result").is_none(),
        "another window cannot rewrite a held result"
    );
    assert!(
        !deliver(&world, "main", "run-2", &["a", "b"]),
        "another window cannot deliver it"
    );
    assert_eq!(marks(&world), 0, "a refused delivery leaves no mark");
    assert!(
        deliver(&world, "window-2", "run-2", &["a", "b"]),
        "the holder delivers it"
    );
    assert_eq!(
        (rows(&world), marks(&world)),
        (0, 1),
        "the delivery deletes the results and marks the group in one commit"
    );
    assert!(
        !deliver(&world, "window-2", "run-2", &["a", "b"]),
        "the same group is delivered once"
    );

    world.kill();
    world.restart();
    assert!(
        claim(&world, "main", "run-3", &["main"]).is_empty(),
        "a delivered result does not come back"
    );
    assert!(
        !deliver(&world, "main", "run-3", &["a", "b"]),
        "a claim keeps a recent delivery mark"
    );
    world
        .conn()
        .execute(
            "UPDATE mono_completion_deliveries SET delivered_at = ?1",
            params![world.now() - 2_592_000_001],
        )
        .unwrap();
    claim(&world, "main", "run-3", &["main"]);
    assert_eq!(marks(&world), 0, "a claim drops marks older than 30 days");
}

#[test]
fn a_closed_windows_results_go_to_the_next_window_that_boots() {
    let world = World::new("ade89-closed-window");
    let open = ["main", "window-4"];
    assert!(claim(&world, "window-4", "run-1", &open).is_empty());
    hold(&world, "window-4", "run-1", &[saved("c", None)]);
    assert!(
        claim(&world, "main", "run-1", &open).is_empty(),
        "an open window keeps its results"
    );
    // window-4 closes. Its last hold reaches Rust after that: its result for
    // c and a new d.
    hold(
        &world,
        "window-4",
        "run-1",
        &[saved("c", Some("done")), saved("d", None)],
    );
    let taken = claim(&world, "main", "run-1", &["main"]);
    assert_eq!(
        ids(&taken),
        ["c", "d"],
        "the next window to boot takes them all"
    );
    assert!(
        taken[0].get("result").is_some(),
        "with the closing window's newest results"
    );
    hold(&world, "main", "run-1", &[saved("e", None)]);
    assert_eq!(
        ids(&claim(&world, "main", "run-1", &["main"])),
        ["e"],
        "a hold replaces what the window held"
    );
}

#[test]
fn the_first_claim_moves_the_results_a_quit_kept_in_the_workspace_row() {
    let mut world = World::new("ade89-workspace-row");
    let row = json!({
        "tabs": [{ "id": "t1" }],
        "activeTabId": "t1",
        "monoCompletions": [saved("a", None), { "origin": {}, "request": {} }],
    });
    world
        .conn()
        .execute(
            "INSERT INTO workspace_snapshot (id, snapshot_json, updated_at) VALUES (1, ?1, 0)",
            params![row.to_string()],
        )
        .unwrap();
    world.kill();
    world.restart();
    let opened: Value = world
        .conn()
        .query_row("SELECT snapshot_json FROM workspace_snapshot", [], |r| {
            r.get::<_, String>(0)
        })
        .map(|json| serde_json::from_str(&json).unwrap())
        .unwrap();
    assert!(
        opened.get("monoCompletions").is_some(),
        "opening the store leaves the row to the first claim"
    );

    assert_eq!(
        ids(&claim(&world, "main", "run-1", BOTH)),
        ["a"],
        "the claim adopts each well-formed result"
    );
    let after: Value = world
        .conn()
        .query_row("SELECT snapshot_json FROM workspace_snapshot", [], |r| {
            r.get::<_, String>(0)
        })
        .map(|json| serde_json::from_str(&json).unwrap())
        .unwrap();
    assert_eq!(
        after,
        json!({ "tabs": [{ "id": "t1" }], "activeTabId": "t1" }),
        "the row keeps its workspace and carries no results"
    );
    assert!(
        claim(&world, "window-2", "run-1", BOTH).is_empty(),
        "the second window gets nothing"
    );

    world.kill();
    world.restart();
    assert_eq!(
        ids(&claim(&world, "main", "run-2", &["main"])),
        ["a"],
        "the adopted result survives a kill once"
    );
    assert_eq!(rows(&world), 1);
}

#[test]
fn two_windows_booting_at_once_take_each_result_once() {
    let world = World::new("ade89-race");
    let many: Vec<Value> = (0..40).map(|n| saved(&format!("r{n}"), None)).collect();
    completions::hold(
        &world.conn(),
        Owner {
            window: "main",
            run: "run-1",
        },
        &many,
    )
    .unwrap();
    let path = world.path().to_path_buf();
    let now = world.now();
    let boots: Vec<_> = ["main", "window-2"]
        .into_iter()
        .map(|window| {
            let path = path.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                let held = completions::claim(
                    &conn,
                    Owner {
                        window,
                        run: "run-2",
                    },
                    BOTH,
                    now,
                )
                .unwrap();
                held.iter()
                    .map(|entry| {
                        entry
                            .pointer("/request/requestId")
                            .and_then(Value::as_str)
                            .unwrap()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut taken: Vec<String> = boots
        .into_iter()
        .flat_map(|boot| boot.join().unwrap())
        .collect();
    let claimed = taken.len();
    taken.sort();
    taken.dedup();
    assert_eq!(
        (claimed, taken.len()),
        (40, 40),
        "every result goes to exactly one window"
    );
}

/// The three commands as a window calls them, through Tauri's argument
/// binding. The other window's claim sees `main` open, so it takes nothing.
#[cfg(not(windows))]
#[test]
fn each_window_claims_holds_and_delivers_its_own_results_through_the_commands() {
    use crate::mono_messages::tests::invoke_command;
    use crate::session_store::SessionStore;
    use tauri::Manager;

    let world = World::new("ade89-commands");
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .manage(completions::Run::default())
        .invoke_handler(tauri::generate_handler![
            completions::mono_completions_claim,
            completions::mono_completions_hold,
            completions::mono_completions_deliver
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
    let held = |window| {
        let claimed = call(window, "mono_completions_claim", json!({}));
        ids(claimed.as_array().unwrap())
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let deliver = |window, request: &str| {
        let args =
            json!({ "groupId": format!("mono-completion-{request}"), "requestIds": [request] });
        call(window, "mono_completions_deliver", args) == json!(true)
    };

    call(
        &main,
        "mono_completions_hold",
        json!({ "saved": [saved("a", None)] }),
    );
    assert_eq!(
        held(&main),
        ["a"],
        "the holding window claims its result back"
    );
    assert!(held(&other).is_empty(), "the other window gets nothing");
    assert!(!deliver(&other, "a"), "the other window cannot deliver it");
    assert!(deliver(&main, "a"), "the holder delivers it");
    assert!(!deliver(&main, "a"), "once");

    call(
        &main,
        "mono_completions_hold",
        json!({ "saved": [saved("b", None)] }),
    );
    let run: String = world
        .conn()
        .query_row("SELECT window_run FROM mono_completions", [], |row| {
            row.get(0)
        })
        .unwrap();
    call(&main, "mono_completions_hold", json!({ "saved": [] }));
    assert!(
        held(&main).is_empty(),
        "a result the window no longer owes is gone"
    );

    // window-2 boots and holds `w` while window-1's claim waits on the
    // store lock. The claim reads the open windows under that lock, so it
    // sees window-2 and leaves `w` to it.
    let store = app.state::<SessionStore>();
    let lock = store.lock_conn().unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let racing = other.clone();
    let waited = std::thread::spawn(move || {
        started.send(()).unwrap();
        let claimed = invoke_command(&racing, "mono_completions_claim", &json!({})).unwrap();
        ids(claimed.as_array().unwrap()).len()
    });
    waiting.recv().unwrap();
    // Time for a claim that read the windows before the lock to read them
    // now, before window-2 exists. The fixed claim passes without it.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let booted = window("window-2");
    let owner = Owner {
        window: "window-2",
        run: &run,
    };
    let open = ["main", "window-1", "window-2"];
    completions::claim(&lock, owner, &open, world.now()).unwrap();
    completions::hold(&lock, owner, &[saved("w", Some("done"))]).unwrap();
    drop(lock);
    assert_eq!(
        waited.join().unwrap(),
        0,
        "a claim that waited on the lock leaves the window that opened meanwhile its rows"
    );
    assert!(deliver(&booted, "w"), "that window delivers its own result");
}

#[cfg(not(windows))]
type Window = tauri::WebviewWindow<tauri::test::MockRuntime>;

/// A launch of the app on `world`'s database, a new run, with a window per
/// label and the commands a window's boot and save call.
#[cfg(not(windows))]
fn launch(world: &World, labels: &[&str]) -> (tauri::App<tauri::test::MockRuntime>, Vec<Window>) {
    use crate::session_store::{self as store, SessionStore};
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .manage(crate::control::tests::host())
        .invoke_handler(tauri::generate_handler![
            store::session_set_in_flight,
            store::session_list_in_flight,
            store::workspace_set_snapshot,
            store::workspace_get_snapshot
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let windows = labels.iter().map(|label| open(&app, label)).collect();
    (app, windows)
}

#[cfg(not(windows))]
fn open(app: &tauri::App<tauri::test::MockRuntime>, label: &str) -> Window {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .build()
        .unwrap()
}

#[cfg(not(windows))]
fn call(window: &Window, command: &str, args: Value) -> Value {
    crate::mono_messages::tests::invoke_command(window, command, &args).unwrap()
}

/// A window's snapshot: one tab showing `tab`, and the chats it holds.
#[cfg(not(windows))]
fn workspace(tab: &str, chats: &[&str]) -> Value {
    json!({
        "tabs": [{ "id": format!("tab-{tab}"), "layout": { "type": "leaf", "id": tab } }],
        "sessions": chats.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>(),
        "activeTabId": format!("tab-{tab}"),
        "projectCwd": "/p",
    })
}

#[cfg(not(windows))]
fn save(window: &Window, snapshot: &Value, read_at: i64) {
    let args = json!({ "snapshot": snapshot, "readAt": read_at });
    call(window, "workspace_set_snapshot", args);
}

/// The chats a window's boot holds: its snapshot rows' stubs and its
/// in-flight chats, which it binds.
#[cfg(not(windows))]
fn holds(rows: &Value, in_flight: &Value) -> Vec<String> {
    let mut ids: Vec<String> = rows
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row["sessions"].as_array().unwrap().iter())
        .chain(in_flight.as_array().unwrap())
        .map(|chat| {
            let id = chat.get("id").or_else(|| chat.get("sessionId"));
            id.and_then(Value::as_str).unwrap().to_owned()
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// ADE-93. After a relaunch, main and window-0 boot before either saves, and
/// each restores through the real commands, the two reads interleaved. A
/// window binds every chat it restores, and its turn on one (a Continue, a
/// queued notice) starts a second harness child for the chat, which ends the
/// first window's turn (`harness.rs`, one child per session). So one window
/// holds the Mono chat; the other copies the tabs, as New Window does.
#[cfg(not(windows))]
#[test]
fn a_relaunch_gives_the_mono_chat_to_one_window() {
    use crate::session_store::now_millis;
    let world = World::new("ade93-one-owner");
    let chat = world
        .add_mono("ade93-mono", crate::mono_roster::MonoRole::Mono)
        .chat;
    let chat = chat.as_str();
    let restored = |window: &Window| call(window, "workspace_get_snapshot", json!({}));
    let in_flight = |window: &Window| call(window, "session_list_in_flight", json!({}));

    let start = now_millis() - 100_000;
    let (app, windows) = launch(&world, &["main", "window-0"]);
    let mut second_saved = workspace("side", &[chat, "side"]);
    second_saved["monoCompletions"] = json!([{ "origin": { "monoId": "ade93-mono", "turn": 1 } }]);
    save(&windows[0], &workspace("work", &[chat, "work"]), start);
    save(&windows[1], &second_saved, start + 10);
    let running = json!([{ "sessionId": chat, "cwd": "/" }, { "sessionId": "work", "cwd": "/p" }]);
    call(
        &windows[0],
        "session_set_in_flight",
        json!({ "sessions": running }),
    );
    call(
        &windows[1],
        "session_set_in_flight",
        json!({ "sessions": [{ "sessionId": "side", "cwd": "/p" }] }),
    );
    drop((windows, app));

    let (app, windows) = launch(&world, &["main", "window-0"]);
    let [main, second] = [&windows[0], &windows[1]];
    let main_rows = restored(main);
    let second_in_flight = in_flight(second);
    let second_rows = restored(second);
    assert_eq!(
        main_rows[0]["monoCompletions"], second_saved["monoCompletions"],
        "the first window to boot restores the Mono's completion batches"
    );
    assert!(
        !holds(&second_rows, &second_in_flight).contains(&chat.to_owned()),
        "the other window does not hold the Mono chat the first one restored"
    );
    assert_eq!(
        second_rows[0].get("monoCompletions"),
        None,
        "the other window's copy holds no Mono completion batch, which would load the Mono chat there"
    );
    assert_eq!(
        second_rows[0]["tabs"], second_saved["tabs"],
        "the other window copies the newest tabs, as New Window does"
    );

    save(second, &second_rows[0], now_millis() - 100);
    call(second, "session_set_in_flight", json!({ "sessions": [] }));
    let none = json!([]);
    assert_eq!(
        holds(&restored(main), &none),
        [chat, "side", "work"],
        "the first window to boot restores the last run's tabs whole, also on a reload after the other window saved first"
    );
    assert_eq!(
        holds(&none, &in_flight(main)),
        [chat, "side", "work"],
        "the other window's save leaves the last run's running chats, its own too, to the first window"
    );
    let relaunched = workspace("work", &[chat, "side", "work"]);
    save(main, &relaunched, now_millis() - 50);
    assert_eq!(
        holds(&restored(main), &in_flight(main)),
        [chat, "side", "work"],
        "a reload of the first window keeps its Mono chat"
    );
    let opened = open(&app, "window-1");
    let copied = restored(&opened);
    assert_eq!(
        holds(&copied, &in_flight(&opened)),
        ["side", "work"],
        "a window opened later copies the chats but not the Mono chat another window holds"
    );
    assert_eq!(copied[0]["tabs"], relaunched["tabs"]);
}

/// ADE-93 r1. An older build saved last, so its shared row (no `windowRows`)
/// is what every window restores. The first window to boot restores it
/// whole, with the Mono chat and its completion batch; the other window
/// copies it without them.
#[cfg(not(windows))]
#[test]
fn an_older_builds_row_gives_the_mono_chat_to_one_window() {
    let world = World::new("ade93-older-row");
    let chat = world
        .add_mono("ade93-older-mono", crate::mono_roster::MonoRole::Mono)
        .chat;
    let chat = chat.as_str();
    let mut older = workspace("work", &[chat, "work"]);
    older["monoCompletions"] = json!([{ "origin": { "monoId": "ade93-older-mono", "turn": 1 } }]);
    world
        .conn()
        .execute(
            "INSERT INTO workspace_snapshot (id, snapshot_json, updated_at) VALUES (1, ?1, 1)",
            [older.to_string()],
        )
        .unwrap();

    let (_app, windows) = launch(&world, &["main", "window-0"]);
    let restored = |window: &Window| call(window, "workspace_get_snapshot", json!({}));
    assert_eq!(
        restored(&windows[0]),
        json!([older]),
        "the first window to boot restores the older build's row whole"
    );
    assert_eq!(
        restored(&windows[1]),
        json!([workspace("work", &["work"])]),
        "the other window copies it without the Mono chat or its completion batch"
    );
}
