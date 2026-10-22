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

fn claim(world: &World, window: &str, run: &str) -> Vec<Value> {
    completions::claim(&world.conn(), Owner { window, run }, world.now()).unwrap()
}

fn save(world: &World, window: &str, run: &str, entries: &[Value]) {
    completions::save(&world.conn(), Owner { window, run }, world.now(), entries).unwrap();
}

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
        claim(&world, "main", "run-1").is_empty(),
        "a new database holds nothing"
    );
    save(
        &world,
        "main",
        "run-1",
        &[saved("a", None), saved("b", Some("done"))],
    );

    world.kill();
    world.restart();
    let first = claim(&world, "window-2", "run-2");
    assert_eq!(
        ids(&first),
        ["a", "b"],
        "the first window to boot takes the killed run's results"
    );
    assert!(
        claim(&world, "main", "run-2").is_empty(),
        "the second window gets nothing"
    );
    let reloaded = claim(&world, "window-2", "run-2");
    assert_eq!(
        ids(&reloaded),
        ["a", "b"],
        "a reloaded window takes back what its earlier boot held"
    );

    save(&world, "main", "run-2", &[saved("a", Some("stolen"))]);
    let held = claim(&world, "window-2", "run-2");
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
        claim(&world, "main", "run-3").is_empty(),
        "a delivered result does not come back"
    );
}

#[test]
fn a_closed_windows_results_go_to_the_next_window_that_boots() {
    let world = World::new("ade89-closed-window");
    assert!(claim(&world, "window-4", "run-1").is_empty());
    save(&world, "window-4", "run-1", &[saved("c", None)]);
    assert!(
        claim(&world, "main", "run-1").is_empty(),
        "a live window keeps its results"
    );
    let closing = Owner {
        window: "window-4",
        run: "run-1",
    };
    completions::release(&world.conn(), closing).unwrap();
    assert_eq!(
        ids(&claim(&world, "main", "run-1")),
        ["c"],
        "the next window to boot takes them"
    );
    let main = Owner {
        window: "main",
        run: "run-1",
    };
    completions::remove(&world.conn(), main, &["c".to_string()]).unwrap();
    assert_eq!(rows(&world), 0, "a dismissed result is gone");
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
        ids(&claim(&world, "main", "run-1")),
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
        claim(&world, "window-2", "run-1").is_empty(),
        "the second window gets nothing"
    );

    world.kill();
    world.restart();
    assert_eq!(
        ids(&claim(&world, "main", "run-2")),
        ["a"],
        "the adopted result survives a kill once"
    );
    assert_eq!(rows(&world), 1);
}

#[test]
fn two_windows_booting_at_once_take_each_result_once() {
    let world = World::new("ade89-race");
    let many: Vec<Value> = (0..40).map(|n| saved(&format!("r{n}"), None)).collect();
    completions::save(
        &world.conn(),
        Owner {
            window: "main",
            run: "run-1",
        },
        world.now(),
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
