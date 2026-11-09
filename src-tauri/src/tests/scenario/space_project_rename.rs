//! ADE-263, T12: what follows a project when `rebase_project` moves it. On an
//! in-memory store, with a temp folder for the lease. No test opens real data.

use rusqlite::params;
use serde_json::json;

use crate::session_store::{rebase_project, SessionStore};
use crate::spaces::{self, FIRST_SPACE};

const TWO: &str = "space-2";
const OLD: &str = "/work/old";
const NEW: &str = "/work/new";

struct Rig {
    store: SessionStore,
}

impl Rig {
    fn new() -> Self {
        let store = SessionStore::open_in_memory().unwrap();
        spaces::create(&store, "Space 2", None, 1).unwrap();
        Self { store }
    }

    fn sql(&self, sql: &str, values: impl rusqlite::Params) {
        self.store
            .lock_conn()
            .unwrap()
            .execute(sql, values)
            .unwrap();
    }

    fn assign(&self, path: &str, space: &str) {
        self.sql(
            "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
            params![path, space],
        );
    }

    fn link(&self, space: &str, path: &str) {
        self.sql(
            "INSERT INTO space_project_links (space_id, project_path) VALUES (?1, ?2)",
            params![space, path],
        );
    }

    fn login(&self, path: &str, account: &str) {
        self.sql(
            "INSERT OR IGNORE INTO provider_accounts (provider, id, label, created_at)
             VALUES ('claude', ?1, ?1, 0)",
            [account],
        );
        self.sql(
            "INSERT INTO space_project_accounts (space_id, project_path, provider, account_id)
             VALUES (?1, ?2, 'claude', ?3)",
            params![TWO, path, account],
        );
    }

    fn mono(&self, id: &str, projects: &serde_json::Value) {
        self.sql(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
             VALUES (?1, ?1, 'mono', ?2, 0, 0, 0, ?3)",
            params![id, json!({ "projects": projects }).to_string(), TWO],
        );
    }

    fn rename(&self, from: &str, to: &str) {
        rebase_project(&self.store.lock_conn().unwrap(), from, to).unwrap();
    }

    fn strings(&self, sql: &str) -> Vec<String> {
        self.store
            .lock_conn()
            .unwrap()
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn spaces_of(&self, path: &str) -> Vec<String> {
        spaces::project_spaces_in(&self.store.lock_conn().unwrap(), path).unwrap()
    }

    fn mono_list(&self, id: &str) -> Vec<String> {
        self.strings(&format!(
            "SELECT j.value FROM monos m, json_each(CASE WHEN json_valid(m.data_json) THEN m.data_json END, '$.projects') j
              WHERE m.id = '{id}'"
        ))
    }
}

/// Defect (T12, F7): a rename loses the assignment, so a Space 2 project falls
/// back to Space 1; leaves a link, a Mono's list, a login or an automation on the
/// old path; a Mono holds the new path twice; a session or milestone under
/// another form of the path stays behind; or a project not renamed is touched.
#[test]
fn a_rename_takes_everything_stored_under_the_project_with_it() {
    let rig = Rig::new();
    rig.assign(OLD, TWO);
    rig.link(FIRST_SPACE, OLD);
    rig.assign("/work/other", TWO);
    rig.login(OLD, "work");
    rig.mono("both", &json!(["/work/old/", "/work/keep", NEW]));
    rig.mono("just-old", &json!([OLD]));
    rig.mono("other", &json!(["/work/keep"]));
    rig.sql(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
         VALUES ('torn', 'torn', 'mono', 'not json', 0, 0, 0)",
        [],
    );
    rig.sql(
        "INSERT INTO automations (id, definition_json, enabled, next_run_at, updated_at)
         VALUES ('nightly', ?1, 1, 1, 1), ('elsewhere', ?2, 1, 1, 1)",
        params![
            json!({"name": "n", "cwd": "/work/old/"}).to_string(),
            json!({"name": "e", "cwd": "/work/keep"}).to_string()
        ],
    );
    for (id, cwd) in [("a", OLD), ("b", "/work/old/"), ("c", "/work/keep")] {
        rig.sql(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
             VALUES (?1, ?2, 'codex', 'm', 'auto', 't', 1, 1)",
            params![id, cwd],
        );
    }

    rig.rename(OLD, NEW);

    assert_eq!(
        rig.strings("SELECT project_path || ' ' || space_id FROM space_projects ORDER BY 1"),
        ["/work/new space-2", "/work/other space-2"]
    );
    assert_eq!(
        rig.strings("SELECT space_id || ' ' || project_path FROM space_project_links"),
        ["space-1 /work/new"]
    );
    assert_eq!(
        rig.strings("SELECT project_path || ' ' || account_id FROM space_project_accounts"),
        ["/work/new work"],
        "a project keeps its own login"
    );
    assert_eq!(
        rig.mono_list("both"),
        [NEW, "/work/keep"],
        "deduped, in place"
    );
    assert_eq!(rig.mono_list("just-old"), [NEW]);
    assert_eq!(rig.mono_list("other"), ["/work/keep"]);
    assert_eq!(
        rig.strings(
            "SELECT json_extract(definition_json, '$.cwd') FROM automations ORDER BY id DESC"
        ),
        [NEW, "/work/keep"]
    );
    assert_eq!(
        rig.strings("SELECT cwd FROM sessions ORDER BY id"),
        [NEW, NEW, "/work/keep"],
        "compared by project_key, not cwd = ?"
    );
    assert_eq!(rig.spaces_of(NEW), [TWO, FIRST_SPACE]);
    assert_eq!(
        rig.spaces_of(OLD),
        [FIRST_SPACE],
        "no row: Space 1's default"
    );

    let moved = rig.strings("SELECT project_path FROM space_projects ORDER BY 1");
    rig.rename(OLD, NEW);
    assert_eq!(
        rig.strings("SELECT project_path FROM space_projects ORDER BY 1"),
        moved,
        "I9: a retry finds nothing under the old path, so the assignment stays"
    );
}

/// Defect (T12, I2, I7): renaming onto a project that has rows drops its Space
/// instead of uniting, or the new path's assignment beats the old one's, or the
/// old path's login beats the one the new path has. A project with no row is
/// Space 1's, and that assignment wins too.
#[test]
fn a_rename_onto_a_known_project_unites_the_two() {
    let rig = Rig::new();
    let three = spaces::create(&rig.store, "Three", None, 2).unwrap().id;
    rig.assign(OLD, TWO);
    rig.assign(NEW, &three);
    rig.login(OLD, "old-login");
    rig.login(NEW, "new-login");
    rig.rename(OLD, NEW);
    assert_eq!(
        rig.spaces_of(NEW),
        [TWO, three.as_str()],
        "the old assignment wins, and the new path's Space stays linked"
    );
    assert_eq!(
        rig.strings("SELECT account_id FROM space_project_accounts"),
        ["new-login"]
    );

    rig.link(TWO, "/work/no-row");
    rig.assign("/work/target", &three);
    rig.rename("/work/no-row", "/work/target");
    assert_eq!(
        rig.spaces_of("/work/target"),
        [FIRST_SPACE, TWO, three.as_str()],
        "no row is Space 1: it wins, and the links unite"
    );

    rig.assign("/work/assigned", TWO);
    rig.link(&three, "/work/linked-only");
    rig.rename("/work/assigned", "/work/linked-only");
    assert_eq!(
        rig.spaces_of("/work/linked-only"),
        [TWO, FIRST_SPACE, three.as_str()],
        "a new path with no row but a link was in Space 1: it stays"
    );

    rig.assign("/work/moved", TWO);
    rig.rename("/work/moved", "/work/unknown");
    assert_eq!(
        rig.spaces_of("/work/unknown"),
        [TWO],
        "an unknown new path was in no Space: Space 1 gains nothing"
    );
}

/// Defect (T12, I4): a stale lease flips who owns the folder after a rename; or a
/// lease moves to a folder that is not there, or over the new path's own lease;
/// or any other lease moves. `~` is no project, and a workspace path gets no row.
#[test]
fn a_lease_moves_with_its_folder_and_only_then() {
    let rig = Rig::new();
    let root = std::env::temp_dir().join(format!("ade263-t12-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("new")).unwrap();
    std::fs::create_dir_all(root.join("held")).unwrap();
    let real = root.canonicalize().unwrap();
    let text = |name: &str| real.join(name).to_string_lossy().into_owned();
    let lease = |path: &str, space: &str| {
        rig.sql(
            "INSERT INTO checkout_leases (checkout_path, space_id, leased_at) VALUES (?1, ?2, 1)",
            params![path, space],
        );
    };
    let leases = || {
        rig.strings(
            "SELECT checkout_path || ' ' || space_id FROM checkout_leases ORDER BY checkout_path",
        )
    };
    lease(&text("old"), TWO);
    lease("/work/stays", TWO);
    rig.rename(&text("old"), &text("new"));
    assert_eq!(
        leases(),
        [
            format!("{} {TWO}", text("new")),
            format!("/work/stays {TWO}")
        ]
    );

    lease(&text("old2"), FIRST_SPACE);
    lease(&text("held"), TWO);
    rig.rename(&text("old2"), &text("held"));
    assert_eq!(
        leases().len(),
        3,
        "the new path's own lease wins, and the stale one goes"
    );
    assert!(leases().contains(&format!("{} {TWO}", text("held"))));

    lease(&text("old3"), TWO);
    rig.rename(&text("old3"), &text("not-there"));
    assert!(
        leases().contains(&format!("{} {TWO}", text("old3"))),
        "no folder to resolve: the lease stays"
    );

    rig.assign(OLD, TWO);
    rig.rename(OLD, "~");
    assert_eq!(rig.spaces_of(OLD), [TWO], "`~` is no project");
    let root_key = spaces::projects::workspace_roots().remove(0);
    let under = format!("{root_key}/gigas/moved");
    rig.rename(OLD, &under);
    assert_eq!(
        rig.strings("SELECT project_path FROM space_projects"),
        [OLD],
        "a workspace path never becomes a row"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
