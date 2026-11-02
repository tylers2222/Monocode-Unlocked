//! Checkout isolation (ADE-173). Two Spaces open the same git project. The
//! Space whose session starts there first holds it; the other Space's session
//! starts in its own worktree `<project>-worktrees/<space>/<session>` on
//! branch `<space>/<session>`. A turn runs where its child does, and one the
//! app cannot move out of another Space's checkout is refused. Only the
//! owner releases a lease, and a restart keeps every lease and copy. Then
//! `harness_spawn` starts two stand-in CLIs, one per Space, in two folders.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::world::World;
use crate::control::tests::{host, turn_folder};
use crate::session_store::{self, SessionStore};
use crate::spaces::leases::{self, CheckoutLease};
use crate::spaces::{prepare_spawn, SpawnTarget, FIRST_SPACE};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `root/project`, a git project with `src/` in its one commit. `root` is
/// canonical: macOS `/tmp` is `/private/tmp`.
fn git_project(root: &Path) -> PathBuf {
    let project = root.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "").unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "initial"]);
    project
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(&root).unwrap()
}

/// Space 2, and session `one` of Space 1 and `two` of Space 2 in `project`.
fn two_spaces(store: &SessionStore, project: &Path) {
    let conn = store.lock_conn().unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Space 2', 0)",
        [],
    )
    .unwrap();
    for (id, space) in [("one", FIRST_SPACE), ("two", "space-2")] {
        let session = serde_json::from_value(json!({
            "id": id,
            "cwd": text(project),
            "harness": "codex",
            "model": "codex:default",
            "modelSettings": {},
            "runtimeMode": "auto",
            "title": id,
            "blocks": [],
        }))
        .unwrap();
        session_store::upsert_session(&conn, &session).unwrap();
        conn.execute(
            "UPDATE sessions SET space_id = ?2 WHERE id = ?1",
            [id, space],
        )
        .unwrap();
    }
}

/// The folder the spawn hook starts `session`'s child in.
fn spawn_in(world: &World, session: &str, workdir: &Path) -> Result<PathBuf, String> {
    let target = SpawnTarget {
        workdir: workdir.to_path_buf(),
        account: None,
    };
    prepare_spawn(world.store(), session, None, world.now(), target).map(|t| t.workdir)
}

fn held(store: &SessionStore) -> Vec<(String, String)> {
    leases::leases(store)
        .unwrap()
        .into_iter()
        .map(
            |CheckoutLease {
                 checkout_path,
                 space_id,
                 ..
             }| (checkout_path, space_id),
        )
        .collect()
}

fn holds(path: &Path, space: &str) -> (String, String) {
    (text(path), space.to_owned())
}

/// Defect: a second Space's session or its numbered child starts, or a turn
/// of it runs, in the checkout the first Space holds; its copy is not under
/// `<project>-worktrees/<space>/<session>`, not on `<space>/<session>`, not
/// of the project's HEAD, or loses the folder inside the checkout, tracked
/// or not; a checkout no Space holds is not leased to the first; a helper
/// child that is no session takes a lease or a copy; a checkout nested in a
/// folder another Space holds is not its own; a folder another Space holds
/// that git cannot copy is not refused; one Space releases another's lease;
/// or a restart, or a deleted copy, moves a session to another folder, loses
/// its commits, or leases anything again.
/// The project, and Space 2's session `two`'s copy of it.
struct Rig {
    root: PathBuf,
    project: PathBuf,
    copy: PathBuf,
}

#[test]
fn two_spaces_in_one_project_work_in_two_folders() {
    let mut world = World::new("space-checkouts");
    let root = temp_root("ade173");
    let project = git_project(&root);
    two_spaces(world.store(), &project);
    let rig = Rig {
        copy: root.join("project-worktrees/space-2/two"),
        root,
        project,
    };
    two_sessions_in_two_folders(&world, &rig);
    let notes = turns_follow_and_foreign_folders_are_refused(&world, &rig);
    let (home, nested) = a_nested_checkout_is_its_own(&world, &rig);
    let leased = vec![
        holds(&home, FIRST_SPACE),
        holds(&nested, "space-2"),
        holds(&notes, FIRST_SPACE),
        holds(&rig.project, FIRST_SPACE),
        holds(&rig.copy, "space-2"),
    ];
    assert_eq!(held(world.store()), leased);
    a_restart_and_a_deleted_copy_keep_every_folder(&mut world, &rig, &leased);

    spawn::two_spaces_start_two_children_in_two_folders();
    let _ = std::fs::remove_dir_all(&rig.root);
}

fn two_sessions_in_two_folders(
    world: &World,
    Rig {
        root,
        project,
        copy,
    }: &Rig,
) {
    assert_eq!(
        spawn_in(world, "one", project),
        Ok(project.clone()),
        "Space 1's session left the project no Space held"
    );
    assert_eq!(
        spawn_in(world, "two", project),
        Ok(copy.clone()),
        "Space 2's session started in Space 1's checkout"
    );
    assert_eq!(git(copy, &["branch", "--show-current"]), "space-2/two");
    assert_eq!(
        git(copy, &["rev-parse", "HEAD"]),
        git(project, &["rev-parse", "HEAD"]),
        "the copy is not of the project's HEAD"
    );
    assert_eq!(
        spawn_in(world, "two", &project.join("src")),
        Ok(copy.join("src")),
        "a folder inside the checkout lost its place in the copy"
    );
    let build = project.join("build");
    std::fs::create_dir_all(&build).unwrap();
    assert_eq!(
        spawn_in(world, "two", &build),
        Ok(copy.join("build")),
        "an untracked folder has no place in the copy"
    );
    assert!(copy.join("build").is_dir());
    assert_eq!(spawn_in(world, "two", copy), Ok(copy.clone()));
    assert_eq!(
        spawn_in(world, "two#1", project),
        Ok(copy.clone()),
        "a session's numbered child left its copy"
    );
    assert_eq!(spawn_in(world, "one", project), Ok(project.clone()));
    let free = git_project(&root.join("free"));
    assert_eq!(
        spawn_in(world, "monocode-claude-text", project),
        Ok(project.clone())
    );
    assert_eq!(
        spawn_in(world, "monocode-claude-text", &free),
        Ok(free.clone())
    );
    assert_eq!(
        held(world.store()),
        [holds(project, FIRST_SPACE), holds(copy, "space-2")],
        "a helper that is no session took a lease or a copy"
    );
}

/// Returns `notes`, a folder Space 1 holds that git cannot copy.
fn turns_follow_and_foreign_folders_are_refused(
    world: &World,
    Rig {
        root,
        project,
        copy,
    }: &Rig,
) -> PathBuf {
    let control = host();
    assert_eq!(
        turn_folder(&control, world.store(), "two", project),
        Ok(text(copy)),
        "Space 2's turn runs in Space 1's checkout"
    );
    assert_eq!(
        turn_folder(&control, world.store(), "one", project),
        Ok(text(project))
    );
    let notes = root.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    leases::take_lease(world.store(), &text(&notes), FIRST_SPACE, world.now()).unwrap();
    let refusal = format!(
        "{} is in space-1's checkout: it is not a git checkout, so it cannot be copied",
        notes.display()
    );
    assert_eq!(
        turn_folder(&control, world.store(), "two", &notes),
        Err(refusal.clone()),
        "Space 2's turn ran in a folder Space 1 holds"
    );
    assert_eq!(spawn_in(world, "two", &notes), Err(refusal));
    assert!(
        leases::release_lease(world.store(), &text(project), "space-2").is_err(),
        "Space 2 released Space 1's lease"
    );
    notes
}

/// A dotfiles repo at `home` holds a project repo of its own. Returns both.
fn a_nested_checkout_is_its_own(world: &World, rig: &Rig) -> (PathBuf, PathBuf) {
    let home = rig.root.join("home");
    let nested = git_project(&home);
    std::fs::create_dir_all(home.join("notes")).unwrap();
    git(&home, &["init", "-q"]);
    assert_eq!(
        spawn_in(world, "one", &home.join("notes")),
        Ok(home.join("notes"))
    );
    assert_eq!(
        spawn_in(world, "two", &nested),
        Ok(nested.clone()),
        "a checkout inside a folder Space 1 holds is not a checkout of its own"
    );
    (home, nested)
}

fn a_restart_and_a_deleted_copy_keep_every_folder(
    world: &mut World,
    Rig { project, copy, .. }: &Rig,
    leased: &[(String, String)],
) {
    std::fs::write(copy.join("two.txt"), "").unwrap();
    git(copy, &["add", "two.txt"]);
    git(copy, &["commit", "-qm", "two"]);
    let work = git(copy, &["rev-parse", "HEAD"]);
    world.down(60_000);
    assert_eq!(
        held(world.store()),
        leased,
        "a restart lost or moved a lease"
    );
    assert_eq!(spawn_in(world, "one", project), Ok(project.clone()));
    assert_eq!(
        spawn_in(world, "two", project),
        Ok(copy.clone()),
        "after a restart Space 2's session moved"
    );
    std::fs::remove_dir_all(copy).unwrap();
    assert_eq!(
        spawn_in(world, "two", project),
        Ok(copy.clone()),
        "a deleted copy was not made again"
    );
    assert_eq!(
        git(copy, &["rev-parse", "HEAD"]),
        work,
        "the copy made again lost its branch's commits"
    );
    assert_eq!(held(world.store()), leased);
}

mod spawn {
    use std::path::Path;

    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app, script, wait_for};
    use super::{git_project, temp_root, text, two_spaces};

    /// As the window runs a turn of each Space's codex session
    /// (`sendHarnessTurn`): `control_authorize_turn` names the folder, and the
    /// provider is told it and spawned in it. `harness_spawn` runs a stand-in
    /// fx that writes the folder it runs in.
    pub(super) fn two_spaces_start_two_children_in_two_folders() {
        let root = temp_root("ade173-spawn");
        let project = git_project(&root);
        let fx = script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) pwd -P > ran.part && mv ran.part ran ;;
esac"#,
        );
        let data = root.join("app-data");
        std::fs::create_dir_all(&data).unwrap();
        let app = mock_app(&data);
        app.manage(crate::control::tests::host());
        two_spaces(&app.state::<crate::session_store::SessionStore>(), &project);
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let authorize = |session: &str| {
            crate::control::control_authorize_turn(
                window.clone(),
                app.state::<crate::control::ControlHost>(),
                session.into(),
                text(&project),
                false,
                format!("turn-{session}"),
            )
        };
        let spawn = |session: &str, workdir: &Path| {
            crate::harness::harness_spawn(
                app.handle().clone(),
                app.state::<crate::harness::HarnessHost>(),
                session.into(),
                text(&fx),
                vec!["acp".into()],
                text(workdir),
                None,
                Some("fx".into()),
                Some(text(&fx)),
                None,
                None,
                None,
            )
        };

        let copy = root.join("project-worktrees/space-2/two");
        let one = authorize("one").unwrap();
        let two = authorize("two").unwrap();
        assert_eq!(one, text(&project));
        assert_eq!(
            two,
            text(&copy),
            "Space 2's codex turn was told Space 1's checkout"
        );
        spawn("one", Path::new(&one)).unwrap();
        spawn("two", Path::new(&two)).unwrap();
        assert_eq!(
            wait_for(&project.join("ran")),
            format!("{}\n", text(&project))
        );
        assert_eq!(
            wait_for(&copy.join("ran")),
            format!("{}\n", text(&copy)),
            "two Spaces' children started in one folder"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
