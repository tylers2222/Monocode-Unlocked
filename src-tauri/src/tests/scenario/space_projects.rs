//! ADE-263: which projects a Space shows. Every store here is in memory or a
//! temp file: no test opens real data.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tauri::{Listener, Manager};

use crate::session_store::SessionStore;
use crate::spaces::projects::{self, InWorkspace, SpaceProject, SpaceProjects};
use crate::spaces::{self, SettingsPatch, FIRST_SPACE};

const TWO: &str = "space-2";

fn store_with_two_spaces() -> SessionStore {
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Two", None, 1).unwrap();
    store
}

/// A `monos` row as an import or an old build leaves it: no link is written.
fn old_mono(conn: &Connection, id: &str, space: &str, order: i64, projects: &Value) {
    conn.execute(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
         VALUES (?1, ?1, 'mono', ?2, ?3, 0, 0, ?4)",
        params![id, json!({ "projects": projects }).to_string(), order, space],
    )
    .unwrap();
}

fn links(conn: &Connection) -> Vec<(String, String)> {
    conn.prepare("SELECT space_id, project_path FROM space_project_links ORDER BY 1, 2")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn pair(space: &str, project: &str) -> (String, String) {
    (space.to_owned(), project.to_owned())
}

/// What a Space's window can tell from its list: every project it names, and
/// which paths are out of it.
fn visible(reply: &SpaceProjects) -> (Vec<&str>, &[String]) {
    let paths = reply.projects.iter().map(|p| p.path.as_str()).collect();
    (paths, &reply.assigned_elsewhere)
}

/// Defect (T15, I6): the backfill links a Mono's project in the wrong Space or
/// in every Space, keeps the path's written form, stops the app from opening
/// on a Mono whose Space is gone, runs again on a later open and so undoes a
/// removal, or changes which projects a Space has.
#[test]
fn the_first_open_links_each_monos_project_in_its_own_space_and_changes_no_list() {
    let store = store_with_two_spaces();
    let conn = store.lock_conn().unwrap();
    old_mono(
        &conn,
        "one",
        FIRST_SPACE,
        0,
        &json!(["/work/a", "/work/a/", "/work/both"]),
    );
    old_mono(
        &conn,
        "two",
        TWO,
        1,
        &json!(["/work/both/", "/work/b", "", "work/rel", 7]),
    );
    old_mono(&conn, "lost", "space-9", 2, &json!(["/work/lost"]));
    conn.execute(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
         VALUES ('torn', 'torn', 'mono', 'not json', 3, 0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO space_projects (project_path, space_id) VALUES ('/work/moved', ?1)",
        [TWO],
    )
    .unwrap();
    drop(conn);
    let read = |space: &str| projects::list(&store, space).unwrap();
    let (one_before, two_before) = (read(FIRST_SPACE), read(TWO));

    let conn = store.lock_conn().unwrap();
    conn.execute_batch("DROP TABLE space_project_links")
        .unwrap();
    spaces::ensure_tables(&conn).unwrap();
    let backfilled = vec![
        pair(FIRST_SPACE, "/work/a"),
        pair(FIRST_SPACE, "/work/both"),
        pair(TWO, "/work/b"),
        pair(TWO, "/work/both"),
    ];
    assert_eq!(links(&conn), backfilled);

    spaces::ensure_tables(&conn).unwrap();
    assert_eq!(links(&conn), backfilled, "a second open changed the links");
    conn.execute(
        "DELETE FROM space_project_links WHERE space_id = ?1 AND project_path = '/work/b'",
        [TWO],
    )
    .unwrap();
    spaces::ensure_tables(&conn).unwrap();
    assert!(
        !links(&conn).contains(&pair(TWO, "/work/b")),
        "a later open backfilled again"
    );
    drop(conn);

    assert_eq!(visible(&read(FIRST_SPACE)), visible(&one_before));
    assert_eq!(visible(&read(TWO)), visible(&two_before));
    assert_eq!(
        visible(&one_before),
        (
            vec!["/work/a", "/work/both"],
            &["/work/moved".to_owned()][..]
        )
    );
}

/// Defect: the resolver misses `A`, `L` or `M`, names a Space twice, or orders
/// the Spaces by anything but assigned first, then oldest (T6 moves an
/// assignment to the first of them); or `/a`, `/a/` and `~/a` are not one
/// project (I10).
#[test]
fn a_project_is_in_each_space_with_a_fact_and_every_form_of_its_path_is_one_project() {
    let store = store_with_two_spaces();
    let three = spaces::create(&store, "Three", None, 2).unwrap().id;
    let conn = store.lock_conn().unwrap();
    let spaces_of = |path: &str| spaces::project_spaces_in(&conn, path).unwrap();
    let link = |space: &str, path: &str| {
        conn.execute(
            "INSERT INTO space_project_links (space_id, project_path) VALUES (?1, ?2)",
            params![space, path],
        )
        .unwrap();
    };

    assert_eq!(spaces_of("/work/p"), [FIRST_SPACE], "no row is Space 1");
    conn.execute(
        "INSERT INTO space_projects (project_path, space_id) VALUES ('/work/p', ?1)",
        [&three],
    )
    .unwrap();
    assert_eq!(spaces_of("/work/p/"), [three.as_str()], "`A` moved it");

    link(TWO, "/work/p");
    assert_eq!(spaces_of("/work/p"), [three.as_str(), TWO], "`L` counts");
    old_mono(&conn, "one", FIRST_SPACE, 0, &json!(["/work/p/"]));
    old_mono(&conn, "two", TWO, 1, &json!(["/work/p"]));
    old_mono(&conn, "lost", "space-9", 2, &json!(["/work/p"]));
    old_mono(&conn, "last", "space-0", 3, &json!(["/work/p"]));
    assert_eq!(
        spaces_of("/work/p"),
        [three.as_str(), FIRST_SPACE, TWO, "space-0", "space-9"],
        "assigned first, then oldest, each once, a Space with no row last"
    );

    let home_form = "~/ade-263-only-a-key";
    let absolute = crate::fs::expand_home(home_form)
        .to_string_lossy()
        .into_owned();
    assert!(absolute.starts_with('/'), "{absolute}");
    old_mono(&conn, "home", TWO, 4, &json!([home_form]));
    assert_eq!(spaces_of(&absolute), [FIRST_SPACE, TWO]);
    assert_eq!(spaces_of(&format!("{absolute}/")), [FIRST_SPACE, TWO]);
    assert_eq!(spaces_of(home_form), [FIRST_SPACE, TWO]);
    assert_eq!(
        spaces::project_spaces_in(&conn, "~").unwrap(),
        [FIRST_SPACE],
        "`~` alone is no project, not the home folder"
    );
}

/// A mock app on a store with a project in each state.
fn seeded_app() -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_builder()
        .manage(store_with_two_spaces())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    {
        let store = app.state::<SessionStore>();
        let conn = store.lock_conn().unwrap();
        for (path, space) in [
            ("/work/here", TWO),
            ("/work/there", FIRST_SPACE),
            ("/work/x", TWO),
        ] {
            conn.execute(
                "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
                params![path, space],
            )
            .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO space_project_links (space_id, project_path) VALUES
               ('space-2', '/work/linked'), ('space-2', '/work/there'), ('space-1', '/work/x')",
        )
        .unwrap();
        old_mono(&conn, "later", TWO, 5, &json!(["/work/held"]));
        old_mono(
            &conn,
            "first",
            TWO,
            1,
            &json!(["/work/held", "/work/held/"]),
        );
        old_mono(&conn, "other", FIRST_SPACE, 2, &json!(["/work/one"]));
    }
    app
}

/// Defect: the list drops or invents a fact, a Mono is named twice or out of
/// roster order, `monoHeld` misses a Mono's project or names another Space's, a
/// Space other than Space 1 claims the projects with no row, a project in the
/// Space is reported as elsewhere, or a Space that does not exist reads as
/// empty, which would hide every project (T14 needs a failure).
#[test]
fn the_list_reads_a_spaces_projects_in_one_reply() {
    let app = seeded_app();
    let store = app.state::<SessionStore>();
    let list = |space: &str| projects::list(&store, space);
    let project = |path: &str, assigned, linked, monos: &[&str]| SpaceProject {
        path: path.into(),
        assigned,
        linked,
        monos: monos.iter().map(|&name| name.to_owned()).collect(),
    };
    assert_eq!(
        list(TWO),
        Ok(SpaceProjects {
            space_id: TWO.into(),
            projects: vec![
                project("/work/held", false, false, &["first", "later"]),
                project("/work/here", true, false, &[]),
                project("/work/linked", false, true, &[]),
                project("/work/there", false, true, &[]),
                project("/work/x", true, false, &[]),
            ],
            mono_held: vec!["/work/held".into()],
            no_row_is_here: false,
            assigned_elsewhere: vec![],
        })
    );
    assert_eq!(
        list(FIRST_SPACE),
        Ok(SpaceProjects {
            space_id: FIRST_SPACE.into(),
            projects: vec![
                project("/work/one", true, false, &["other"]),
                project("/work/there", true, false, &[]),
                project("/work/x", false, true, &[]),
            ],
            mono_held: vec!["/work/one".into()],
            no_row_is_here: true,
            assigned_elsewhere: vec!["/work/here".into()],
        })
    );
    assert_eq!(list("space-9"), Err("No Space space-9".into()));
}

/// Defect: a command reads a Space other than the calling window's; its reply
/// is not what the window reads; the known list misses a project or a Space, or
/// a stored assignment does not tell the windows (T13).
#[test]
fn the_commands_read_the_calling_windows_space_and_every_known_project() {
    let app = seeded_app();
    let store = || app.state::<SessionStore>();
    let window = super::agent_sandbox::main_window(&app);
    let list = |space: &str| projects::list(&store(), space);
    let from_window = || spaces::spaces_project_list(window.clone(), store());
    assert_eq!(
        from_window(),
        list(FIRST_SPACE),
        "a new window is Space 1's"
    );
    let patch = SettingsPatch {
        active_space_id: Some(TWO.into()),
        ..SettingsPatch::default()
    };
    spaces::spaces_settings_set(window.clone(), store(), patch).unwrap();
    assert_eq!(from_window(), list(TWO), "the window switched to Space 2");
    let reply = serde_json::to_value(from_window().unwrap()).unwrap();
    assert_eq!(
        reply["noRowIsHere"],
        json!(false),
        "the window reads camelCase"
    );
    assert_eq!(reply["monoHeld"], json!(["/work/held"]));

    let known: Vec<(String, Vec<String>)> = spaces::spaces_project_known(store())
        .unwrap()
        .into_iter()
        .map(|known| (known.path, known.spaces))
        .collect();
    let entry = |path: &str, spaces: &[&str]| {
        (
            path.to_owned(),
            spaces.iter().map(|&space| space.to_owned()).collect(),
        )
    };
    assert_eq!(
        known,
        [
            entry("/work/held", &[FIRST_SPACE, TWO]),
            entry("/work/here", &[TWO]),
            entry("/work/linked", &[FIRST_SPACE, TWO]),
            entry("/work/one", &[FIRST_SPACE]),
            entry("/work/there", &[FIRST_SPACE, TWO]),
            entry("/work/x", &[TWO, FIRST_SPACE]),
        ],
        "the add list's paths, each with its Spaces, assigned first"
    );

    let changed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let heard = changed.clone();
    app.listen_any(projects::CHANGED, move |_| {
        heard.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let set = |path: &str| {
        spaces::spaces_project_set(app.handle().clone(), store(), path.into(), TWO.into())
    };
    set("/work/there").unwrap();
    assert!(set("work/rel").is_err());
    assert_eq!(
        changed.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a stored assignment did not tell the windows, or a refused one did"
    );
}

/// F1. Defect: a path under the Giga workspace becomes a row or a list entry
/// (`gigas/monocode-general-2` holds a Giga's chat and every session of its
/// Space); the root is listed with no Mono of the Space, or hidden from the
/// Space of a Mono that lists it (I1, R1); a folder that only starts with the
/// workspace's name is taken for it; or the CLI's resolver stops answering for
/// a Giga's own folder.
#[test]
fn the_giga_workspace_never_becomes_a_row_and_a_path_under_it_is_never_listed() {
    let store = store_with_two_spaces();
    let roots = projects::workspace_roots();
    let root = roots.first().expect("a home folder").clone();
    let under = format!("{root}/gigas/monocode-general-2");
    let neighbour = format!("{root}-other");
    assert_eq!(projects::in_workspace(&root, &roots), InWorkspace::Root);
    assert_eq!(projects::in_workspace(&under, &roots), InWorkspace::Under);
    assert_eq!(
        projects::in_workspace(&neighbour, &roots),
        InWorkspace::Outside
    );

    let conn = store.lock_conn().unwrap();
    old_mono(&conn, "giga", TWO, 0, &json!([root, under, neighbour]));
    conn.execute_batch("DROP TABLE space_project_links")
        .unwrap();
    spaces::ensure_tables(&conn).unwrap();
    assert_eq!(links(&conn), [pair(TWO, &neighbour)], "only the neighbour");
    assert_eq!(
        spaces::project_spaces_in(&conn, &under).unwrap(),
        [FIRST_SPACE, TWO],
        "the CLI still answers for a Giga's own folder"
    );
    conn.execute(
        "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
        params![under, TWO],
    )
    .unwrap();
    drop(conn);

    let reply = projects::list(&store, TWO).unwrap();
    assert_eq!(visible(&reply).0, [root.as_str(), neighbour.as_str()]);
    let reply = projects::list(&store, FIRST_SPACE).unwrap();
    assert_eq!(
        visible(&reply),
        (Vec::<&str>::new(), &[][..]),
        "the root with no Mono here, and a path under it, are not listed"
    );
    let known = projects::known(&store).unwrap();
    assert_eq!(known.len(), 1, "only the neighbour: {known:?}");
    assert_eq!(known[0].path, neighbour);
}
