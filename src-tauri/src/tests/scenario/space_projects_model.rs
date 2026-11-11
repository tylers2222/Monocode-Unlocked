//! ADE-235 spec, Proof 1: a breadth-first walk of every event on 2 windows, 2
//! Spaces, 2 Monos and 2 projects, with a plain reference model beside the real
//! functions. After each step the store must read as the model says, each reply
//! must be the model's, and every invariant must hold. It runs on one in-memory
//! store, restored from the model before each event. No test opens real data.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::time::Instant;

use rusqlite::{params, Connection};
use serde_json::json;

use crate::session_store::{delete_session, rebase_project, Removal, SessionStore};
use crate::spaces::project_add::{self, Added};
use crate::spaces::project_removal::{self as removal, ProjectRefusal, Turns};
use crate::spaces::{self, projects, FIRST_SPACE};

const SPACES: [&str; 2] = [FIRST_SPACE, "space-2"];
/// One local project and one remote: a remote project follows the same states.
const PROJECTS: [&str; 2] = ["/work/p", "remote://host/work/q"];
const WINDOWS: [&str; 2] = ["w1", "w2"];
/// The spec asks for depth 6, and allows less if a scenario test's time budget
/// needs it.
/// It is: on the 4 vCPU box, depth 3 is 1841 states in 13 s and depth 4 is 9613
/// states in 71 s, about five times per depth, so depth 5 is near 6 minutes.
const DEPTH: usize = 4;

/// One Mono: it stays in its Space for life, as nothing moves a Mono.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Mono {
    exists: bool,
    lists: [bool; 2],
}

/// One session per Space and project at most: what a guard can tell apart.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Chat {
    exists: bool,
    running: bool,
}

/// The reference model, in the spec's words.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Model {
    /// `space_projects`: `None` is no row, which is Space 1.
    row: [Option<usize>; 2],
    /// `L`, by Space then project.
    link: [[bool; 2]; 2],
    /// Mono `m` is in Space `m`.
    mono: [Mono; 2],
    /// By Space then project.
    chat: [[Chat; 2]; 2],
    /// Shared data of a project: its milestones.
    milestone: [bool; 2],
    /// An automation whose folder is the project.
    automation: [bool; 2],
    /// Each window's Space.
    window: [usize; 2],
}

impl Model {
    fn start() -> Self {
        Self {
            row: [None; 2],
            link: [[false; 2]; 2],
            mono: [Mono {
                exists: true,
                lists: [false; 2],
            }; 2],
            chat: [[Chat::default(); 2]; 2],
            milestone: [true; 2],
            automation: [false; 2],
            window: [0, 1],
        }
    }

    fn assigned(&self, project: usize) -> usize {
        self.row[project].unwrap_or(0)
    }

    fn m(&self, space: usize, project: usize) -> bool {
        self.mono[space].exists && self.mono[space].lists[project]
    }

    fn v(&self, space: usize, project: usize) -> bool {
        self.assigned(project) == space || self.link[space][project] || self.m(space, project)
    }

    fn known(&self, project: usize) -> bool {
        self.row[project].is_some()
            || (0..2).any(|space| {
                self.link[space][project]
                    || self.m(space, project)
                    || self.chat[space][project].exists
            })
    }

    fn running(&self, space: usize, project: usize) -> bool {
        self.chat[space][project].running
    }

    fn running_anywhere(&self, project: usize) -> bool {
        (0..2).any(|space| self.running(space, project))
    }

    /// T5, T6, T7, T8, in the refusal order the spec approves.
    fn remove(&mut self, space: usize, project: usize) -> Reply {
        if self.m(space, project) {
            return Reply::Refused("monoHeld");
        }
        let assigned_here = self.assigned(project) == space;
        if !assigned_here && !self.link[space][project] {
            return Reply::Nothing;
        }
        let other = 1 - space;
        let other_has = self.link[other][project] || self.m(other, project);
        if assigned_here && !other_has {
            return Reply::Refused("onlySpace");
        }
        if assigned_here && self.automation[project] {
            return Reply::Refused("automation");
        }
        if self.running(space, project) {
            return Reply::Refused("sessionRunning");
        }
        if assigned_here {
            self.row[project] = Some(other);
        }
        self.link[space][project] = false;
        Reply::Done
    }

    /// T9.
    fn archive(&self, project: usize) -> Reply {
        if (0..2).any(|space| self.m(space, project)) {
            Reply::Refused("monoHeld")
        } else if self.running_anywhere(project) {
            Reply::Refused("sessionRunning")
        } else {
            Reply::Done
        }
    }

    /// T10, T11, T16. Only this Space's sessions, in either.
    fn delete(&mut self, space: usize, project: usize) -> Reply {
        let last = !self.v(1 - space, project);
        if self.m(space, project) {
            return Reply::Refused("monoHeld");
        }
        let running = if last {
            self.running_anywhere(project)
        } else {
            self.running(space, project)
        };
        if running {
            return Reply::Refused("sessionRunning");
        }
        if self.assigned(project) == space && self.automation[project] {
            return Reply::Refused("automation");
        }
        self.chat[space][project] = Chat::default();
        if last {
            self.row[project] = None;
            self.link = self.link.map(|mut links| {
                links[project] = false;
                links
            });
            self.milestone[project] = false;
        } else {
            let was_assigned = self.assigned(project) == space;
            if was_assigned {
                self.row[project] = Some(1 - space);
            }
            self.link[space][project] = false;
        }
        Reply::Done
    }

    /// T1, T2.
    fn add(&mut self, space: usize, project: usize, known_to_window: bool) -> Reply {
        if known_to_window || self.known(project) {
            if self.assigned(project) != space {
                self.link[space][project] = true;
            }
            Reply::Added(Added::Linked)
        } else {
            self.row[project] = Some(space);
            Reply::Added(Added::Assigned)
        }
    }

    /// T3, T4: a Mono's list changes, and what it holds or held stays linked.
    fn set_list(&mut self, mono: usize, project: usize, lists: bool) {
        if !self.mono[mono].exists {
            return;
        }
        for listed in 0..2 {
            if self.mono[mono].lists[listed] || (listed == project && lists) {
                self.link[mono][listed] = true;
            }
        }
        self.mono[mono].lists[project] = lists;
    }

    /// T4, and a Mono made anew with an empty list.
    fn set_exists(&mut self, mono: usize, exists: bool) {
        if self.mono[mono].exists && !exists {
            for project in 0..2 {
                if self.mono[mono].lists[project] {
                    self.link[mono][project] = true;
                }
            }
        }
        self.mono[mono] = Mono {
            exists,
            lists: if exists {
                self.mono[mono].lists
            } else {
                [false; 2]
            },
        };
    }

    /// T12 onto the other project: its rows unite, the old assignment wins.
    fn rename(&mut self, from: usize) {
        let to = 1 - from;
        if self.known(from) {
            let winner = self.assigned(from);
            let to_has_rows = self.row[to].is_some()
                || (0..2).any(|space| self.link[space][to] || self.m(space, to));
            if to_has_rows && self.assigned(to) != winner {
                let loser = self.assigned(to);
                self.link[loser][to] = true;
            }
            self.row[to] = self.row[from];
            self.row[from] = None;
        }
        for space in 0..2 {
            self.link[space][to] = self.link[space][to] || self.link[space][from];
            self.link[space][from] = false;
            let chat = self.chat[space][from];
            if chat.exists {
                self.chat[space][to] = chat;
            }
            self.chat[space][from] = Chat::default();
            let listed = self.mono[space].lists;
            if listed[from] {
                self.mono[space].lists[to] = true;
                self.mono[space].lists[from] = false;
            }
        }
        if self.milestone[from] {
            self.milestone[to] = true;
        }
        self.milestone[from] = false;
        self.automation[to] = self.automation[to] || self.automation[from];
        self.automation[from] = false;
    }
}

/// What an event answered. A refusal is its typed code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reply {
    Done,
    Nothing,
    Added(Added),
    Refused(&'static str),
}

#[derive(Clone, Copy, Debug)]
enum Event {
    Add(usize, usize, bool),
    Remove(usize, usize),
    Archive(usize, usize),
    Delete(usize, usize),
    SwitchWindow(usize, usize),
    SetList(usize, usize, bool),
    SetMonoExists(usize, bool),
    Rename(usize),
    Chat(usize, usize),
    Run(usize, usize, bool),
    Automation(usize, bool),
}

fn events() -> Vec<Event> {
    let mut all = Vec::new();
    for project in 0..2 {
        for window in 0..2 {
            all.push(Event::Add(window, project, false));
            all.push(Event::Add(window, project, true));
            all.push(Event::Remove(window, project));
            all.push(Event::Archive(window, project));
            all.push(Event::Delete(window, project));
        }
        for space in 0..2 {
            all.push(Event::SetList(space, project, true));
            all.push(Event::SetList(space, project, false));
            all.push(Event::Chat(space, project));
            all.push(Event::Run(space, project, true));
            all.push(Event::Run(space, project, false));
        }
        all.push(Event::Rename(project));
        all.push(Event::Automation(project, true));
        all.push(Event::Automation(project, false));
    }
    for index in 0..2 {
        all.push(Event::SwitchWindow(index, 0));
        all.push(Event::SwitchWindow(index, 1));
        all.push(Event::SetMonoExists(index, false));
        all.push(Event::SetMonoExists(index, true));
    }
    all
}

fn session_id(space: usize, project: usize) -> String {
    format!("s{space}p{project}")
}

/// Writes the model into the store, so the next event starts from that state.
fn restore(conn: &Connection, model: &Model, store: &SessionStore) {
    conn.execute_batch(
        "BEGIN;
         DELETE FROM space_projects; DELETE FROM space_project_links; DELETE FROM monos;
         DELETE FROM sessions; DELETE FROM milestones; DELETE FROM automations;",
    )
    .unwrap();
    for (project, path_of) in PROJECTS.iter().enumerate() {
        if let Some(space) = model.row[project] {
            conn.execute(
                "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
                params![*path_of, SPACES[space]],
            )
            .unwrap();
        }
        if model.milestone[project] {
            conn.execute(
                "INSERT INTO milestones (id, project_path, title, status, sort_order, created_by, created_at, updated_at)
                 VALUES (?1, ?2, 'goal', 'idea', 0, 'user', 1, 1)",
                params![format!("{project}").repeat(36), *path_of],
            )
            .unwrap();
        }
        if model.automation[project] {
            conn.execute(
                "INSERT INTO automations (id, definition_json, enabled, next_run_at, updated_at)
                 VALUES (?1, ?2, 1, 1, 1)",
                params![
                    format!("auto{project}"),
                    json!({"name": format!("auto{project}"), "cwd": *path_of}).to_string()
                ],
            )
            .unwrap();
        }
        for (space, space_of) in SPACES.iter().enumerate() {
            if model.link[space][project] {
                conn.execute(
                    "INSERT INTO space_project_links (space_id, project_path) VALUES (?1, ?2)",
                    params![*space_of, *path_of],
                )
                .unwrap();
            }
            if model.chat[space][project].exists {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, has_user_message, space_id)
                     VALUES (?1, ?2, 'codex', 'm', 'auto', 't', 1, 1, 1, ?3)",
                    params![session_id(space, project), *path_of, *space_of],
                )
                .unwrap();
            }
        }
    }
    for (index, mono) in model.mono.iter().enumerate() {
        if mono.exists {
            let listed: Vec<&str> = (0..2)
                .filter(|&project| mono.lists[project])
                .map(|project| PROJECTS[project])
                .collect();
            conn.execute(
                "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
                 VALUES (?1, ?1, 'mono', ?2, ?3, 0, 0, ?4)",
                params![
                    format!("mono{index}"),
                    json!({"mascot": "owl", "projects": listed}).to_string(),
                    index as i64,
                    SPACES[index]
                ],
            )
            .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
    let mut windows = store.window_spaces.lock().unwrap();
    for (index, label) in WINDOWS.iter().enumerate() {
        windows.insert((*label).to_owned(), SPACES[model.window[index]].to_owned());
    }
}

fn strings(conn: &Connection, sql: &str) -> BTreeSet<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The store as the model would write it.
fn expected_rows(model: &Model) -> Vec<BTreeSet<String>> {
    let mut rows = BTreeSet::new();
    let mut links = BTreeSet::new();
    let mut chats = BTreeSet::new();
    let mut lists = BTreeSet::new();
    let mut milestones = BTreeSet::new();
    let mut automations = BTreeSet::new();
    for (project, path_of) in PROJECTS.iter().enumerate() {
        let path = *path_of;
        if let Some(space) = model.row[project] {
            rows.insert(format!("{path} {}", SPACES[space]));
        }
        if model.milestone[project] {
            milestones.insert(path.to_owned());
        }
        if model.automation[project] {
            automations.insert(path.to_owned());
        }
        for (space, space_of) in SPACES.iter().enumerate() {
            if model.link[space][project] {
                links.insert(format!("{} {path}", *space_of));
            }
            if model.m(space, project) {
                lists.insert(format!("{} {path}", *space_of));
            }
        }
    }
    for (space, space_of) in SPACES.iter().enumerate() {
        for (project, path_of) in PROJECTS.iter().enumerate() {
            if model.chat[space][project].exists {
                chats.insert(format!("{} {}", *space_of, *path_of));
            }
        }
    }
    vec![rows, links, chats, lists, milestones, automations]
}

fn actual_rows(conn: &Connection) -> Vec<BTreeSet<String>> {
    vec![
        strings(conn, "SELECT project_path || ' ' || space_id FROM space_projects"),
        strings(conn, "SELECT space_id || ' ' || project_path FROM space_project_links"),
        strings(conn, "SELECT space_id || ' ' || cwd FROM sessions"),
        strings(
            conn,
            "SELECT m.space_id || ' ' || j.value FROM monos m, json_each(m.data_json, '$.projects') j",
        ),
        strings(conn, "SELECT project_path FROM milestones"),
        strings(conn, "SELECT json_extract(definition_json, '$.cwd') FROM automations"),
    ]
}

/// I1, I2, I3, I7 over the real resolver and the real list.
fn check_invariants(store: &SessionStore, model: &Model, path: &[Event]) {
    for (project, path_of) in PROJECTS.iter().enumerate() {
        let path_text = *path_of;
        let resolver = {
            let conn = store.lock_conn().unwrap();
            spaces::project_spaces_in(&conn, path_text).unwrap()
        };
        assert_eq!(resolver[0], SPACES[model.assigned(project)], "I2: {path:?}");
        assert!(!resolver.is_empty(), "I7: {path:?}");
        for (space, space_of) in SPACES.iter().enumerate() {
            let reply = projects::list(store, space_of).unwrap();
            let listed = reply.projects.iter().any(|p| p.path == path_text);
            let elsewhere = reply.assigned_elsewhere.iter().any(|p| p == path_text);
            let window_says = listed || (reply.no_row_is_here && !elsewhere);
            assert_eq!(
                resolver.iter().any(|s| s == *space_of),
                window_says,
                "I3 for {} {path_text}: {path:?}",
                *space_of
            );
            assert_eq!(
                window_says,
                model.v(space, project),
                "V for {} {path_text}: {path:?}",
                *space_of
            );
            if model.m(space, project) {
                assert!(listed, "I1 for {} {path_text}: {path:?}", *space_of);
                assert!(
                    reply.mono_held.iter().any(|p| p == path_text),
                    "monoHeld for {path_text}: {path:?}"
                );
            }
        }
    }
}

fn code(refusal: &ProjectRefusal) -> &'static str {
    match refusal {
        ProjectRefusal::MonoHeld { .. } => "monoHeld",
        ProjectRefusal::SessionRunning { .. } => "sessionRunning",
        ProjectRefusal::MonoChat { .. } => "monoChat",
        ProjectRefusal::Automation { .. } => "automation",
        ProjectRefusal::OnlySpace { .. } => "onlySpace",
        ProjectRefusal::Workspace { .. } => "workspace",
        ProjectRefusal::Failed { .. } => "failed",
    }
}

fn turns_of(model: &Model) -> Turns {
    let mut turns = HashSet::new();
    for space in 0..2 {
        for project in 0..2 {
            if model.chat[space][project].running {
                turns.insert(session_id(space, project));
            }
        }
    }
    turns
}

/// A user's action in a window: the real function's reply, and the model's.
fn window_event(store: &SessionStore, model: &mut Model, event: Event) -> (Reply, Reply) {
    let turns = turns_of(model);
    let now = || Some(turns.clone());
    let space_of = |window: usize| {
        let space = spaces::shown_space(store, WINDOWS[window]).unwrap();
        SPACES.iter().position(|id| *id == space).unwrap()
    };
    let end = |id: &str| -> Result<(), String> {
        let conn = store.lock_conn()?;
        delete_session(&conn, id, Removal::End)
            .map(drop)
            .map_err(|error| error.to_string())
    };
    let refused = |refusal: ProjectRefusal| Reply::Refused(code(&refusal));
    match event {
        Event::Add(window, project, known) => {
            let space = space_of(window);
            let real = project_add::add(store, SPACES[space], PROJECTS[project], known)
                .map_or_else(refused, |(added, _)| Reply::Added(added));
            (real, model.add(space, project, known))
        }
        Event::Remove(window, project) => {
            let space = space_of(window);
            let real = removal::remove(store, SPACES[space], PROJECTS[project], now).map_or_else(
                refused,
                |left| if left { Reply::Done } else { Reply::Nothing },
            );
            (real, model.remove(space, project))
        }
        Event::Archive(window, project) => {
            let space = space_of(window);
            let real = removal::archive_check(store, SPACES[space], PROJECTS[project], now)
                .map_or_else(refused, |()| Reply::Done);
            (real, model.archive(project))
        }
        Event::Delete(window, project) => {
            let space = space_of(window);
            let real = removal::delete(store, SPACES[space], PROJECTS[project], now, end)
                .map_or_else(refused, |_| Reply::Done);
            (real, model.delete(space, project))
        }
        _ => unreachable!("not a window event: {event:?}"),
    }
}

/// T3 and T4: a Mono's list changes through the roster's real writer.
fn set_list(store: &SessionStore, model: &mut Model, mono: usize, project: usize, lists: bool) {
    if model.mono[mono].exists {
        let mut after = model.mono[mono].lists;
        after[project] = lists;
        let listed: Vec<&str> = (0..2)
            .filter(|&index| after[index])
            .map(|index| PROJECTS[index])
            .collect();
        let update = serde_json::from_value(json!({
            "id": format!("mono{mono}"),
            "patch": {"projects": listed},
        }))
        .unwrap();
        let update = crate::mono_roster::validate_update(update).unwrap();
        crate::mono_roster::update_many(&store.lock_conn().unwrap(), &[update]).unwrap();
    }
    model.set_list(mono, project, lists);
}

/// T4: a Mono is deleted through the roster's real writer. Making one anew writes
/// the row directly: its Space is fixed in this model.
fn set_mono_exists(store: &SessionStore, model: &mut Model, mono: usize, exists: bool) {
    let conn = store.lock_conn().unwrap();
    let id = format!("mono{mono}");
    if !exists {
        crate::mono_roster::delete(&conn, &id).unwrap();
    } else if !model.mono[mono].exists {
        conn.execute(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
             VALUES (?1, ?1, 'mono', '{\"mascot\":\"owl\",\"projects\":[]}', ?2, 0, 0, ?3)",
            params![id, mono as i64, SPACES[mono]],
        )
        .unwrap();
    }
    drop(conn);
    model.set_exists(mono, exists);
}

/// T12 onto the other project. The model holds one chat per Space and project, so
/// a chat that moved takes the new pair's id.
fn rename(store: &SessionStore, model: &mut Model, from: usize) {
    let to = 1 - from;
    let conn = store.lock_conn().unwrap();
    rebase_project(&conn, PROJECTS[from], PROJECTS[to]).unwrap();
    for space in 0..2 {
        if model.chat[space][from].exists {
            if model.chat[space][to].exists {
                conn.execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    [session_id(space, to)],
                )
                .unwrap();
            }
            conn.execute(
                "UPDATE sessions SET id = ?2 WHERE id = ?1",
                params![session_id(space, from), session_id(space, to)],
            )
            .unwrap();
        }
    }
    drop(conn);
    model.rename(from);
}

fn add_chat(store: &SessionStore, model: &mut Model, space: usize, project: usize) {
    if !model.chat[space][project].exists {
        model.chat[space][project].exists = true;
        store
            .lock_conn()
            .unwrap()
            .execute(
                "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, has_user_message, space_id)
                 VALUES (?1, ?2, 'codex', 'm', 'auto', 't', 1, 1, 1, ?3)",
                params![session_id(space, project), PROJECTS[project], SPACES[space]],
            )
            .unwrap();
    }
}

fn set_automation(store: &SessionStore, model: &mut Model, project: usize, on: bool) {
    let conn = store.lock_conn().unwrap();
    if on && !model.automation[project] {
        conn.execute(
            "INSERT INTO automations (id, definition_json, enabled, next_run_at, updated_at)
             VALUES (?1, ?2, 1, 1, 1)",
            params![
                format!("auto{project}"),
                json!({"name": "a", "cwd": PROJECTS[project]}).to_string()
            ],
        )
        .unwrap();
    }
    if !on {
        conn.execute(
            "DELETE FROM automations WHERE json_extract(definition_json, '$.cwd') = ?1",
            [PROJECTS[project]],
        )
        .unwrap();
    }
    model.automation[project] = on;
}

/// Runs `event` on the real functions and on `model`; the two replies must match.
fn step(store: &SessionStore, model: &mut Model, event: Event, path: &[Event]) {
    let (real, reference) = match event {
        Event::Add(..) | Event::Remove(..) | Event::Archive(..) | Event::Delete(..) => {
            window_event(store, model, event)
        }
        other => {
            match other {
                Event::SwitchWindow(window, space) => {
                    model.window[window] = space;
                    store
                        .window_spaces
                        .lock()
                        .unwrap()
                        .insert(WINDOWS[window].to_owned(), SPACES[space].to_owned());
                }
                Event::SetList(mono, project, lists) => {
                    set_list(store, model, mono, project, lists)
                }
                Event::SetMonoExists(mono, exists) => set_mono_exists(store, model, mono, exists),
                Event::Rename(from) => rename(store, model, from),
                Event::Chat(space, project) => add_chat(store, model, space, project),
                Event::Run(space, project, running) => {
                    let chat = &mut model.chat[space][project];
                    chat.running = chat.exists && running;
                }
                Event::Automation(project, on) => set_automation(store, model, project, on),
                _ => unreachable!(),
            }
            (Reply::Done, Reply::Done)
        }
    };
    assert_eq!(
        real, reference,
        "the reply differs after {path:?} {event:?}"
    );
}

/// I5: a running session survives every event.
fn check_running_sessions_survive(state: &Model, after: &Model, event: Event, walked: &[Event]) {
    for space in 0..2 {
        for project in 0..2 {
            if state.chat[space][project].running {
                let moved = matches!(event, Event::Rename(from) if from == project);
                let at = if moved { 1 - project } else { project };
                assert!(
                    after.chat[space][at].exists,
                    "I5: a running session went: {walked:?}"
                );
            }
        }
    }
}

/// Defect: any path of up to `DEPTH` events reaches a state where the real
/// functions and the spec's model disagree on a reply or a row, a running session
/// is gone or changed, a refused event changed anything, a repeated event changed
/// anything, or an invariant fails.
#[test]
fn every_path_of_events_matches_the_reference_model() {
    let started = Instant::now();
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Space 2", None, 1).unwrap();
    let events = events();
    let mut seen: HashSet<Model> = HashSet::new();
    let mut frontier = vec![(Model::start(), Vec::new())];
    seen.insert(Model::start());
    let mut steps = 0_u64;
    for depth in 1..=DEPTH {
        let mut next = Vec::new();
        for (state, path) in &frontier {
            let mut store_is_at_state = false;
            for &event in &events {
                if !store_is_at_state {
                    restore(&store.lock_conn().unwrap(), state, &store);
                }
                let walked: Vec<Event> =
                    path.iter().copied().chain(std::iter::once(event)).collect();
                let mut model = state.clone();
                step(&store, &mut model, event, path);
                steps += 1;
                let changed = model != *state;
                store_is_at_state = !changed;
                if !changed {
                    assert_eq!(
                        actual_rows(&store.lock_conn().unwrap()),
                        expected_rows(state),
                        "a no-op changed rows, or a refusal did (I8): {walked:?}"
                    );
                    continue;
                }
                assert_eq!(
                    actual_rows(&store.lock_conn().unwrap()),
                    expected_rows(&model),
                    "rows after {walked:?}"
                );
                check_running_sessions_survive(state, &model, event, &walked);
                let mut again = model.clone();
                step(&store, &mut again, event, &walked);
                assert_eq!(again, model, "I9 in the model: {walked:?}");
                assert_eq!(
                    actual_rows(&store.lock_conn().unwrap()),
                    expected_rows(&model),
                    "I9: {walked:?}"
                );
                if seen.insert(model.clone()) {
                    check_invariants(&store, &model, &walked);
                    next.push((model, walked));
                }
            }
        }
        frontier = next;
        // Not captured by the test harness: the depth and runtime go in the PR.
        writeln!(
            std::io::stderr(),
            "space projects model: depth {depth}, {} states, {steps} steps, {:?}",
            seen.len(),
            started.elapsed()
        )
        .unwrap();
    }
}

/// R13, General's rule. Defect: "last Space" reads as "no chats anywhere", so a
/// delete takes the chats another Space left hidden when it removed the project.
/// A project with 3 chats leaves Space 1 and is deleted in Space 2, its last
/// Space: Space 1's chats stay, and return when Space 1 adds it again.
#[test]
fn a_last_space_delete_keeps_the_chats_another_space_left_hidden() {
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Space 2", None, 1).unwrap();
    let (one, two) = (SPACES[0], SPACES[1]);
    let path = PROJECTS[0];
    let conn = || store.lock_conn().unwrap();
    for (id, space) in [
        ("one-a", one),
        ("one-b", one),
        ("one-c", one),
        ("two-a", two),
    ] {
        conn()
            .execute(
                "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, has_user_message, space_id)
                 VALUES (?1, ?2, 'codex', 'm', 'auto', 't', 1, 1, 1, ?3)",
                params![id, path, space],
            )
            .unwrap();
    }
    let none = || Some(Turns::new());
    assert_eq!(
        project_add::add(&store, two, path, true).map(|(added, _)| added),
        Ok(Added::Linked)
    );
    assert_eq!(removal::remove(&store, one, path, none), Ok(true), "T6");
    assert_eq!(spaces::project_spaces_in(&conn(), path).unwrap(), [two]);

    let preview = removal::delete_preview(&store, two, path, none).unwrap();
    assert!(preview.last_space);
    assert_eq!(preview.session_count, 1, "Space 2's chats only");
    assert_eq!(preview.hidden_session_count, 3);

    let end = |id: &str| -> Result<(), String> {
        let conn = store.lock_conn()?;
        delete_session(&conn, id, Removal::End)
            .map(drop)
            .map_err(|error| error.to_string())
    };
    let deleted = removal::delete(&store, two, path, none, end).unwrap();
    assert!(deleted.last_space);
    assert_eq!(deleted.deleted_session_ids, ["two-a"]);
    assert_eq!(
        strings(&conn(), "SELECT id FROM sessions"),
        BTreeSet::from(["one-a".to_owned(), "one-b".to_owned(), "one-c".to_owned()]),
        "the delete took chats Space 1 had left hidden"
    );

    assert_eq!(
        project_add::add(&store, one, path, false).map(|(added, _)| added),
        Ok(Added::Linked)
    );
    assert!(
        spaces::project_spaces_in(&conn(), path)
            .unwrap()
            .iter()
            .any(|space| space == one),
        "re-adding brings the project and its chats back"
    );
}

/// T12. Defect: a rename drops a project's own login, or the old path's beats the
/// one the new path already has. Each chat of the walk has no login, so this case
/// runs here rather than as a state of the walk, which it would multiply.
#[test]
fn a_rename_keeps_a_projects_login_and_a_login_the_new_path_has_wins() {
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Space 2", None, 1).unwrap();
    let (from, to) = (PROJECTS[0], "/work/renamed");
    let conn = store.lock_conn().unwrap();
    for account in ["old", "new"] {
        conn.execute(
            "INSERT INTO provider_accounts (provider, id, label, created_at)
             VALUES ('claude', ?1, ?1, 0)",
            [account],
        )
        .unwrap();
    }
    let login = |space: &str, path: &str, account: &str| {
        conn.execute(
            "INSERT INTO space_project_accounts (space_id, project_path, provider, account_id)
             VALUES (?1, ?2, 'claude', ?3)",
            params![space, path, account],
        )
        .unwrap();
    };
    login(SPACES[0], from, "old");
    login(SPACES[1], from, "old");
    login(SPACES[1], to, "new");
    rebase_project(&conn, from, to).unwrap();
    assert_eq!(
        strings(
            &conn,
            "SELECT space_id || ' ' || project_path || ' ' || account_id FROM space_project_accounts"
        ),
        BTreeSet::from([
            format!("{} {to} old", SPACES[0]),
            format!("{} {to} new", SPACES[1]),
        ])
    );
}
