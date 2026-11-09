//! ADE-263, ADE-267: taking a project out of a Space and deleting its data,
//! on an in-memory store with the real functions. No test opens real data.

use std::cell::Cell;
use std::collections::HashSet;

use rusqlite::{params, Connection};
use serde_json::json;
use tauri::{Listener, Manager};

use crate::session_store::{delete_session, list_by_project, Removal, SessionStore};
use crate::spaces::project_removal::{
    self as removal, running_spaces, ProjectRefusal, SessionFacts, Turns,
};
use crate::spaces::{self, projects, SettingsPatch, FIRST_SPACE};

const TWO: &str = "space-2";
const SHARED: &str = "/work/sample-app";

struct Rig {
    store: SessionStore,
}

fn no_turns() -> Option<Turns> {
    Some(Turns::new())
}

fn turn(session: &str) -> Turns {
    HashSet::from([session.to_owned()])
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
        spaces::set_project_space(&self.store, path, space).unwrap();
    }

    fn link(&self, space: &str, path: &str) {
        self.sql(
            "INSERT INTO space_project_links (space_id, project_path) VALUES (?1, ?2)",
            params![space, path],
        );
    }

    fn mono(&self, id: &str, name: &str, space: &str, projects: &[&str]) {
        self.chat_mono(id, name, space, projects, "no-such-chat");
    }

    fn chat_mono(&self, id: &str, name: &str, space: &str, projects: &[&str], chat: &str) {
        self.sql(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
             VALUES (?1, ?2, 'mono', ?3, (SELECT count(*) FROM monos), 0, 0, ?4)",
            params![
                id,
                name,
                json!({ "projects": projects, "sessionId": chat }).to_string(),
                space
            ],
        );
    }

    fn session(&self, id: &str, cwd: &str, space: &str) {
        self.session_with(id, cwd, space, 1);
    }

    fn session_with(&self, id: &str, cwd: &str, space: &str, has_user_message: i64) {
        self.sql(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, has_user_message, space_id)
             VALUES (?1, ?2, 'codex', 'm', 'auto', ?1, 1, 1, ?4, ?3)",
            params![id, cwd, space, has_user_message],
        );
    }

    fn automation(&self, id: &str, name: &str, cwd: &str) {
        self.sql(
            "INSERT INTO automations (id, definition_json, enabled, next_run_at, updated_at)
             VALUES (?1, ?2, 1, 1, 1)",
            params![id, json!({ "name": name, "cwd": cwd }).to_string()],
        );
    }

    fn milestone(&self, project: &str) {
        self.sql(
            "INSERT INTO milestones (id, project_path, title, status, sort_order, created_by, created_at, updated_at)
             VALUES (lower(hex(randomblob(18))), ?1, 'goal', 'idea', 0, 'user', 1, 1)",
            [project],
        );
        self.sql(
            "INSERT INTO milestone_settings (project_path, enabled, updated_at) VALUES (?1, 1, 1)",
            [project],
        );
    }

    fn strings(&self, sql: &str) -> Vec<String> {
        let conn = self.store.lock_conn().unwrap();
        read_strings(&conn, sql)
    }

    fn sessions(&self) -> Vec<String> {
        self.strings("SELECT id FROM sessions ORDER BY id")
    }

    /// Every fact a refused event must leave alone.
    fn everything(&self) -> Vec<Vec<String>> {
        [
            "SELECT id FROM sessions ORDER BY id",
            "SELECT project_path || ' ' || space_id FROM space_projects ORDER BY 1",
            "SELECT space_id || ' ' || project_path FROM space_project_links ORDER BY 1",
            "SELECT project_path FROM milestones ORDER BY 1",
            "SELECT project_path FROM milestone_settings ORDER BY 1",
            "SELECT checkout_path || ' ' || space_id FROM checkout_leases ORDER BY 1",
        ]
        .map(|sql| self.strings(sql))
        .into()
    }

    fn end(&self, session: &str) -> Result<(), String> {
        let conn = self.store.lock_conn()?;
        delete_session(&conn, session, Removal::End)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, space: &str, path: &str) -> Result<removal::ProjectDeleted, ProjectRefusal> {
        removal::delete(&self.store, space, path, no_turns, |id| self.end(id))
    }

    fn remove(&self, space: &str, path: &str) -> Result<bool, ProjectRefusal> {
        removal::remove(&self.store, space, path, no_turns)
    }

    fn spaces_of(&self, path: &str) -> Vec<String> {
        let conn = self.store.lock_conn().unwrap();
        spaces::project_spaces_in(&conn, path).unwrap()
    }

    fn preview(&self, space: &str, path: &str) -> removal::DeletePreview {
        removal::delete_preview(&self.store, space, path, no_turns).unwrap()
    }

    /// Deleting `path` from `space` refuses with `expected` and changes nothing.
    fn delete_refused(&self, space: &str, path: &str, expected: &ProjectRefusal) {
        let before = self.everything();
        assert_eq!(
            removal::delete_preview(&self.store, space, path, no_turns).err(),
            Some(expected.clone())
        );
        assert_eq!(self.delete(space, path).err(), Some(expected.clone()));
        assert_eq!(self.everything(), before, "a refused delete changed a row");
    }
}

fn read_strings(conn: &Connection, sql: &str) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

fn running_here() -> ProjectRefusal {
    ProjectRefusal::SessionRunning {
        project: "sample-app".into(),
        message: "A session is running in sample-app here. Stop it first.".into(),
    }
}

/// The P4 data loss. Defect (T10, I5): a delete from Space 2 takes another
/// Space's sessions, a shared milestone, or a session of another project; it
/// leaves a hidden worker or draft of its own Space, which the window would keep
/// showing; its count is not this Space's chats; a path form hides a session
/// from it; or the project stays in Space 2 or leaves Space 1.
#[test]
fn deleting_a_shared_project_in_one_space_takes_only_that_spaces_sessions() {
    let rig = Rig::new();
    rig.link(TWO, SHARED);
    for (id, cwd, space) in [
        ("one-a", SHARED, FIRST_SPACE),
        ("one-b", "/work/sample-app/", FIRST_SPACE),
        ("two-a", SHARED, TWO),
        ("two-b", "/work/sample-app/", TWO),
        ("two-other", "/work/other", TWO),
    ] {
        rig.session(id, cwd, space);
    }
    rig.session_with("two-draft", SHARED, TWO, 0);
    rig.milestone(SHARED);

    let preview = rig.preview(TWO, SHARED);
    assert!(!preview.last_space);
    assert_eq!(
        preview.session_count, 2,
        "the dialog counts Space 2's chats"
    );
    assert_eq!(preview.other_spaces, [FIRST_SPACE]);
    assert_eq!(preview.hidden_session_count, 2);

    let deleted = rig.delete(TWO, SHARED).unwrap();
    assert!(!deleted.last_space);
    assert_eq!(
        sorted(deleted.deleted_session_ids),
        ["two-a", "two-b", "two-draft"]
    );
    assert_eq!(rig.sessions(), ["one-a", "one-b", "two-other"]);
    assert_eq!(rig.strings("SELECT project_path FROM milestones"), [SHARED]);
    assert_eq!(
        rig.strings("SELECT project_path FROM milestone_settings"),
        [SHARED]
    );
    assert_eq!(
        rig.spaces_of(SHARED),
        [FIRST_SPACE],
        "T5 follows the delete"
    );

    let again = rig.delete(TWO, SHARED).unwrap();
    assert!(again.deleted_session_ids.is_empty(), "I9");
    assert_eq!(rig.spaces_of(SHARED), [FIRST_SPACE]);
}

/// Defect (T10 then T6): a delete in a project's assigned Space does not hand
/// the assignment to the other Space, so the project has no Space left.
#[test]
fn deleting_in_the_assigned_space_moves_the_assignment_to_the_other_space() {
    let rig = Rig::new();
    rig.assign(SHARED, TWO);
    rig.mono("one", "One", FIRST_SPACE, &[SHARED]);
    rig.session("one-a", SHARED, FIRST_SPACE);
    rig.session("two-a", SHARED, TWO);

    let deleted = rig.delete(TWO, SHARED).unwrap();
    assert!(!deleted.last_space);
    assert_eq!(deleted.deleted_session_ids, ["two-a"]);
    assert_eq!(rig.spaces_of(SHARED), [FIRST_SPACE]);
    assert_eq!(rig.sessions(), ["one-a"]);
}

/// Defect (T11, F3): a project's last Space leaves its milestones, settings,
/// assigned or link rows behind; takes another Space's hidden sessions, which
/// T5 promises return; or a purge reaches another project.
#[test]
fn deleting_in_the_last_space_purges_the_project_and_keeps_other_spaces_chats() {
    let rig = Rig::new();
    rig.assign(SHARED, TWO);
    rig.link(TWO, SHARED);
    rig.session("two-a", SHARED, TWO);
    rig.session("one-hidden", SHARED, FIRST_SPACE);
    rig.session("two-other", "/work/other", TWO);
    rig.milestone(SHARED);
    rig.milestone("/work/other");
    rig.assign("/work/other", TWO);
    rig.link(TWO, "/work/other");

    let preview = rig.preview(TWO, SHARED);
    assert!(preview.last_space);
    assert_eq!(preview.session_count, 1, "this Space's chats only");
    assert!(preview.other_spaces.is_empty());
    assert_eq!(preview.hidden_session_count, 1, "T11's dialog line");

    let deleted = rig.delete(TWO, SHARED).unwrap();
    assert!(deleted.last_space);
    assert_eq!(deleted.deleted_session_ids, ["two-a"]);
    assert_eq!(rig.sessions(), ["one-hidden", "two-other"]);
    for table in [
        "milestones",
        "milestone_settings",
        "space_projects",
        "space_project_links",
    ] {
        assert_eq!(
            rig.strings(&format!("SELECT project_path FROM {table}")),
            ["/work/other"],
            "{table}"
        );
    }
    assert_eq!(rig.preview(TWO, "/work/free").hidden_session_count, 0);
}

/// F1, the MUST. Defect: a path at or under the Giga workspace gets a Remove,
/// an Archive or a Delete; a delete takes a Mono's chat; a project holding a
/// Mono's chat in any Space can be archived or deleted; or a rule added here
/// stops a Giga's chat resolving, so it no longer opens.
#[test]
fn the_workspace_and_every_monos_chat_are_out_of_reach() {
    let rig = Rig::new();
    let roots = projects::workspace_roots();
    let root = roots.first().expect("a home folder").clone();
    let under = format!("{root}/gigas/sample-giga");
    let conn_row = |path: &str| {
        rig.sql(
            "INSERT OR IGNORE INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
            params![path, TWO],
        );
    };
    conn_row(&under);
    rig.session("chief-chat", &under, TWO);
    rig.chat_mono("chief", "Chief", TWO, &[root.as_str()], "chief-chat");
    let before = rig.everything();
    for path in [&root, &under] {
        for refused in [
            rig.remove(TWO, path).map(drop),
            rig.delete(TWO, path).map(drop),
            removal::archive_check(&rig.store, TWO, path, no_turns),
            removal::delete_preview(&rig.store, TWO, path, no_turns).map(drop),
        ] {
            assert!(
                matches!(refused, Err(ProjectRefusal::Workspace { .. })),
                "{path}: {refused:?}"
            );
        }
    }
    assert_eq!(rig.everything(), before, "the live row stays untouched");
    {
        let conn = rig.store.lock_conn().unwrap();
        assert_eq!(
            spaces::project_spaces_in(&conn, &under).unwrap(),
            [TWO],
            "a Giga's folder stopped resolving"
        );
        assert_eq!(
            list_by_project(&conn, &under).unwrap()[0].id,
            "chief-chat",
            "a Giga's chat stopped opening"
        );
    }
    assert_eq!(spaces::project_space(&rig.store, &under).unwrap(), TWO);

    rig.link(TWO, SHARED);
    rig.session("chat-here", SHARED, FIRST_SPACE);
    rig.session("two-a", SHARED, TWO);
    rig.chat_mono("away", "Away", FIRST_SPACE, &[], "chat-here");
    let chat = ProjectRefusal::MonoChat {
        project: "sample-app".into(),
        monos: vec!["Away".into()],
        message: "Away's chat is in this project.".into(),
    };
    rig.delete_refused(TWO, SHARED, &chat);
    assert_eq!(
        removal::archive_check(&rig.store, TWO, SHARED, no_turns),
        Err(chat)
    );
    rig.chat_mono("second", "Second", TWO, &[], "two-a");
    let refused = removal::archive_check(&rig.store, TWO, SHARED, no_turns).unwrap_err();
    assert!(
        matches!(&refused, ProjectRefusal::MonoChat { message, .. }
            if message == "Away and Second have chats in this project."),
        "{refused:?}"
    );
}

/// Defect (T7, T9, I8): a Mono does not stop a removal, delete or archive; its
/// text is not the approved text; T9 forgets a Mono of another Space or the
/// Space's name; a Mono refusal does not come before a running one.
#[test]
fn a_mono_stops_every_way_out_in_the_approved_words() {
    let rig = Rig::new();
    rig.link(TWO, SHARED);
    rig.session("two-a", SHARED, TWO);
    rig.milestone(SHARED);
    rig.mono("site-builder", "Site Builder", TWO, &["/work/sample-app/"]);
    let held = ProjectRefusal::MonoHeld {
        project: "sample-app".into(),
        monos: vec!["Site Builder".into()],
        message: "Site Builder uses sample-app. Take it off that Mono first.".into(),
    };
    let before = rig.everything();
    assert_eq!(rig.remove(TWO, SHARED), Err(held.clone()));
    rig.delete_refused(TWO, SHARED, &held);
    assert_eq!(
        removal::remove(&rig.store, TWO, SHARED, || Some(turn("two-a"))),
        Err(held),
        "a Mono refusal shows before a running one"
    );

    let archive = |space: &str| match removal::archive_check(&rig.store, space, SHARED, no_turns) {
        Err(ProjectRefusal::MonoHeld { message, .. }) => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        archive(TWO),
        "Site Builder uses sample-app, and Archive hides it in every Space. \
         Take it off that Mono first."
    );
    assert_eq!(
        archive(FIRST_SPACE),
        "Site Builder in Space 2 uses sample-app, and Archive hides it in every Space. \
         Take it off that Mono first."
    );
    rig.mono("helper", "Helper", TWO, &[SHARED]);
    rig.mono("third", "Third", FIRST_SPACE, &[SHARED]);
    assert_eq!(
        archive(TWO),
        "Site Builder, Helper and Third in Space 1 use sample-app, and Archive hides it \
         in every Space. Take it off those Monos first."
    );
    assert_eq!(rig.everything(), before);
    assert_eq!(
        removal::archive_check(&rig.store, TWO, "/work/free", no_turns),
        Ok(())
    );
    assert!(matches!(
        removal::archive_check(&rig.store, TWO, "~", no_turns),
        Err(ProjectRefusal::Failed { .. })
    ));
}

/// Defect (T8, T10, T11, F14, I5): a guard misses a session in flight, reads a
/// session of the wrong Space or project, stops a Space's delete for a turn of
/// another Space it does not touch, lets a last Space's delete run past a turn
/// of another Space, or passes when the turns cannot be read.
#[test]
fn a_running_session_stops_what_reaches_it_and_nothing_else() {
    let facts = |id: &str, space: &str, project: Option<&str>| SessionFacts {
        id: id.into(),
        space: space.into(),
        project: project.map(str::to_owned),
    };
    let sessions = [
        facts("b", TWO, Some(SHARED)),
        facts("a", FIRST_SPACE, Some(SHARED)),
        facts("c", TWO, Some("/work/other")),
        facts("d", TWO, None),
    ];
    let set = |ids: &[&str]| ids.iter().map(|&id| id.to_owned()).collect::<HashSet<_>>();
    assert!(running_spaces(&sessions, &set(&["c", "d"]), SHARED).is_empty());
    assert_eq!(
        running_spaces(&sessions, &set(&["b", "a"]), SHARED),
        [FIRST_SPACE, TWO]
    );

    let rig = Rig::new();
    rig.link(TWO, SHARED);
    rig.session("two-a", SHARED, TWO);
    rig.session("one-a", SHARED, FIRST_SPACE);
    rig.milestone(SHARED);
    let before = rig.everything();
    for here in [
        turn("two-a"),
        HashSet::from(["two-a".into(), "one-a".into()]),
    ] {
        let turns = || Some(here.clone());
        assert_eq!(
            removal::remove(&rig.store, TWO, SHARED, turns),
            Err(running_here())
        );
        assert_eq!(
            removal::delete(&rig.store, TWO, SHARED, turns, |id| rig.end(id)).err(),
            Some(running_here()),
            "\"here\" wins over another Space"
        );
        assert_eq!(
            removal::delete_preview(&rig.store, TWO, SHARED, turns).err(),
            Some(running_here())
        );
    }
    assert!(matches!(
        removal::remove(&rig.store, TWO, SHARED, || None),
        Err(ProjectRefusal::Failed { .. })
    ));
    assert!(matches!(
        removal::delete(&rig.store, TWO, SHARED, || None, |id| rig.end(id)),
        Err(ProjectRefusal::Failed { .. })
    ));
    assert_eq!(rig.everything(), before);

    rig.sql(
        "INSERT INTO in_flight_sessions (session_id, cwd, sort_index, window_label, run_id)
         VALUES ('two-a', ?1, 0, 'main', 'run')",
        [SHARED],
    );
    assert_eq!(rig.remove(TWO, SHARED), Err(running_here()), "in flight");
    rig.sql("DELETE FROM in_flight_sessions", []);

    let deleted = removal::delete(
        &rig.store,
        TWO,
        SHARED,
        || Some(turn("one-a")),
        |id| rig.end(id),
    );
    assert_eq!(
        deleted.unwrap().deleted_session_ids,
        ["two-a"],
        "Space 1's turn is no guard on Space 2's delete"
    );
    assert_eq!(rig.sessions(), ["one-a"], "and it keeps running");

    rig.session("one-b", "/work/arch", FIRST_SPACE);
    let archive_refused =
        removal::archive_check(&rig.store, TWO, "/work/arch", || Some(turn("one-b")));
    assert!(
        matches!(&archive_refused, Err(ProjectRefusal::SessionRunning { message, .. })
            if message == "A session is running in arch in Space 1. Stop it first."),
        "R14, G3: {archive_refused:?}"
    );

    let last = "/work/last";
    rig.assign(last, TWO);
    rig.session("hidden", last, FIRST_SPACE);
    let refused = removal::delete(
        &rig.store,
        TWO,
        last,
        || Some(turn("hidden")),
        |id| rig.end(id),
    );
    assert!(
        matches!(refused.as_ref().err(), Some(ProjectRefusal::SessionRunning { message, .. })
            if message == "A session is running in last in Space 1. Stop it first."),
        "T11 names the other Space"
    );
}

/// Defect (F13): T6 or T11 silently moves an automation, in T5 or T10 words a
/// delete stops for an automation that keeps its Space, or a refused T10 keeps
/// its deleted sessions.
#[test]
fn an_automation_stops_what_would_move_its_space() {
    let rig = Rig::new();
    rig.assign(SHARED, TWO);
    rig.automation("nightly", "Nightly report", "/work/sample-app/");
    rig.session("two-a", SHARED, TWO);
    let automation = ProjectRefusal::Automation {
        project: "sample-app".into(),
        automations: vec!["Nightly report".into()],
        message: "The automation Nightly report runs in sample-app. Move or delete it first."
            .into(),
    };
    rig.delete_refused(TWO, SHARED, &automation);

    rig.link(FIRST_SPACE, SHARED);
    rig.delete_refused(TWO, SHARED, &automation);
    assert_eq!(rig.remove(TWO, SHARED), Err(automation.clone()), "T6");
    assert_eq!(
        rig.remove(FIRST_SPACE, SHARED),
        Ok(true),
        "T5 moves nothing"
    );

    rig.link(FIRST_SPACE, SHARED);
    rig.session("one-a", SHARED, FIRST_SPACE);
    let kept = rig.delete(FIRST_SPACE, SHARED).unwrap();
    assert_eq!(kept.deleted_session_ids, ["one-a"], "T10 keeps its Space");

    rig.automation("weekly", "Weekly", SHARED);
    let refused = rig.delete(TWO, SHARED).err().unwrap();
    assert!(
        matches!(&refused, ProjectRefusal::Automation { message, .. }
            if message == "The automations Nightly report and Weekly run in sample-app. \
                           Move or delete them first."),
        "{refused:?}"
    );
}

/// Defect (G10, F19, I8): a guard is read after the first delete instead of
/// before it, so a refused delete leaves chats gone; a turn that starts in a
/// session still to go does not stop the delete; a failed session delete still
/// takes the project out of the Space; or a Mono that arrives before the
/// membership write is not seen. A failure halfway leaves the project listed,
/// and a retry finishes it.
#[test]
fn every_guard_passes_before_the_first_delete_and_membership_commits_last() {
    let rig = Rig::new();
    rig.assign(SHARED, TWO);
    rig.link(FIRST_SPACE, SHARED);
    rig.automation("nightly", "Nightly report", SHARED);
    rig.session("two-a", SHARED, TWO);
    rig.session("two-b", SHARED, TWO);
    let before = rig.everything();
    let refused = removal::delete(&rig.store, TWO, SHARED, no_turns, |id| {
        panic!("deleted {id} before T6's automation guard refused")
    });
    assert!(matches!(refused, Err(ProjectRefusal::Automation { .. })));
    assert_eq!(rig.everything(), before);
    rig.sql("DELETE FROM automations", []);

    let reads = Cell::new(0);
    let turns = || {
        reads.set(reads.get() + 1);
        Some(if reads.get() < 3 {
            Turns::new()
        } else {
            HashSet::from(["two-a".to_owned(), "two-b".to_owned()])
        })
    };
    let ended = Cell::new(0);
    let refused = removal::delete(&rig.store, TWO, SHARED, turns, |id| {
        ended.set(ended.get() + 1);
        rig.end(id)
    });
    assert_eq!(refused.err(), Some(running_here()));
    assert_eq!(ended.get(), 1, "the delete went on past the new turn");
    assert_eq!(rig.spaces_of(SHARED), [TWO, FIRST_SPACE], "P stays listed");

    let failed = removal::delete(&rig.store, TWO, SHARED, no_turns, |_| {
        Err("the store said no".to_owned())
    });
    assert_eq!(
        failed.err(),
        Some(ProjectRefusal::Failed {
            message: "the store said no".into()
        })
    );
    assert_eq!(rig.spaces_of(SHARED), [TWO, FIRST_SPACE], "P stays listed");

    let late = removal::delete(&rig.store, TWO, SHARED, no_turns, |id| {
        rig.mono("late", "Late", TWO, &[SHARED]);
        rig.end(id)
    });
    assert!(matches!(late, Err(ProjectRefusal::MonoHeld { .. })));
    assert_eq!(rig.spaces_of(SHARED), [TWO, FIRST_SPACE], "P stays listed");
    rig.sql("DELETE FROM monos", []);

    let finished = rig.delete(TWO, SHARED).unwrap();
    assert!(!finished.last_space);
    assert_eq!(
        rig.spaces_of(SHARED),
        [FIRST_SPACE],
        "the retry finished it"
    );
    assert!(rig.sessions().is_empty());

    let last = "/work/last";
    rig.assign(last, TWO);
    rig.session("last-a", last, TWO);
    rig.milestone(last);
    let gained = removal::delete(&rig.store, TWO, last, no_turns, |id| {
        rig.link(FIRST_SPACE, last);
        rig.end(id)
    })
    .unwrap();
    assert!(!gained.last_space, "a Space gained it during the delete");
    assert_eq!(
        rig.strings("SELECT project_path FROM milestones"),
        [last],
        "so its shared milestones stay"
    );
    assert_eq!(
        rig.spaces_of(last),
        [FIRST_SPACE],
        "T6 moved the assignment"
    );
}

/// Defect (T5, T6, I2, I4, I7, T16): a removal deletes a session, moves a lease,
/// or strands a project: the assignment goes to any but the first other Space,
/// or stays on a Space that left. An assigned project in no other Space is
/// refused. A remote project follows the same states.
#[test]
fn removing_a_project_drops_its_link_or_moves_its_assignment_and_keeps_its_sessions() {
    let rig = Rig::new();
    let three = spaces::create(&rig.store, "Three", None, 2).unwrap().id;
    rig.sql(
        "INSERT INTO checkout_leases (checkout_path, space_id, leased_at) VALUES (?1, ?2, 1)",
        params![SHARED, TWO],
    );
    let leases = || rig.strings("SELECT space_id FROM checkout_leases");
    rig.link(TWO, SHARED);
    rig.session("two-a", SHARED, TWO);

    assert_eq!(rig.remove(TWO, SHARED), Ok(true), "T5");
    assert_eq!(rig.spaces_of(SHARED), [FIRST_SPACE]);
    assert_eq!(rig.sessions(), ["two-a"]);
    assert_eq!(
        rig.remove(TWO, SHARED),
        Ok(false),
        "I9: it is not in Space 2"
    );

    rig.link(TWO, SHARED);
    rig.link(&three, SHARED);
    assert_eq!(rig.remove(FIRST_SPACE, SHARED), Ok(true), "T6, no row");
    assert_eq!(
        rig.strings("SELECT space_id FROM space_projects"),
        [TWO],
        "the oldest other Space takes the assignment"
    );
    assert_eq!(rig.spaces_of(SHARED), [TWO, three.as_str()]);

    assert_eq!(rig.remove(TWO, SHARED), Ok(true), "T6 again");
    assert_eq!(rig.spaces_of(SHARED), [three.as_str()]);
    assert_eq!(leases(), [TWO], "a held lease never moves");

    let only = ProjectRefusal::OnlySpace {
        project: "sample-app".into(),
        message: "sample-app is only in this Space. Archive or delete it instead.".into(),
    };
    assert_eq!(rig.remove(&three, SHARED), Err(only));
    assert_eq!(rig.spaces_of(SHARED), [three.as_str()], "I7");
    assert!(matches!(
        rig.remove(TWO, "~"),
        Err(ProjectRefusal::Failed { .. })
    ));

    let remote = "remote://host/work/app";
    rig.link(TWO, remote);
    assert!(
        rig.delete(TWO, remote)
            .unwrap()
            .deleted_session_ids
            .is_empty(),
        "T16: a remote project has no saved chats"
    );
    assert_eq!(rig.spaces_of(remote), [FIRST_SPACE], "membership only");
    rig.link(TWO, remote);
    assert_eq!(rig.remove(TWO, remote), Ok(true), "Remove applies");
}

/// Defect: a command acts on a Space other than the calling window's, does not
/// call its function, emits on a refusal or when nothing changed, or its reply
/// is not the shape the window reads.
#[test]
fn the_commands_read_the_windows_space_and_tell_every_window_of_a_change() {
    let rig = Rig::new();
    rig.link(TWO, SHARED);
    rig.session("two-a", SHARED, TWO);
    let app = tauri::test::mock_builder()
        .manage(rig.store)
        .manage(crate::control::tests::host())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = super::agent_sandbox::main_window(&app);
    let heard = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = heard.clone();
    app.listen_any(projects::CHANGED, move |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let events = || heard.load(std::sync::atomic::Ordering::SeqCst);
    let remove = |path: &str| {
        removal::spaces_project_remove(
            window.clone(),
            app.handle().clone(),
            app.state(),
            app.state(),
            path.into(),
        )
    };
    let switch_to = |space: &str| {
        let patch = SettingsPatch {
            active_space_id: Some(space.into()),
            ..SettingsPatch::default()
        };
        spaces::spaces_settings_set(window.clone(), app.state(), patch).unwrap();
    };

    switch_to(TWO);
    let preview = removal::spaces_project_delete_preview(
        window.clone(),
        app.state(),
        app.state(),
        SHARED.into(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(preview).unwrap(),
        json!({
            "lastSpace": false,
            "sessionCount": 1,
            "otherSpaces": [FIRST_SPACE],
            "hiddenSessionCount": 0,
        })
    );

    assert_eq!(remove(SHARED), Ok(true));
    assert_eq!(events(), 1, "a removal emits");
    assert_eq!(remove(SHARED), Ok(false));
    assert_eq!(events(), 1, "nothing changed, nothing to tell");

    let state: tauri::State<'_, SessionStore> = app.state();
    state
        .lock_conn()
        .unwrap()
        .execute(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
             VALUES ('held', 'Held', 'mono', ?1, 0, 0, 0, ?2)",
            params![json!({"projects": [SHARED]}).to_string(), FIRST_SPACE],
        )
        .unwrap();
    switch_to(FIRST_SPACE);
    let refused = remove(SHARED).unwrap_err();
    assert_eq!(events(), 1, "a refusal emits nothing");
    assert_eq!(
        serde_json::to_value(&refused).unwrap(),
        json!({
            "code": "monoHeld",
            "project": "sample-app",
            "monos": ["Held"],
            "message": "Held uses sample-app. Take it off that Mono first."
        })
    );
    let archive = |path: &str| {
        removal::spaces_project_archive_check(window.clone(), app.state(), app.state(), path.into())
    };
    assert!(matches!(
        archive(SHARED),
        Err(ProjectRefusal::MonoHeld { message, .. })
            if message.starts_with("Held uses sample-app, and Archive")
    ));
    assert_eq!(archive("/work/free"), Ok(()));
    removal::spaces_project_notify_changed(app.handle().clone());
    assert_eq!(events(), 2, "T9 writes only the window's storage");
}
