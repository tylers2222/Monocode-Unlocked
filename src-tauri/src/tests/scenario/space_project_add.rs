//! ADE-263: T1 and T2, and what a Mono's list writes (T3, T4). On an in-memory
//! store with the real functions.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusqlite::params;
use serde_json::{json, Value};
use tauri::{Listener, Manager};

use crate::mono_roster::{self, MonoCreate, MonoUpdate};
use crate::session_store::SessionStore;
use crate::spaces::project_add::{self, Added};
use crate::spaces::project_removal::ProjectRefusal;
use crate::spaces::{self, projects, SettingsPatch, FIRST_SPACE};

const TWO: &str = "space-2";

fn store() -> SessionStore {
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Space 2", None, 1).unwrap();
    store
}

fn spaces_of(store: &SessionStore, path: &str) -> Vec<String> {
    spaces::project_spaces_in(&store.lock_conn().unwrap(), path).unwrap()
}

fn rows(store: &SessionStore, sql: &str) -> Vec<String> {
    store
        .lock_conn()
        .unwrap()
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The Mono joins the active Space (`space_for_row`), as ADE-229's test does.
fn create_mono(store: &SessionStore, id: &str, space: &str, projects: &[&str]) {
    let patch = SettingsPatch {
        active_space_id: Some(space.into()),
        ..SettingsPatch::default()
    };
    spaces::set_settings(store, &patch).unwrap();
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": id,
        "role": "mono",
        "data": {"name": id, "projects": projects},
        "sortOrder": 0,
    }))
    .unwrap();
    let mono = mono_roster::validate_create(mono).unwrap();
    mono_roster::create(&store.lock_conn().unwrap(), &mono, None).unwrap();
}

fn update_mono(store: &SessionStore, id: &str, patch: &Value) {
    let update: MonoUpdate = serde_json::from_value(json!({"id": id, "patch": patch})).unwrap();
    let update = mono_roster::validate_update(update).unwrap();
    mono_roster::update_many(&store.lock_conn().unwrap(), &[update]).unwrap();
}

const LINKS: &str = "SELECT space_id || ' ' || project_path FROM space_project_links ORDER BY 1";

/// Defect (T1, T2, P3, I9): a new folder added in Space 2 lands in Space 1, or
/// shows in Space 1 too; `knownToWindow` is ignored, so a recent of Space 1 with
/// no row moves out of Space 1; a known project moves a Space's assignment, or
/// writes a link to the Space already assigned; or a second add changes
/// anything or says it wrote.
#[test]
fn adding_assigns_a_new_project_and_links_a_known_one() {
    let store = store();
    let add = |space: &str, path: &str, known| project_add::add(&store, space, path, known);

    assert_eq!(add(TWO, "/work/new/", false), Ok((Added::Assigned, true)));
    assert_eq!(spaces_of(&store, "/work/new"), [TWO], "P3: only in Space 2");
    assert_eq!(
        add(TWO, "/work/new", false),
        Ok((Added::Linked, false)),
        "I9"
    );
    assert!(rows(&store, LINKS).is_empty(), "already assigned: no write");

    assert_eq!(
        add(FIRST_SPACE, "/work/first", false),
        Ok((Added::Assigned, true))
    );
    assert_eq!(
        rows(&store, "SELECT project_path FROM space_projects ORDER BY 1"),
        ["/work/first", "/work/new"],
        "T1 writes the row in Space 1 too"
    );

    assert_eq!(add(TWO, "/work/recent", true), Ok((Added::Linked, true)));
    assert_eq!(
        spaces_of(&store, "/work/recent"),
        [FIRST_SPACE, TWO],
        "a recent of Space 1 stays there"
    );

    assert_eq!(
        add(FIRST_SPACE, "/work/new", false),
        Ok((Added::Linked, true)),
        "row"
    );
    assert_eq!(
        add(FIRST_SPACE, "/work/new", false),
        Ok((Added::Linked, false)),
        "already linked: no write"
    );
    assert_eq!(spaces_of(&store, "/work/new"), [TWO, FIRST_SPACE]);

    assert_eq!(
        add(FIRST_SPACE, "/work/recent", false),
        Ok((Added::Linked, false)),
        "a link knows it"
    );
    assert_eq!(
        rows(&store, LINKS),
        ["space-1 /work/new", "space-2 /work/recent"],
        "Space 1 is /work/recent's assigned Space: no link"
    );

    store
        .lock_conn()
        .unwrap()
        .execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
             VALUES ('s', ?1, 'codex', 'm', 'auto', 't', 1, 1)",
            params!["/work/chatted/"],
        )
        .unwrap();
    assert_eq!(
        add(TWO, "/work/chatted", false),
        Ok((Added::Linked, true)),
        "session"
    );
    assert_eq!(spaces_of(&store, "/work/chatted"), [FIRST_SPACE, TWO]);

    create_mono(&store, "m", FIRST_SPACE, &["/work/held"]);
    assert_eq!(
        add(TWO, "/work/held", false),
        Ok((Added::Linked, true)),
        "Mono"
    );
    assert_eq!(
        add(TWO, "/work/fresh", false),
        Ok((Added::Assigned, true)),
        "a Mono that lists another path does not make this one known"
    );
    assert_eq!(spaces_of(&store, "/work/fresh"), [TWO]);
}

/// Defect (T1, T17): a remote project is not a project; or a workspace path or
/// `~` is added.
#[test]
fn a_remote_project_is_added_and_the_workspace_and_home_are_not() {
    let store = store();
    let add = |path: &str| project_add::add(&store, TWO, path, false);
    assert_eq!(
        add("remote://box/srv/app/"),
        Ok((Added::Assigned, true)),
        "a remote project takes the states of any project"
    );
    assert_eq!(spaces_of(&store, "remote://box/srv/app"), [TWO]);

    let root = projects::workspace_roots().remove(0);
    for path in [root.clone(), format!("{root}/gigas/x")] {
        assert!(matches!(add(&path), Err(ProjectRefusal::Workspace { .. })));
    }
    assert!(matches!(add("~"), Err(ProjectRefusal::Failed { .. })));
}

/// Defect (T3, T4, F10): a Mono's project gets no link in its Space, or one in
/// the wrong Space; a Mono losing a project, or being deleted, drops it from the
/// Space, for a legacy Mono with no link too; a patch of another field writes a
/// link; a listed path that is no key, or `~` (no project, which Remove
/// refuses), gets a link or stops the later paths getting theirs; or the
/// workspace root or a path under it gets a link (a live Giga lists the root).
#[test]
fn a_monos_list_writes_links_that_outlive_it() {
    let store = store();
    let root = projects::workspace_roots().remove(0);
    let under = format!("{root}/gigas/x");
    create_mono(
        &store,
        "two",
        TWO,
        &["relative/x", "~", "/work/a/", root.as_str(), under.as_str()],
    );
    assert_eq!(rows(&store, LINKS), ["space-2 /work/a"], "T3 on create");

    update_mono(&store, "two", &json!({"projects": ["/work/a", "/work/b"]}));
    assert_eq!(
        rows(&store, LINKS),
        ["space-2 /work/a", "space-2 /work/b"],
        "T3 on update"
    );

    update_mono(&store, "two", &json!({"projects": ["/work/b"]}));
    assert_eq!(spaces_of(&store, "/work/a"), [FIRST_SPACE, TWO], "T4");

    store
        .lock_conn()
        .unwrap()
        .execute_batch(
            r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
               VALUES ('legacy', 'L', 'mono', '{"mascot":"owl","projects":["/work/old","/work/gone"]}', 1, 0, 0, 'space-2')"#,
        )
        .unwrap();
    update_mono(&store, "legacy", &json!({"name": "Renamed"}));
    assert!(
        !rows(&store, LINKS).contains(&"space-2 /work/old".to_owned()),
        "a patch of another field wrote a link"
    );
    update_mono(&store, "legacy", &json!({"projects": ["/work/gone"]}));
    assert_eq!(spaces_of(&store, "/work/old"), [FIRST_SPACE, TWO], "F10");

    let conn = store.lock_conn().unwrap();
    conn.execute_batch(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
         VALUES ('bad', 'B', 'mono', 'not json', 2, 0, 0, 'space-2')",
    )
    .unwrap();
    assert!(mono_roster::delete(&conn, "legacy").unwrap());
    assert!(mono_roster::delete(&conn, "bad").unwrap());
    assert!(mono_roster::delete(&conn, "two").unwrap());
    assert!(!mono_roster::delete(&conn, "two").unwrap());
    drop(conn);
    assert_eq!(spaces_of(&store, "/work/gone"), [FIRST_SPACE, TWO], "T4");
    assert_eq!(spaces_of(&store, "/work/b"), [FIRST_SPACE, TWO], "T4");
    assert_eq!(
        rows(&store, LINKS),
        [
            "space-2 /work/a",
            "space-2 /work/b",
            "space-2 /work/gone",
            "space-2 /work/old"
        ],
        "no link for the workspace, even when its Mono goes"
    );
}

/// Defect (T13, R6): a Mono's list changes with no event, so the window shows it
/// only after a reload, or every Mono write refetches the list; the add command
/// reads a Space other than the window's, tells no window of a write, or tells
/// every window of an add that wrote nothing.
#[test]
fn the_commands_read_the_windows_space_and_tell_every_window() {
    let app = tauri::test::mock_builder()
        .manage(store())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = super::agent_sandbox::main_window(&app);
    assert!(
        matches!(
            project_add::spaces_project_add(
                window.clone(),
                app.handle().clone(),
                app.state(),
                "/work/early".into(),
                false,
            ),
            Err(ProjectRefusal::Failed { .. })
        ),
        "a window with no Space added a project"
    );
    let heard = Arc::new(AtomicUsize::new(0));
    let count = heard.clone();
    app.listen_any(projects::CHANGED, move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    let events = || heard.load(Ordering::SeqCst);

    let patch = SettingsPatch {
        active_space_id: Some(TWO.into()),
        ..SettingsPatch::default()
    };
    spaces::spaces_settings_set(window.clone(), app.state(), patch).unwrap();
    let add = |path: &str, known| {
        project_add::spaces_project_add(
            window.clone(),
            app.handle().clone(),
            app.state(),
            path.into(),
            known,
        )
    };
    assert_eq!(add("/work/new", false), Ok(Added::Assigned));
    assert_eq!(
        serde_json::to_value(Added::Assigned).unwrap(),
        json!("assigned")
    );
    assert_eq!(spaces_of(&app.state(), "/work/new"), [TWO]);
    assert_eq!(events(), 1, "an add tells the windows");
    assert_eq!(add("/work/new", false), Ok(Added::Linked));
    assert_eq!(events(), 1, "an add that wrote nothing tells no window");
    assert_eq!(add("/work/recent", true), Ok(Added::Linked));
    assert_eq!(events(), 2, "an add that wrote a link tells the windows");

    let state: tauri::State<'_, SessionStore> = app.state();
    create_mono(&state, "m", TWO, &["/work/a"]);

    let update = |patch: Value| -> MonoUpdate {
        serde_json::from_value(json!({"id": "m", "patch": patch})).unwrap()
    };
    let write = |patch: Value| {
        mono_roster::monos_update(app.handle().clone(), app.state(), vec![update(patch)]).unwrap()
    };
    write(json!({"name": "renamed"}));
    assert_eq!(events(), 2, "a rename does not touch the list");
    write(json!({"projects": ["/work/a", "/work/b"]}));
    assert_eq!(events(), 3, "T3");
    write(json!({"projects": []}));
    assert_eq!(events(), 4, "T4");
}
