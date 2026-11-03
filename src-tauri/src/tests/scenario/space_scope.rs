//! ADE-175: `monos.list`, `sessions.list` and `messages.get_monos` answer for
//! the caller's Space only; ADE-194: so do `monos.get` and `monos.read`, and
//! `sessions.list` names no other Space's session. Each call is parsed by the
//! real `app` CLI parser and checked by the real `mono_app_action_check` path
//! (`check_action`) on saved rows: Space 1 holds a Giga, a regular Mono, a
//! project session, a lead and its worker; Space 2 ("Work") holds the same
//! plus a remote project. Then a one-Space world lists what it listed before
//! Spaces.
//! Every checkpoint prints `ok` or `FAIL`, so one run shows every rule a
//! change broke.

use std::fmt::Debug;

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::control_cli;
use crate::mono_gate::{Caller, CallerKind, Speaker};
use crate::mono_messages::{self, ActionChecked, MonoId, RequestId, SessionId};
use crate::mono_roster::MonoRole;
use crate::spaces::scope::SpaceScope;
use crate::spaces::{self, FIRST_SPACE};

const WORK: &str = "space-2";
const REMOTE: &str = "remote://host/path";

/// `caller`'s `app <args>`, as the CLI sends it and the level check answers.
fn call(world: &World, caller: &Caller, args: &[&str]) -> Result<Option<SpaceScope>, String> {
    let (action, input, request_id) = control_cli::app_call(args)?;
    let checked = mono_messages::check_action(
        &world.conn(),
        caller,
        &action,
        None,
        None,
        &RequestId::parse(&request_id).unwrap(),
        &input,
        world.now(),
    );
    match checked.unwrap() {
        ActionChecked::Allowed { scope, .. } => Ok(scope),
        ActionChecked::Failed { error } => Err(error.code.into()),
    }
}

pub(super) fn scope_of(world: &World, caller: &Caller, args: &[&str]) -> SpaceScope {
    call(world, caller, args)
        .unwrap_or_else(|code| panic!("{args:?}: {code}"))
        .unwrap_or_else(|| panic!("{args:?}: no scope"))
}

/// Each listed Mono's id and its other Space's name.
fn listed(scope: &SpaceScope) -> Vec<(String, Option<String>)> {
    scope
        .monos
        .iter()
        .map(|mono| (mono.mono_id.as_str().to_owned(), mono.other_space.clone()))
        .collect()
}

fn same(ids: &[&str]) -> Vec<(String, Option<String>)> {
    ids.iter().map(|id| ((*id).to_owned(), None)).collect()
}

/// What `sessions.list` refuses in the web app: a project that is not one of
/// the caller's in its Space.
fn refused(scope: &SpaceScope, project: &str) -> bool {
    !scope.projects.iter().any(|path| path == project)
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// The sessions `sessions.list` may list.
fn ours(scope: &SpaceScope) -> Vec<String> {
    sorted(
        scope
            .sessions
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
    )
}

fn checkpoint<T: PartialEq + Debug>(failed: &mut Vec<String>, name: &str, got: T, want: T) {
    if got == want {
        println!("ok   {name}: {got:?}");
    } else {
        println!("FAIL {name}: {got:?}, expected {want:?}");
        failed.push(name.to_owned());
    }
}

fn habit_run(mono: &Mono, run: &str) -> Caller {
    Caller {
        speaker: Speaker::Mono {
            kind: CallerKind::HabitRun,
            mono_id: mono.id.clone(),
        },
        session_id: SessionId::parse(run).unwrap(),
        turn_id: "t1".into(),
    }
}

fn move_to_work(world: &World, table: &str, id: &str) {
    let sql = format!("UPDATE {table} SET space_id = ?1 WHERE id = ?2");
    assert_eq!(
        world.conn().execute(&sql, [WORK, id]).unwrap(),
        1,
        "{table} {id}"
    );
}

const MONOS_LIST: &[&str] = &["monos.list"];
/// The Space 2 Mono's projects: /p1 and `remote://host/other` are assigned
/// to Space 1, /p2 and `remote://host/path` to Space 2.
const D_MONO_PROJECTS: [&str; 4] = ["/p1", "/p2", "remote://host/path/", "remote://host/other"];
const GET_MONOS: &[&str] = &["messages.get_monos"];

/// Space 1: a Giga, a regular Mono, a project session, a lead and its
/// worker. Space 2: a Giga, a regular Mono, a project session and a lead in
/// a remote project.
struct Cast {
    world: World,
    giga1: Mono,
    mono1: Mono,
    giga2: Mono,
    mono2: Mono,
    lead1: SessionId,
    worker1: SessionId,
    lead2: SessionId,
}

impl Cast {
    fn new() -> Self {
        let world = World::new("space-scope");
        let sql = "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Work', 1)";
        world.conn().execute(sql, [WORK]).unwrap();
        // Roster order is creation then id, so the ids name it.
        let giga1 = world.add_mono("a-giga", MonoRole::Giga);
        let mono1 = world.add_mono_in("b-mono", MonoRole::Mono, &["/p1"]);
        let giga2 = world.add_mono("c-giga", MonoRole::Giga);
        let mono2 = world.add_mono_in("d-mono", MonoRole::Mono, &D_MONO_PROJECTS);
        world.add_session("s1", "/p1", None);
        world.add_session("s2", "/p2", None);
        let lead1 = world.start_lead(&mono1, "lead1", "/p1");
        let worker1 = world.add_session("worker1", "/p1", Some("lead1"));
        let lead2 = world.start_lead(&mono2, "lead2", REMOTE);
        for mono in [&giga2, &mono2] {
            move_to_work(&world, "monos", mono.id.as_str());
            move_to_work(&world, "sessions", mono.chat.as_str());
        }
        for session in ["s2", "lead2"] {
            move_to_work(&world, "sessions", session);
        }
        spaces::set_project_space(world.store(), "/p2", WORK).unwrap();
        spaces::set_project_space(world.store(), REMOTE, WORK).unwrap();
        Self {
            world,
            giga1,
            mono1,
            giga2,
            mono2,
            lead1,
            worker1,
            lead2,
        }
    }

    fn scope(&self, caller: &Caller, args: &[&str]) -> SpaceScope {
        scope_of(&self.world, caller, args)
    }

    /// a. A Space 2 Mono lists only Space 2's Monos, sessions and projects.
    fn space_two_mono(&self, failed: &mut Vec<String>) {
        let caller = mono_caller(&self.mono2, "t1");
        let listing = self.scope(&caller, MONOS_LIST);
        let same_space: Vec<_> = listing
            .monos
            .iter()
            .filter(|mono| mono.other_space.is_none())
            .map(|mono| mono.mono_id.as_str())
            .collect();
        let want = vec!["c-giga", "d-mono"];
        checkpoint(failed, "a monos.list: only Space 2 Monos", same_space, want);
        let projects_of = |scope: &SpaceScope, id: &str| {
            let mono = scope.monos.iter().find(|mono| mono.mono_id.as_str() == id);
            mono.map(|mono| mono.projects.clone())
        };
        checkpoint(
            failed,
            "a monos.list shows every project of d-mono, its own Space's Mono (ADE-229)",
            projects_of(&listing, "d-mono"),
            Some(D_MONO_PROJECTS.map(str::to_owned).to_vec()),
        );
        let from_space1 = self.scope(&mono_caller(&self.mono1, "t1"), MONOS_LIST);
        checkpoint(
            failed,
            "a a Space 1 Mono sees d-mono's Space 1 projects only",
            projects_of(&from_space1, "d-mono"),
            Some(vec!["/p1".to_owned(), "remote://host/other".into()]),
        );
        let p2 = self.scope(
            &caller,
            &["sessions.list", "--json", r#"{"project":"/p2"}"#],
        );
        checkpoint(
            failed,
            "a sessions.list names only Space 2's sessions in its projects, no Space 1 id",
            ours(&p2),
            vec!["lead2".to_owned(), "s2".into()],
        );
        let name = "a sessions.list takes /p1, a project Space 1 shares (ADE-229)";
        checkpoint(failed, name, refused(&p2, "/p1"), false);
        let lead1_in_p2 = self.scope(
            &session_caller(&self.lead1, "/p2"),
            &["sessions.list", "--json", r#"{"project":"/p2"}"#],
        );
        let name = "a a Space 1 session in a project only Space 2 has gets no project";
        checkpoint(failed, name, refused(&lead1_in_p2, "/p2"), true);
        let name = "a sessions.list takes a Space 2 project";
        checkpoint(failed, name, refused(&p2, "/p2"), false);
        checkpoint(
            failed,
            "a the caller's Space is Space 2",
            p2.space.as_str(),
            WORK,
        );
    }

    /// b. A regular Mono sees other Spaces' regular Monos, marked.
    fn regular_monos_across(&self, failed: &mut Vec<String>) {
        let mut want = same(&["a-giga", "b-mono"]);
        want.push(("d-mono".into(), Some("Work".into())));
        checkpoint(
            failed,
            "b a Space 1 Mono sees Space 2's Mono marked, never its Giga",
            listed(&self.scope(&mono_caller(&self.mono1, "t1"), GET_MONOS)),
            want,
        );
        let mut want = vec![("b-mono".to_owned(), Some("Space 1".to_owned()))];
        want.extend(same(&["c-giga", "d-mono"]));
        checkpoint(
            failed,
            "b a Space 2 Mono sees Space 1's Mono marked, never its Giga",
            listed(&self.scope(&mono_caller(&self.mono2, "t1"), GET_MONOS)),
            want,
        );
    }

    /// c. A Giga and a lead or worker session get no other Space's Monos.
    fn no_peers_across(&self, failed: &mut Vec<String>) {
        checkpoint(
            failed,
            "c a Space 2 Giga gets only Space 2 Monos",
            listed(&self.scope(&mono_caller(&self.giga2, "t1"), GET_MONOS)),
            same(&["c-giga", "d-mono"]),
        );
        for (who, caller) in [
            ("a Space 1 Giga", mono_caller(&self.giga1, "t1")),
            ("a lead", session_caller(&self.lead1, "/p1")),
            ("a worker", session_caller(&self.worker1, "/p1")),
        ] {
            let name = format!("c {who} gets only Space 1 Monos");
            let got = listed(&self.scope(&caller, GET_MONOS));
            checkpoint(failed, &name, got, same(&["a-giga", "b-mono"]));
        }
    }

    /// d. A habit run answers for its Mono's Space, whatever its session row.
    fn habit_runs(&self, failed: &mut Vec<String>) {
        self.world.add_session("d-run-saved", "/", None);
        for run in ["d-run-unsaved", "d-run-saved"] {
            let habit = habit_run(&self.mono2, run);
            let space = self.scope(&habit, MONOS_LIST).space;
            let name = format!("d habit run {run} answers for Space 2");
            checkpoint(failed, &name, space.as_str(), WORK);
            let projects = self.scope(&habit, &["sessions.list"]).projects;
            let name = format!("d habit run {run} has every project of its Mono");
            let want = D_MONO_PROJECTS.map(str::to_owned).to_vec();
            checkpoint(failed, &name, projects, want);
        }
    }

    /// e. A remote key moved to Space 2, with and without its trailing slash.
    fn remote_projects(&self, failed: &mut Vec<String>) {
        let mono = self.scope(&mono_caller(&self.mono2, "t1"), &["sessions.list"]);
        checkpoint(
            failed,
            "e a Mono's remote project with a trailing slash is in Space 2",
            refused(&mono, "remote://host/path/"),
            false,
        );
        checkpoint(
            failed,
            "e a Mono's remote project assigned to Space 1 is still its own (ADE-229)",
            refused(&mono, "remote://host/other"),
            false,
        );
        let lead = self.scope(&session_caller(&self.lead2, REMOTE), &["sessions.list"]);
        checkpoint(
            failed,
            "e a lead in the remote project lists it",
            (lead.space.as_str(), lead.projects),
            (WORK, vec![REMOTE.to_owned()]),
        );
        checkpoint(
            failed,
            "e the remote key reads as project_space reads it",
            spaces::project_space(self.world.store(), "remote://host/path/").unwrap(),
            WORK.to_owned(),
        );
    }

    /// g. `monos.get` and `monos.read` answer only for a Mono the caller's
    /// Space lists (ADE-194).
    fn reads(&self, failed: &mut Vec<String>) {
        let mono1 = mono_caller(&self.mono1, "t1");
        for action in ["monos.get", "monos.read"] {
            for (who, caller, id, want) in [
                (
                    "a Space 1 Mono",
                    &mono1,
                    "d-mono",
                    "found, other_space Work",
                ),
                ("a Space 1 Mono", &mono1, "c-giga", "unknown_mono"),
                ("a Space 1 Mono", &mono1, "z-none", "unknown_mono"),
                (
                    "a Space 1 Giga",
                    &mono_caller(&self.giga1, "t1"),
                    "d-mono",
                    "unknown_mono",
                ),
                (
                    "a Space 2 Giga",
                    &mono_caller(&self.giga2, "t1"),
                    "d-mono",
                    "found",
                ),
                (
                    "an unsaved Space 2 habit run",
                    &habit_run(&self.mono2, "g-run"),
                    "b-mono",
                    "found, other_space Space 1",
                ),
            ] {
                let name = format!("g {action}: {who} reading {id}");
                checkpoint(
                    failed,
                    &name,
                    answer(&self.world, caller, action, id),
                    want.into(),
                );
            }
        }
    }

    /// h. Each Space's sessions in a project both share: `sessions.list`
    /// names only the caller's.
    fn shared_sessions(&self, failed: &mut Vec<String>) {
        self.world.add_session("h-two", "/p1", None);
        move_to_work(&self.world, "sessions", "h-two");
        let p1 = ["sessions.list", "--json", r#"{"project":"/p1"}"#];
        let space1 = self.scope(&mono_caller(&self.mono1, "t1"), &p1);
        let want = sorted(vec!["lead1".into(), "s1".into(), "worker1".into()]);
        checkpoint(
            failed,
            "h a Space 1 Mono gets no Space 2 id in /p1",
            ours(&space1),
            want,
        );
        let space2 = self.scope(&mono_caller(&self.mono2, "t1"), &p1);
        let want = vec!["h-two".to_owned(), "lead2".into(), "s2".into()];
        checkpoint(
            failed,
            "h a Space 2 Mono gets no Space 1 id in /p1",
            ours(&space2),
            want,
        );
        let lead = self.scope(&session_caller(&self.lead1, "/p1"), &p1);
        let want = sorted(vec!["lead1".into(), "s1".into(), "worker1".into()]);
        checkpoint(
            failed,
            "h a Space 1 lead gets only Space 1's /p1 sessions",
            ours(&lead),
            want,
        );
    }
}

/// What the web app answers for `monos.get` or `monos.read` of `id` from the
/// scope the level check gives: the Mono, marked when it is of another Space,
/// or the unknown-ID error.
fn answer(world: &World, caller: &Caller, action: &str, id: &str) -> String {
    let input = format!(r#"{{"monoId":"{id}"}}"#);
    match call(world, caller, &[action, "--json", &input]) {
        Err(code) => format!("refused {code}"),
        Ok(None) => "no scope".into(),
        Ok(Some(scope)) => match scope.monos.iter().find(|mono| mono.mono_id.as_str() == id) {
            None => "unknown_mono".into(),
            Some(mono) => match &mono.other_space {
                None => "found".into(),
                Some(space) => format!("found, other_space {space}"),
            },
        },
    }
}

/// f. One Space: every list as it was before Spaces.
fn one_space(failed: &mut Vec<String>) {
    let one = World::new("space-scope-one");
    let giga = one.add_mono("a-giga", MonoRole::Giga);
    let mono = one.add_mono_in("b-mono", MonoRole::Mono, &["/p1", REMOTE]);
    let lead = one.start_lead(&mono, "lead", "/p1");
    let worker = one.add_session("worker", "/p1", Some("lead"));
    one.add_session("s1", "/p1", None);
    let every = same(&["a-giga", "b-mono"]);
    let mono_c = mono_caller(&mono, "t1");
    let listing = scope_of(&one, &mono_c, MONOS_LIST);
    checkpoint(
        failed,
        "f monos.list lists every Mono",
        listed(&listing),
        every.clone(),
    );
    let projects: Vec<_> = listing
        .monos
        .into_iter()
        .map(|entry| entry.projects)
        .collect();
    checkpoint(
        failed,
        "f monos.list keeps every project",
        projects,
        vec![vec![], vec!["/p1".to_owned(), REMOTE.to_owned()]],
    );
    let list = scope_of(
        &one,
        &mono_c,
        &["sessions.list", "--json", r#"{"project":"/p1"}"#],
    );
    checkpoint(
        failed,
        "f sessions.list names every session in the Mono's projects and keeps every project",
        (ours(&list), list.projects),
        (
            sorted(vec!["lead".into(), "s1".into(), "worker".into()]),
            vec!["/p1".to_owned(), REMOTE.to_owned()],
        ),
    );
    let gone = Caller {
        speaker: Speaker::Mono {
            kind: CallerKind::MonoChat,
            mono_id: MonoId::parse("z-gone").unwrap(),
        },
        ..mono_c.clone()
    };
    let unsaved = SessionId::parse("unsaved").unwrap();
    for (who, caller) in [
        ("a Giga", mono_caller(&giga, "t1")),
        ("a Mono", mono_c.clone()),
        ("a habit run", habit_run(&mono, "b-run")),
        ("a lead", session_caller(&lead, "/p1")),
        ("a worker", session_caller(&worker, "/p1")),
        ("an unsaved session", session_caller(&unsaved, "/p1")),
        ("a gone Mono's chat", gone),
    ] {
        let name = format!("f get_monos for {who} lists every Mono in Space 1");
        let got = call(&one, &caller, GET_MONOS);
        let got = got.map(|scope| scope.as_ref().map(listed));
        checkpoint(failed, &name, got, Ok(Some(every.clone())));
    }
    checkpoint(
        failed,
        "f an action that lists no Space gets no scope",
        call(&one, &mono_c, &["models.list"]),
        Ok(None),
    );
    checkpoint(
        failed,
        "f the Space is Space 1",
        list.space.as_str(),
        FIRST_SPACE,
    );
}

/// Defect: a list action answers with another Space's Monos, projects or
/// sessions, or hides a Mono's own project assigned to another Space
/// (ADE-229); the caller's Space comes from anything but its saved rows (a
/// habit run reads its session); a Giga or a session sees other Spaces'
/// Monos, or a regular Mono sees another Space's Giga or no Space name; a
/// remote key reaches the Space rule in another form than `project_space`
/// reads; or a one-Space world lists anything other than it did before.
#[test]
fn list_actions_answer_for_the_callers_space() {
    let cast = Cast::new();
    let mut failed = Vec::new();
    cast.space_two_mono(&mut failed);
    cast.regular_monos_across(&mut failed);
    cast.no_peers_across(&mut failed);
    cast.habit_runs(&mut failed);
    cast.remote_projects(&mut failed);
    one_space(&mut failed);
    assert!(failed.is_empty(), "failed checkpoints: {failed:?}");
}

/// Defect (ADE-194): `monos.get` or `monos.read` answers for another Space's
/// Giga, for any other Space's Mono to a Giga, or unmarked; tells an
/// out-of-Space Mono from an unknown ID; or `sessions.list` names a session
/// of another Space in a project both Spaces share.
#[test]
fn reads_answer_for_the_callers_space() {
    let cast = Cast::new();
    let mut failed = Vec::new();
    cast.reads(&mut failed);
    cast.shared_sessions(&mut failed);
    assert!(failed.is_empty(), "failed checkpoints: {failed:?}");
}
