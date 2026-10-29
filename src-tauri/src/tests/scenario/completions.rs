//! ADE-89: a Mono completion result a window owes survives a kill, a reload
//! and a closed window, reaches one window only, and is delivered once: by
//! the save of the Mono chat that holds its notice.

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

/// A result already sent in notice `notice`.
fn sent(request: &str, notice: &str) -> Value {
    let mut entry = saved(request, Some("done"));
    entry["notice"] = json!(notice);
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

/// `earlier`: the window restores the earlier run. `live` is the labels of
/// the windows open at the boot.
fn claim(world: &World, window: &str, run: &str, earlier: bool, live: &[&str]) -> Vec<Value> {
    completions::claim(
        &world.conn(),
        Owner { window, run },
        earlier,
        live,
        world.now(),
    )
    .unwrap()
}

fn hold(world: &World, window: &str, run: &str, entries: &[Value]) {
    completions::hold(&world.conn(), Owner { window, run }, entries).unwrap();
}

const BOTH: &[&str] = &["main", "window-2"];

/// The Mono chat's row with `queue` as its saved queue, written as a crash
/// left it: no Mono chat save ran after it.
fn chat_row(world: &World, queue: &Value) {
    world
        .conn()
        .execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, queued_messages_json)
             VALUES ('mono-chat', '/p', 'fx', 'm', 'auto', 'Mono', 0, 0, ?1)
             ON CONFLICT (id) DO UPDATE SET queued_messages_json = excluded.queued_messages_json",
            params![queue.to_string()],
        )
        .unwrap();
}

/// The Mono chat is saved with `queue`, and the save settles its notices.
fn save_chat(world: &World, queue: &Value) {
    chat_row(world, queue);
    completions::settle(&world.conn(), Some("mono-chat"), world.now()).unwrap();
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
    chat_row(&world, &json!([]));
    assert!(
        claim(&world, "main", "run-1", true, &["main"]).is_empty(),
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
    let first = claim(&world, "window-2", "run-2", true, BOTH);
    assert_eq!(
        ids(&first),
        ["a", "b"],
        "the first window to boot takes the killed run's results"
    );
    assert!(
        claim(&world, "main", "run-2", false, BOTH).is_empty(),
        "the second window gets nothing"
    );
    let reloaded = claim(&world, "window-2", "run-2", true, BOTH);
    assert_eq!(
        ids(&reloaded),
        ["a", "b"],
        "a reloaded window takes back what its earlier boot held"
    );

    hold(&world, "main", "run-2", &[saved("a", Some("stolen"))]);
    let held = claim(&world, "window-2", "run-2", true, BOTH);
    assert!(
        held[0].get("result").is_none(),
        "another window cannot rewrite a held result"
    );

    hold(
        &world,
        "window-2",
        "run-2",
        &[sent("a", "notice-ab"), sent("b", "notice-ab")],
    );
    hold(&world, "window-2", "run-2", &[]);
    save_chat(&world, &json!([{ "id": "notice-other" }]));
    assert_eq!(
        (rows(&world), marks(&world)),
        (2, 0),
        "nothing is marked while the Mono chat does not hold the notice"
    );
    save_chat(&world, &json!([{ "id": "notice-ab" }]));
    assert_eq!(
        (rows(&world), marks(&world)),
        (0, 1),
        "the save that holds the notice marks it and deletes its results"
    );
    hold(&world, "window-2", "run-2", &[sent("a", "notice-ab")]);

    world.kill();
    world.restart();
    assert!(
        claim(&world, "main", "run-3", true, &["main"]).is_empty(),
        "a delivered result does not come back"
    );
    world
        .conn()
        .execute(
            "UPDATE mono_completion_deliveries SET delivered_at = ?1",
            params![world.now() - 2_592_000_001],
        )
        .unwrap();
    claim(&world, "main", "run-3", true, &["main"]);
    assert_eq!(marks(&world), 0, "a claim drops marks older than 30 days");
}

/// Review r2 M1: the Mono stopped watching a session, before or after its
/// notice was queued. No save will hold a result for it, so its rows go,
/// and only that Mono's, that session's and that window's.
#[test]
fn a_session_the_mono_stopped_watching_keeps_no_row() {
    let mut world = World::new("ade89-dismiss");
    chat_row(&world, &json!([]));
    let other_session = json!({
        "origin": { "monoId": "mono-chat", "turn": 3 },
        "request": { "requestId": "c", "sessionId": "b", "project": "/p", "prompt": "Build it." },
    });
    hold(
        &world,
        "main",
        "run-1",
        &[
            sent("a", "notice-ab"),
            sent("b", "notice-ab"),
            other_session,
        ],
    );
    hold(&world, "window-2", "run-1", &[saved("d", None)]);
    let owner = Owner {
        window: "main",
        run: "run-1",
    };
    completions::dismiss(&world.conn(), owner, "other-mono", "a").unwrap();
    completions::dismiss(
        &world.conn(),
        Owner {
            window: "main",
            run: "run-0",
        },
        "mono-chat",
        "a",
    )
    .unwrap();
    assert_eq!(
        rows(&world),
        4,
        "another Mono's or another run's dismissal deletes nothing"
    );
    completions::dismiss(&world.conn(), owner, "mono-chat", "a").unwrap();
    completions::dismiss(&world.conn(), owner, "mono-chat", "d").unwrap();
    assert_eq!(
        rows(&world),
        3,
        "only the stopped session's row in that window goes"
    );
    completions::dismiss(&world.conn(), owner, "mono-chat", "b").unwrap();
    world.kill();
    world.restart();
    assert_eq!(
        ids(&claim(&world, "main", "run-2", true, BOTH)),
        ["d"],
        "a relaunch gives the Mono no result on a session it stopped"
    );
}

/// Review r2 M1: a removed Mono's rows, in a notice or still owed, come back
/// to no boot: a claim deletes the rows it takes whose Mono has no chat.
#[test]
fn a_claim_deletes_the_rows_of_a_mono_with_no_chat() {
    let mut world = World::new("ade89-no-chat");
    chat_row(&world, &json!([]));
    let gone = json!({
        "origin": { "monoId": "removed-mono", "turn": 1 },
        "request": { "requestId": "g", "sessionId": "g", "project": "/p", "prompt": "Build it." },
        "notice": "mono-completion-g",
    });
    hold(&world, "main", "run-1", &[saved("a", None), gone.clone()]);
    hold(
        &world,
        "window-2",
        "run-1",
        &[json!({ "origin": { "monoId": "removed-mono", "turn": 1 },
        "request": { "requestId": "h", "sessionId": "h", "project": "/p", "prompt": "Build it." } })],
    );
    assert_eq!(
        ids(&claim(&world, "main", "run-1", true, BOTH)),
        ["a"],
        "the claiming window keeps only the rows of a Mono that has a chat"
    );
    assert_eq!(
        rows(&world),
        2,
        "a row another open window holds stays its own"
    );
    world.kill();
    world.restart();
    assert_eq!(ids(&claim(&world, "main", "run-2", true, &["main"])), ["a"]);
    assert_eq!(
        rows(&world),
        1,
        "no boot gets a removed Mono's results back"
    );
}

/// Review r1 M2: a and b went out in one notice that the Mono chat saved,
/// c still ran, and the app died before that save settled them. The boot
/// delivers a and b and gives back only c, so c is reported once and a and
/// b are not sent again.
#[test]
fn a_claim_delivers_what_the_mono_chat_saved_and_returns_the_rest() {
    let mut world = World::new("ade89-part-release");
    hold(
        &world,
        "main",
        "run-1",
        &[
            sent("a", "notice-ab"),
            sent("b", "notice-ab"),
            saved("c", None),
        ],
    );
    chat_row(&world, &json!([{ "id": "notice-ab" }]));
    world.kill();
    world.restart();
    assert_eq!(ids(&claim(&world, "main", "run-2", true, &["main"])), ["c"]);
    assert_eq!((rows(&world), marks(&world)), (1, 1));
}

/// A boot that finds a notice in the Mono's transcript, not its queue,
/// delivers it too: the queue ran it before the save that settles it.
#[test]
fn a_claim_delivers_a_notice_the_mono_transcript_holds() {
    let world = World::new("ade89-transcript");
    hold(&world, "main", "run-1", &[sent("a", "notice-a")]);
    chat_row(&world, &json!([]));
    world
        .conn()
        .execute_batch(
            "INSERT INTO mono_transcripts (session_id) VALUES ('mono-chat');
             INSERT INTO mono_blocks (session_id, seq, block_id, block_json, search_text, is_user, is_draft)
             VALUES ('mono-chat', 1, 'u1', '{\"id\":\"u1\",\"appRequestId\":\"notice-a\"}', '', 1, 0);",
        )
        .unwrap();
    assert!(claim(&world, "main", "run-2", true, &["main"]).is_empty());
    assert_eq!(marks(&world), 1);
}

/// m1: window-4 closed in this run. A result's notice opens the Mono chat
/// in the window that takes it, and only main, the window that restored the
/// run, holds the Mono chats (ADE-93), so a window that boots later takes
/// none of window-4's results and main takes them all.
#[test]
fn a_closed_windows_results_go_only_to_the_window_that_restored_the_mono_chats() {
    let world = World::new("ade89-closed-window");
    chat_row(&world, &json!([]));
    let open = ["main", "window-4"];
    assert!(claim(&world, "main", "run-1", true, &open).is_empty());
    assert!(claim(&world, "window-4", "run-1", false, &open).is_empty());
    hold(&world, "window-4", "run-1", &[saved("c", None)]);
    assert!(
        claim(&world, "main", "run-1", true, &open).is_empty(),
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
    let now_open = ["main", "window-5"];
    assert!(
        claim(&world, "window-5", "run-1", false, &now_open).is_empty(),
        "a window that boots later takes none of them"
    );
    let taken = claim(&world, "main", "run-1", true, &now_open);
    assert_eq!(
        ids(&taken),
        ["c", "d"],
        "the window that holds the Mono chats takes them all"
    );
    assert!(
        taken[0].get("result").is_some(),
        "with the closing window's newest results"
    );
    hold(&world, "main", "run-1", &[saved("e", None)]);
    assert_eq!(
        ids(&claim(&world, "main", "run-1", true, &now_open)),
        ["e"],
        "a hold replaces what the window held"
    );
}

#[test]
fn the_first_claim_moves_the_results_a_quit_kept_in_the_workspace_row() {
    let mut world = World::new("ade89-workspace-row");
    chat_row(&world, &json!([]));
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
        ids(&claim(&world, "main", "run-1", true, BOTH)),
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
        claim(&world, "window-2", "run-1", false, BOTH).is_empty(),
        "the second window gets nothing"
    );

    world.kill();
    world.restart();
    assert_eq!(
        ids(&claim(&world, "main", "run-2", true, &["main"])),
        ["a"],
        "the adopted result survives a kill once"
    );
    assert_eq!(rows(&world), 1);
}

/// window-3 closed in this run, and main, the window that restores the run,
/// and window-2 boot at once: each result goes to one window.
#[test]
fn two_windows_booting_at_once_take_each_result_once() {
    let world = World::new("ade89-race");
    chat_row(&world, &json!([]));
    let many: Vec<Value> = (0..40).map(|n| saved(&format!("r{n}"), None)).collect();
    completions::hold(
        &world.conn(),
        Owner {
            window: "window-3",
            run: "run-2",
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
                    window == "main",
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

/// `mono_session_upsert`'s arguments: the Mono chat saved with `notice` queued.
#[cfg(not(windows))]
fn chat_save(notice: &str) -> Value {
    let session = json!({
        "id": "mono-chat", "cwd": "/p", "harness": "fx", "model": "m", "modelSettings": {},
        "runtimeMode": "auto", "title": "Mono", "blocks": [],
        "queuedMessages": [{ "id": notice, "text": "MonoCode completion notification", "attachments": [] }],
    });
    json!({ "session": session, "afterBlockId": null, "fromBlockId": null, "blocksChanged": false, "appendOnly": false })
}

#[cfg(not(windows))]
type Window = tauri::WebviewWindow<tauri::test::MockRuntime>;

/// One app process on `world`'s database, with the commands a window's boot
/// and saves call: each launch is a new run.
#[cfg(not(windows))]
fn launch(world: &World) -> tauri::App<tauri::test::MockRuntime> {
    use crate::session_store::{self as store, SessionStore};
    tauri::test::mock_builder()
        .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
        .manage(completions::Run::default())
        .manage(crate::control::tests::host())
        .invoke_handler(tauri::generate_handler![
            completions::mono_completions_claim,
            completions::mono_completions_hold,
            completions::mono_completions_dismiss,
            crate::mono_transcript::mono_session_upsert,
            store::session_set_in_flight,
            store::session_list_in_flight,
            store::workspace_set_snapshot,
            store::workspace_get_snapshot
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap()
}

/// A launch with a window per label.
#[cfg(not(windows))]
fn launch_windows(
    world: &World,
    labels: &[&str],
) -> (tauri::App<tauri::test::MockRuntime>, Vec<Window>) {
    let app = launch(world);
    let windows = labels.iter().map(|label| window(&app, label)).collect();
    (app, windows)
}

#[cfg(not(windows))]
fn window(app: &tauri::App<tauri::test::MockRuntime>, label: &str) -> Window {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .build()
        .unwrap()
}

/// `command`'s answer through Tauri's argument binding, as the web app calls it.
#[cfg(not(windows))]
fn call(window: &Window, command: &str, args: Value) -> Value {
    crate::mono_messages::tests::invoke_command(window, command, &args).unwrap()
}

/// The request ids `window`'s boot claims.
#[cfg(not(windows))]
fn held(window: &Window) -> Vec<String> {
    let claimed = call(window, "mono_completions_claim", json!({}));
    ids(claimed.as_array().unwrap())
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Review r2 M1, as the window calls the commands: a's result went out in a
/// notice, then the Mono stopped a's session before that notice was queued.
/// The window deletes a's row with the hold that drops it, so the relaunch
/// gives the Mono no second report of a.
#[cfg(not(windows))]
#[test]
fn a_result_the_window_drops_is_not_reported_again_after_a_relaunch() {
    let world = World::new("ade89-drop");
    chat_row(&world, &json!([]));
    let app = launch(&world);
    let main = window(&app, "main");
    assert!(held(&main).is_empty());
    call(
        &main,
        "mono_completions_hold",
        json!({ "saved": [sent("a", "mono-completion-a"), saved("b", None)] }),
    );
    call(
        &main,
        "mono_completions_dismiss",
        json!({ "monoId": "mono-chat", "sessionId": "a" }),
    );
    call(
        &main,
        "mono_completions_hold",
        json!({ "saved": [saved("b", None)] }),
    );
    drop(app);

    let relaunched = launch(&world);
    assert_eq!(
        held(&window(&relaunched, "main")),
        ["b"],
        "the relaunch gives back only the result the Mono still waits for"
    );
}

/// Review r2 m1, as the window calls the commands: the hold that names a's
/// notice reaches Rust before the Mono chat save that holds the notice. A
/// kill between the two leaves a's row and no save, so the relaunch gives a
/// back once, and the save after it delivers a for good.
#[cfg(not(windows))]
#[test]
fn a_kill_between_the_noticed_hold_and_the_mono_chat_save_loses_nothing() {
    let world = World::new("ade89-hold-then-save");
    chat_row(&world, &json!([]));
    let app = launch(&world);
    let main = window(&app, "main");
    assert!(held(&main).is_empty());
    let noticed = json!({ "saved": [sent("a", "mono-completion-a")] });
    call(&main, "mono_completions_hold", noticed.clone());
    drop(app);

    let app = launch(&world);
    let main = window(&app, "main");
    assert_eq!(
        held(&main),
        ["a"],
        "the relaunch gives back the result no Mono chat save holds"
    );
    call(&main, "mono_completions_hold", noticed);
    call(&main, "mono_session_upsert", chat_save("mono-completion-a"));
    assert_eq!(
        (rows(&world), marks(&world)),
        (0, 1),
        "the save that holds the notice delivers it"
    );
    drop(app);

    let app = launch(&world);
    assert!(
        held(&window(&app, "main")).is_empty(),
        "a delivered result does not come back"
    );
}

/// The commands as a window calls them, through Tauri's argument binding.
/// The other window's claim sees `main` open, so it takes nothing.
#[cfg(not(windows))]
#[test]
fn each_window_claims_holds_and_delivers_its_own_results_through_the_commands() {
    use crate::session_store::SessionStore;
    use tauri::Manager;

    let world = World::new("ade89-commands");
    chat_row(&world, &json!([]));
    let app = launch(&world);
    let (main, other) = (window(&app, "main"), window(&app, "window-1"));
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
    call(
        &main,
        "mono_completions_hold",
        json!({ "saved": [sent("a", "mono-completion-a")] }),
    );
    call(
        &other,
        "mono_session_upsert",
        chat_save("mono-completion-a"),
    );
    assert_eq!(
        (rows(&world), marks(&world)),
        (0, 1),
        "the Mono chat save that holds the notice delivers it, from any window"
    );

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

    // window-2 boots and holds `w` while main's claim waits on the store
    // lock. The claim reads the open windows under that lock, so it sees
    // window-2 and leaves `w` to it.
    let store = app.state::<SessionStore>();
    let lock = store.lock_conn().unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let racing = main.clone();
    let waited = std::thread::spawn(move || {
        started.send(()).unwrap();
        held(&racing).len()
    });
    waiting.recv().unwrap();
    // Time for a claim that read the windows before the lock to read them
    // now, before window-2 exists. The fixed claim passes without it.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let booted = window(&app, "window-2");
    let owner = Owner {
        window: "window-2",
        run: &run,
    };
    let open = ["main", "window-1", "window-2"];
    completions::claim(&lock, owner, false, &open, world.now()).unwrap();
    completions::hold(&lock, owner, &[saved("w", Some("done"))]).unwrap();
    drop(lock);
    assert_eq!(
        waited.join().unwrap(),
        0,
        "a claim that waited on the lock leaves the window that opened meanwhile its rows"
    );
    assert_eq!(held(&booted), ["w"], "that window keeps its own result");
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
    let (app, windows) = launch_windows(&world, &["main", "window-0"]);
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

    let (app, windows) = launch_windows(&world, &["main", "window-0"]);
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
    let opened = window(&app, "window-1");
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

    let (_app, windows) = launch_windows(&world, &["main", "window-0"]);
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

/// ADE-89 on ADE-93. A Mono completion result's notice opens the Mono chat
/// in the window that holds the result, so after a relaunch only the window
/// that restores the earlier run, and with it the Mono chat, takes the
/// results that run left: those of a window that boots again, of one that
/// does not, and those an older build kept in the workspace row. Here
/// window-0's claim comes after main's restore.
#[cfg(not(windows))]
#[test]
fn a_relaunch_gives_the_mono_completions_to_the_window_that_restores_the_mono_chat() {
    let world = World::new("ade89-one-owner");
    chat_row(&world, &json!([]));
    let (app, windows) = launch_windows(&world, &["main", "window-0", "window-1"]);
    call(
        &windows[1],
        "mono_completions_hold",
        json!({ "saved": [saved("a", None)] }),
    );
    call(
        &windows[2],
        "mono_completions_hold",
        json!({ "saved": [saved("b", None)] }),
    );
    drop((windows, app));
    let older = json!({ "tabs": [], "monoCompletions": [saved("c", None)] });
    world
        .conn()
        .execute(
            "INSERT INTO workspace_snapshot (id, snapshot_json, updated_at) VALUES (1, ?1, 1)",
            [older.to_string()],
        )
        .unwrap();

    let (_app, windows) = launch_windows(&world, &["main", "window-0"]);
    let [main, second] = [&windows[0], &windows[1]];
    call(main, "workspace_get_snapshot", json!({}));
    assert!(
        held(second).is_empty(),
        "the other window takes none of the last run's results: its own window's, a window not open now, or the workspace row's"
    );
    assert_eq!(
        held(main),
        ["a", "b", "c"],
        "the window that restored the last run takes them"
    );
}
