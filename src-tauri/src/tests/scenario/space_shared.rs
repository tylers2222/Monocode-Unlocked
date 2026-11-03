//! ADE-229: one project, two Spaces. A regular Mono in Space 1 and one in
//! Space 2 both list the same git project, as at the Space 2 cut-over, when
//! every project a Space 2 Mono used was Space 1's too. Through the real
//! `app` CLI parser and level check, each Mono lists the project and may
//! list its sessions; each starts a lead in it, and the spawn hook runs the
//! two leads in different checkouts: Space 1's in the project, Space 2's in
//! its own copy. Space 1 reads as it did before: the project stays assigned
//! to it and no `space_projects` row is written.

use super::agents::{mono_caller, session_caller};
use super::space_checkouts::{git_project, spawn_in, temp_root};
use super::space_scope::scope_of;
use super::world::World;
use crate::mono_roster::MonoRole;
use crate::spaces::scope::SpaceScope;
use crate::spaces::{self, SettingsPatch, FIRST_SPACE};

const TWO: &str = "space-2";

fn active(world: &World, space: &str) {
    let patch = SettingsPatch {
        active_space_id: Some(space.into()),
        ..SettingsPatch::default()
    };
    spaces::set_settings(world.store(), &patch).unwrap();
}

fn space_of(world: &World, table: &str, id: &str) -> String {
    let sql = format!("SELECT space_id FROM {table} WHERE id = ?1");
    world
        .conn()
        .query_row(&sql, [id], |row| row.get(0))
        .unwrap()
}

/// The projects `monos.list` shows for Mono `id`.
fn listed_projects(scope: &SpaceScope, id: &str) -> Option<Vec<String>> {
    let mono = scope.monos.iter().find(|mono| mono.mono_id.as_str() == id);
    mono.map(|mono| mono.projects.clone())
}

/// The sessions `sessions.list` may list.
fn ours(scope: &SpaceScope) -> Vec<&str> {
    scope.sessions.iter().map(|id| id.as_str()).collect()
}

/// Defect: a project is in one Space only, so a Mono of a second Space that
/// lists it gets no projects in `monos.list` and a refused `sessions.list`
/// (ADE-229); a Space's session in the shared project is refused its own
/// folder; the two Spaces' leads run in one checkout; or sharing the project
/// moves it out of Space 1 (its assignment, its lease, a `space_projects`
/// row, or what Space 1's Mono lists).
#[test]
fn two_spaces_share_one_project() {
    let world = World::new("space-shared");
    let root = temp_root("ade229");
    let project = git_project(&root);
    let path = project.to_string_lossy().into_owned();
    let ours = vec![path.clone()];
    world
        .conn()
        .execute(
            "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 2', 1)",
            [TWO],
        )
        .unwrap();
    let one = world.add_mono_in("a-mono", MonoRole::Mono, &[&path]);
    active(&world, TWO);
    let two = world.add_mono_in("b-mono", MonoRole::Mono, &[&path]);
    active(&world, FIRST_SPACE);
    assert_eq!(space_of(&world, "monos", "a-mono"), FIRST_SPACE);
    assert_eq!(space_of(&world, "monos", "b-mono"), TWO);

    // Both Monos see the project.
    let monos_list = ["monos.list"];
    let from_two = scope_of(&world, &mono_caller(&two, "t1"), &monos_list);
    assert_eq!(from_two.space, TWO);
    assert_eq!(
        listed_projects(&from_two, "b-mono"),
        Some(ours.clone()),
        "Space 2's Mono lost the project Space 1 shares"
    );
    let from_one = scope_of(&world, &mono_caller(&one, "t1"), &monos_list);
    assert_eq!(
        listed_projects(&from_one, "a-mono"),
        Some(ours.clone()),
        "Space 1's Mono lost its project"
    );

    // Both start a lead in it. A lead's id names its Mono's chat, as the
    // app's do (`app-<chat>-<request>`), so it joins its Mono's Space.
    let id1 = format!("app-{}-r1", one.chat.as_str());
    let id2 = format!("app-{}-r1", two.chat.as_str());
    let lead1 = world.start_lead(&one, &id1, &path);
    let lead2 = world.start_lead(&two, &id2, &path);
    assert_eq!(space_of(&world, "sessions", &id1), FIRST_SPACE);
    assert_eq!(space_of(&world, "sessions", &id2), TWO);

    // Both may list the project's sessions, each only its own Space's.
    let sessions_list = [
        "sessions.list",
        "--json",
        &format!(r#"{{"project":"{path}"}}"#),
    ];
    let two_lists = scope_of(&world, &mono_caller(&two, "t2"), &sessions_list);
    assert_eq!(
        two_lists.projects, ours,
        "sessions.list refuses Space 2's Mono"
    );
    assert_eq!(ours(&two_lists), [id2.as_str()], "{two_lists:?}");
    let one_lists = scope_of(&world, &mono_caller(&one, "t2"), &sessions_list);
    assert_eq!(
        one_lists.projects, ours,
        "sessions.list refuses Space 1's Mono"
    );
    assert_eq!(ours(&one_lists), [id1.as_str()], "{one_lists:?}");
    let lead2_lists = scope_of(&world, &session_caller(&lead2, &path), &sessions_list);
    assert_eq!(
        (lead2_lists.space.as_str(), lead2_lists.projects),
        (TWO, ours.clone()),
        "Space 2's lead is refused its own folder"
    );

    // The two leads run in different checkouts.
    let first = spawn_in(&world, lead1.as_str(), &project).unwrap();
    let second = spawn_in(&world, lead2.as_str(), &project).unwrap();
    assert_eq!(first, project, "Space 1's lead left the project's checkout");
    assert_eq!(
        second,
        root.join("project-worktrees").join(TWO).join(&id2),
        "Space 2's lead did not get its own copy"
    );

    // Space 1 is as it was: the project stays assigned to it and leased by it.
    assert_eq!(
        spaces::project_space(world.store(), &path).unwrap(),
        FIRST_SPACE
    );
    let rows: i64 = world
        .conn()
        .query_row("SELECT count(*) FROM space_projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0, "sharing the project wrote a space_projects row");
    let leases: Vec<_> = spaces::leases::leases(world.store())
        .unwrap()
        .into_iter()
        .map(|lease| (lease.checkout_path, lease.space_id))
        .collect();
    assert!(
        leases.contains(&(path.clone(), FIRST_SPACE.to_owned())),
        "{leases:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
