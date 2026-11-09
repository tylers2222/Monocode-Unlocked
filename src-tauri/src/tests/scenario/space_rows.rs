//! The Space switcher's rows (ADE-179). The window shows only the active
//! Space, so every Mono and session it lists carries the Space its insert
//! stored: the roster (`monos_list`), a save's reply (`session_upsert`), a
//! project's sessions (`session_list_by_project`) and the linked threads
//! (`session_list_linked`), each called as the window invokes it. Switching
//! the active Space (`spaces_settings_set`) only changes where new rows go; a
//! row already saved keeps its Space through later saves and a restart.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use tauri::test::MockRuntime;
use tauri::{App, Manager};

use crate::mono_roster::{self, MonoCreate, MonoRole};
use crate::session_store::{self, now_millis, SessionStore, SessionSummary, SessionUpsert};
use crate::spaces::{self, SettingsPatch, FIRST_SPACE};

const PROJECT: &str = "/space-rows/project";

/// The app on the database at `path`, through its own open path.
fn launch(path: &Path) -> App<MockRuntime> {
    let store = SessionStore::open_at(path.to_path_buf(), now_millis()).unwrap();
    tauri::test::mock_builder()
        .manage(store)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap()
}

fn add_mono(app: &App<MockRuntime>, id: &str) {
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": id,
        "role": MonoRole::Mono,
        "data": {"name": id},
        "sortOrder": 0,
    }))
    .unwrap();
    let window = crate::scenario::agent_sandbox::main_window(app);
    mono_roster::monos_create(app.handle().clone(), window, app.state(), mono).unwrap();
}

/// A window save of session `id` in `PROJECT`, linked to a PR so the linked
/// list holds it too; the reply it gets.
fn save(app: &App<MockRuntime>, id: &str) -> SessionSummary {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": PROJECT,
        "harness": "codex",
        "model": "codex:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [{"id": "u1", "role": "user", "text": "Start"}],
        "linkedWorkItem": {"kind": "pr", "repo": "o/r", "number": 1, "url": "https://example.com/1"},
    }))
    .unwrap();
    let window = crate::scenario::agent_sandbox::main_window(app);
    session_store::session_upsert(window, app.state(), session).unwrap()
}

fn switch_to(app: &App<MockRuntime>, space: &str) {
    let patch = SettingsPatch {
        active_space_id: Some(space.to_owned()),
        route_grant_ttl_ms: None,
    };
    spaces::spaces_settings_set(
        crate::scenario::agent_sandbox::main_window(app),
        app.state(),
        patch,
    )
    .unwrap();
}

/// `spaceId` as the window reads it from a row's JSON.
fn space_of(row: &impl Serialize) -> String {
    let value = serde_json::to_value(row).unwrap();
    value["spaceId"].as_str().unwrap_or("<none>").to_owned()
}

/// Every listed row of `ids` as `kind id space`, from the roster and each
/// session list the window reads.
fn listed(app: &App<MockRuntime>, ids: &[&str]) -> Vec<String> {
    let ours = |id: &str| ids.contains(&id);
    let monos = mono_roster::monos_list(app.state()).unwrap().monos;
    let by_project = session_store::session_list_by_project(app.state(), PROJECT.into()).unwrap();
    let linked = session_store::session_list_linked(app.state()).unwrap();
    let mut rows: Vec<String> = monos
        .iter()
        .filter(|row| ours(&row.id))
        .map(|row| format!("mono {} {}", row.id, space_of(row)))
        .chain(
            by_project
                .iter()
                .filter(|row| ours(&row.id))
                .map(|row| format!("project-session {} {}", row.id, space_of(row))),
        )
        .chain(
            linked
                .iter()
                .filter(|row| ours(&row.id))
                .map(|row| format!("linked-session {} {}", row.id, space_of(row))),
        )
        .collect();
    rows.sort();
    rows
}

fn database() -> PathBuf {
    std::env::temp_dir()
        .join(format!("space-rows-{}", uuid::Uuid::new_v4()))
        .join("monocode.db")
}

#[test]
fn each_listed_row_carries_its_saved_space() {
    let path = database();
    let app = launch(&path);
    let space_2 = spaces::spaces_create(app.state(), "Space 2".into(), None)
        .unwrap()
        .id;
    let mut failed = Vec::new();

    // 1. Rows made while Space 1 is active are in Space 1, and say so.
    add_mono(&app, "rows-one");
    let reply = save(&app, "rows-s-one");
    if space_of(&reply) != FIRST_SPACE {
        failed.push(format!("1 Space 1 save replied {}", space_of(&reply)));
    }

    // 2. After a switch to Space 2, new rows are in Space 2, and say so.
    switch_to(&app, &space_2);
    add_mono(&app, "rows-two");
    let reply = save(&app, "rows-s-two");
    if space_of(&reply) != space_2 {
        failed.push(format!("2 Space 2 save replied {}", space_of(&reply)));
    }

    let ids = ["rows-one", "rows-two", "rows-s-one", "rows-s-two"];
    let expected = vec![
        format!("linked-session rows-s-one {FIRST_SPACE}"),
        format!("linked-session rows-s-two {space_2}"),
        format!("mono rows-one {FIRST_SPACE}"),
        format!("mono rows-two {space_2}"),
        format!("project-session rows-s-one {FIRST_SPACE}"),
        format!("project-session rows-s-two {space_2}"),
    ];
    // 3. Every list the window reads gives each row its stored Space.
    if listed(&app, &ids) != expected {
        failed.push(format!(
            "3 lists after the switch: {:?}",
            listed(&app, &ids)
        ));
    }

    // 4. Switching back moves nothing: a later save of a Space 2 session
    // still replies Space 2, and every list is unchanged.
    switch_to(&app, FIRST_SPACE);
    let reply = save(&app, "rows-s-two");
    if space_of(&reply) != space_2 {
        failed.push(format!(
            "4 re-save after switching back replied {}",
            space_of(&reply)
        ));
    }
    if listed(&app, &ids) != expected {
        failed.push(format!(
            "4 lists after switching back: {:?}",
            listed(&app, &ids)
        ));
    }

    // 5. A restart on the same database reads the same Spaces back.
    drop(app);
    let app = launch(&path);
    if listed(&app, &ids) != expected {
        failed.push(format!("5 lists after a restart: {:?}", listed(&app, &ids)));
    }

    drop(app);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    assert!(
        failed.is_empty(),
        "space rows checkpoints failed:\n{}",
        failed.join("\n")
    );
}
