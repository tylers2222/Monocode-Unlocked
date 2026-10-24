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

/// `live` is the labels of the windows open at the boot.
fn claim(world: &World, window: &str, run: &str, live: &[&str]) -> Vec<Value> {
    completions::claim(&world.conn(), Owner { window, run }, live, world.now()).unwrap()
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

    hold(
        &world,
        "window-2",
        "run-2",
        &[sent("a", "notice-ab"), sent("b", "notice-ab")],
    );
    hold(&world, "window-2", "run-2", &[]);
    assert_eq!(
        rows(&world),
        2,
        "a result in a notice stays once the window drops it"
    );
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
        claim(&world, "main", "run-3", &["main"]).is_empty(),
        "a delivered result does not come back"
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
    assert_eq!(ids(&claim(&world, "main", "run-2", &["main"])), ["c"]);
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
    assert!(claim(&world, "main", "run-2", &["main"]).is_empty());
    assert_eq!(marks(&world), 1);
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

/// `mono_session_upsert`'s arguments: the Mono chat saved with `notice` queued.
fn chat_save(notice: &str) -> Value {
    let session = json!({
        "id": "mono-chat", "cwd": "/p", "harness": "fx", "model": "m", "modelSettings": {},
        "runtimeMode": "auto", "title": "Mono", "blocks": [],
        "queuedMessages": [{ "id": notice, "text": "MonoCode completion notification", "attachments": [] }],
    });
    json!({ "session": session, "afterBlockId": null, "fromBlockId": null, "blocksChanged": false, "appendOnly": false })
}

/// The commands as a window calls them, through Tauri's argument binding.
/// The other window's claim sees `main` open, so it takes nothing.
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
            crate::mono_transcript::mono_session_upsert
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
    assert_eq!(held(&booted), ["w"], "that window keeps its own result");
}
