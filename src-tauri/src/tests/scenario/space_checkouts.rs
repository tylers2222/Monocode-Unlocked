//! Checkout isolation (ADE-173). Two Spaces open the same git project. The
//! Space the project is assigned (Space 1 when none is) holds its checkout;
//! the other Space's session starts in its own worktree
//! `<project>-worktrees/<space>/<session>` on branch `<space>/<session>`, and
//! that copy becomes the session's working copy. A turn runs where its child
//! does, and one the app cannot move out of another Space's checkout is
//! refused. Only the owner releases a lease, a held lease never moves, and a
//! restart keeps every lease and copy. Then `harness_spawn` starts two
//! stand-in CLIs, one per Space, in two folders.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::world::World;
use crate::control::tests::{host, turn_folder};
use crate::session_store::{self, SessionStore};
use crate::spaces::leases::{self, CheckoutLease};
use crate::spaces::{prepare_spawn, set_project_space, SpawnTarget, FIRST_SPACE};

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

/// What the window saves of session `id` in `cwd` when it holds no working
/// copy for it (`session_upsert`), and the summary the save replies with. A
/// user message puts the session in its project's session list.
fn save(store: &SessionStore, id: &str, cwd: &Path) -> session_store::SessionSummary {
    let session = serde_json::from_value(json!({
        "id": id,
        "cwd": text(cwd),
        "harness": "codex",
        "model": "codex:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [{"id": "u1", "role": "user", "text": "Start"}],
    }))
    .unwrap();
    session_store::upsert_session(&store.lock_conn().unwrap(), &session).unwrap()
}

/// The working copy and branch a window save of placed session `id` leaves
/// in its reply, in its row and in the project's session list.
fn saved_copy(
    store: &SessionStore,
    id: &str,
    project: &Path,
) -> [(Option<String>, Option<String>); 3] {
    let reply = save(store, id, project);
    let row = store
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT worktree_cwd, branch FROM sessions WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let listed = session_store::list_by_project(&store.lock_conn().unwrap(), &text(project))
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == id)
        .unwrap();
    [
        (reply.worktree_cwd, reply.branch),
        row,
        (listed.worktree_cwd, listed.branch),
    ]
}

fn add_session(store: &SessionStore, id: &str, cwd: &Path, space: &str) {
    save(store, id, cwd);
    store
        .lock_conn()
        .unwrap()
        .execute(
            "UPDATE sessions SET space_id = ?2 WHERE id = ?1",
            [id, space],
        )
        .unwrap();
}

/// Space 2, and session `one` of Space 1 and `two` of Space 2 in `project`.
fn two_spaces(store: &SessionStore, project: &Path) {
    store
        .lock_conn()
        .unwrap()
        .execute(
            "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Space 2', 0)",
            [],
        )
        .unwrap();
    add_session(store, "one", project, FIRST_SPACE);
    add_session(store, "two", project, "space-2");
}

/// The working copy the store records for `session`.
fn work_cwd(store: &SessionStore, session: &str) -> Option<String> {
    store
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT worktree_cwd FROM sessions WHERE id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
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

/// `leases` as `held` lists them: sorted by folder.
fn sorted(mut leases: Vec<(String, String)>) -> Vec<(String, String)> {
    leases.sort();
    leases
}

fn holds(path: &Path, space: &str) -> (String, String) {
    (text(path), space.to_owned())
}

/// The project, and Space 2's session `two`'s copy of it.
struct Rig {
    root: PathBuf,
    project: PathBuf,
    copy: PathBuf,
}

/// Defect: a second Space's session or its numbered child starts, or a turn
/// of it runs, in the checkout the first Space holds; the first session to
/// arrive, not the project's Space, takes an unleased checkout; a held lease
/// moves when the project is reassigned; the copy is not under
/// `<project>-worktrees/<space>/<session>`, not on `<space>/<session>`, not
/// of the project's HEAD, loses the folder inside the checkout, tracked or
/// not, or is not recorded as the session's working copy, before and after a
/// restart; a helper child that is no session takes a lease or a copy; a
/// lease taken through a symlink, `..` or a trailing slash is a second key
/// for one folder; a submodule of a checkout another Space holds is given to
/// the session; a checkout nested in a folder another Space holds is not its
/// own; a folder another Space holds that git cannot copy is not refused; one
/// Space releases another's lease; or a restart, or a deleted copy, moves a
/// session to another folder, loses its commits, or leases anything again.
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
    let mut leased = vec![
        holds(&notes, FIRST_SPACE),
        holds(&rig.project, FIRST_SPACE),
        holds(&rig.copy, "space-2"),
    ];
    leased.extend(the_assigned_space_holds_a_nested_checkout(&world, &rig));
    leased.extend(a_submodule_belongs_to_its_checkout(&world, &rig));
    let leased = sorted(leased);
    assert_eq!(held(world.store()), leased);
    a_restart_and_a_deleted_copy_keep_every_folder(&mut world, &rig, &leased);
    a_held_lease_outlives_a_reassignment(&world, &rig);

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
        spawn_in(world, "two", project),
        Ok(copy.clone()),
        "Space 2's session, first in a project no Space is assigned, took Space 1's checkout"
    );
    assert_eq!(
        work_cwd(world.store(), "two"),
        Some(text(copy)),
        "the copy is not the session's working copy"
    );
    let placed = (Some(text(copy)), Some("space-2/two".to_owned()));
    assert_eq!(
        saved_copy(world.store(), "two", project),
        [placed.clone(), placed.clone(), placed],
        "a window save erased the copy the hook placed the session in, or its branch, in the reply, the row or the session list"
    );
    assert_eq!(
        spawn_in(world, "one", project),
        Ok(project.clone()),
        "Space 1's session left the checkout its Space holds"
    );
    assert!(
        work_cwd(world.store(), "one")
            .unwrap_or_default()
            .is_empty(),
        "a session in its own Space's checkout got another working copy"
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
    let free = git_project(&root.join("free"));
    world
        .conn()
        .execute(
            "UPDATE sessions SET worktree_cwd = ?1 WHERE id = 'one'",
            [text(&free)],
        )
        .unwrap();
    save(world.store(), "one", project);
    assert!(
        work_cwd(world.store(), "one")
            .unwrap_or_default()
            .is_empty(),
        "a window save could not clear a working copy that is no Space's copy"
    );
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
    std::os::unix::fs::symlink(root, root.join("link")).unwrap();
    let linked = format!("{}/link/notes/", text(root));
    leases::take_lease(world.store(), &linked, FIRST_SPACE, world.now()).unwrap();
    assert!(
        held(world.store()).contains(&holds(&notes, FIRST_SPACE)),
        "a lease taken through a symlink is not on the folder"
    );
    let missing = root.join("missing");
    assert_eq!(
        leases::take_lease(world.store(), &text(&missing), FIRST_SPACE, world.now()),
        Err(format!("No such folder: {}", missing.display())),
        "a folder that does not exist was leased by its text"
    );
    let dotted = format!("{}/notes/../notes", text(root));
    assert!(
        leases::take_lease(world.store(), &dotted, "space-2", world.now()).is_err(),
        "a `..` path leased one folder twice"
    );
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

/// A dotfiles repo at `home`, which Space 1 holds, holds a project repo of
/// its own, assigned to Space 2. Returns the leases it adds.
fn the_assigned_space_holds_a_nested_checkout(world: &World, rig: &Rig) -> Vec<(String, String)> {
    let home = rig.root.join("home");
    let nested = git_project(&home);
    std::fs::create_dir_all(home.join("notes")).unwrap();
    git(&home, &["init", "-q"]);
    assert_eq!(
        spawn_in(world, "one", &home.join("notes")),
        Ok(home.join("notes"))
    );
    set_project_space(world.store(), &text(&nested), "space-2").unwrap();
    add_session(world.store(), "one_n", &nested, FIRST_SPACE);
    add_session(world.store(), "two_n", &nested, "space-2");
    assert_eq!(
        spawn_in(world, "two_n", &nested),
        Ok(nested.clone()),
        "a checkout inside a folder Space 1 holds is not a checkout of its own, or its assigned Space lost it"
    );
    let copy = home.join("project-worktrees/space-1/one_n");
    assert_eq!(
        spawn_in(world, "one_n", &nested),
        Ok(copy.clone()),
        "Space 1's session took the checkout of a project assigned to Space 2"
    );
    vec![
        holds(&home, FIRST_SPACE),
        holds(&nested, "space-2"),
        holds(&copy, FIRST_SPACE),
    ]
}

/// `outer`, which Space 1 holds, has the submodule `sub`. Returns the lease
/// it adds.
fn a_submodule_belongs_to_its_checkout(world: &World, rig: &Rig) -> Vec<(String, String)> {
    let lib = git_project(&rig.root.join("lib"));
    let outer = git_project(&rig.root.join("outer"));
    git(
        &outer,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &text(&lib),
            "sub",
        ],
    );
    git(&outer, &["commit", "-qm", "sub"]);
    assert_eq!(spawn_in(world, "one", &outer), Ok(outer.clone()));
    let sub = outer.join("sub");
    assert_eq!(
        spawn_in(world, "two", &sub),
        Err(format!(
            "{} is in space-1's checkout: it is a submodule, which a copy of the checkout does not hold",
            sub.display()
        )),
        "Space 2's session was given a submodule of Space 1's checkout"
    );
    vec![holds(&outer, FIRST_SPACE)]
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
    save(world.store(), "two", project);
    world.down(60_000);
    assert_eq!(
        held(world.store()),
        leased,
        "a restart lost or moved a lease"
    );
    assert_eq!(
        work_cwd(world.store(), "two"),
        Some(text(copy)),
        "a restart lost the session's working copy"
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

/// The project moves to Space 2 while Space 1 holds its checkout.
fn a_held_lease_outlives_a_reassignment(world: &World, Rig { root, project, .. }: &Rig) {
    set_project_space(world.store(), &text(project), "space-2").unwrap();
    add_session(world.store(), "three", project, "space-2");
    add_session(world.store(), "four", project, "space-2");
    assert_eq!(
        spawn_in(world, "three", project),
        Ok(root.join("project-worktrees/space-2/three")),
        "a reassignment moved the lease Space 1 holds"
    );
    assert_eq!(spawn_in(world, "one", project), Ok(project.clone()));
    leases::release_lease(world.store(), &text(project), FIRST_SPACE).unwrap();
    assert_eq!(
        spawn_in(world, "four", project),
        Ok(project.clone()),
        "the released checkout did not go to the project's Space"
    );
    assert_eq!(
        spawn_in(world, "one", project),
        Ok(root.join("project-worktrees/space-1/one"))
    );
}

mod spawn {
    use std::path::Path;

    use tauri::async_runtime::block_on;
    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app, script, wait_for};
    use super::{git_project, saved_copy, temp_root, text, two_spaces};
    use crate::checkpoint;

    type App = tauri::App<tauri::test::MockRuntime>;

    /// As the window begins turn `t2` of `two`: with the folder it stores.
    fn begin_checkpoint(app: &App, project: &Path) {
        block_on(checkpoint::session_checkpoint_begin_turn(
            app.handle().clone(),
            app.state(),
            "two".into(),
            text(project),
            "t2".into(),
            None,
        ))
        .unwrap();
    }

    /// The rest of turn `t2`, still in the folder the window stores, while
    /// the agent edits `src/lib.rs` in `copy`. Returns the review's files,
    /// which also hold the `ran` the stand-in CLI wrote there.
    fn edit_in_copy(app: &App, project: &Path, copy: &Path) -> Vec<String> {
        let paths = || vec!["src/lib.rs".to_owned()];
        block_on(checkpoint::session_checkpoint_prepare(
            app.handle().clone(),
            app.state(),
            "two".into(),
            text(project),
            paths(),
        ))
        .unwrap();
        std::fs::write(copy.join("src/lib.rs"), "two\n").unwrap();
        block_on(checkpoint::session_checkpoint_capture(
            app.handle().clone(),
            app.state(),
            "two".into(),
            text(project),
            paths(),
        ))
        .unwrap();
        block_on(checkpoint::session_checkpoint_finish_turn(
            app.handle().clone(),
            app.state(),
            "two".into(),
            text(project),
            "t2".into(),
        ))
        .unwrap();
        block_on(checkpoint::session_checkpoint_status(
            app.state(),
            "two".into(),
            text(copy),
            None,
        ))
        .unwrap()
        .files
        .into_iter()
        .map(|file| file.relative)
        .collect()
    }

    /// A turn of `one` in `plain`, a folder outside git, whose review keeps
    /// the session's own snapshots: `prepare` takes the file before the edit
    /// and `capture` takes the edit, so the review lists the file, with its
    /// line counts, only when both ran. Returns the review's files.
    fn review_outside_git(app: &App, plain: &Path) -> Vec<(String, i64, i64, bool)> {
        std::fs::create_dir_all(plain).unwrap();
        std::fs::write(plain.join("notes.md"), "one\n").unwrap();
        block_on(checkpoint::session_checkpoint_begin_turn(
            app.handle().clone(),
            app.state(),
            "one".into(),
            text(plain),
            "t3".into(),
            None,
        ))
        .unwrap();
        block_on(checkpoint::session_checkpoint_prepare(
            app.handle().clone(),
            app.state(),
            "one".into(),
            text(plain),
            vec!["notes.md".to_owned()],
        ))
        .unwrap();
        std::fs::write(plain.join("notes.md"), "one\ntwo\n").unwrap();
        block_on(checkpoint::session_checkpoint_capture(
            app.handle().clone(),
            app.state(),
            "one".into(),
            text(plain),
            vec!["notes.md".to_owned()],
        ))
        .unwrap();
        block_on(checkpoint::session_checkpoint_finish_turn(
            app.handle().clone(),
            app.state(),
            "one".into(),
            text(plain),
            "t3".into(),
        ))
        .unwrap();
        block_on(checkpoint::session_checkpoint_status(
            app.state(),
            "one".into(),
            text(plain),
            None,
        ))
        .unwrap()
        .files
        .into_iter()
        .map(|file| (file.relative, file.additions, file.deletions, file.exact))
        .collect()
    }

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
        checkpoint::init(app.handle()).unwrap();
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
        begin_checkpoint(&app, &project);
        let sessions = app.state::<crate::session_store::SessionStore>();
        let placed = (Some(text(&copy)), Some("space-2/two".to_owned()));
        assert_eq!(
            saved_copy(&sessions, "two", &project),
            [placed.clone(), placed.clone(), placed],
            "a window save erased the copy a checkpoint placed the session in, or its branch"
        );
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
        assert!(
            edit_in_copy(&app, &project, &copy).contains(&"src/lib.rs".to_owned()),
            "the first placed turn's checkpoint was taken in Space 1's checkout"
        );
        assert_eq!(
            review_outside_git(&app, &root.join("plain")),
            [("notes.md".to_owned(), 1, 0, true)],
            "a turn's prepare or capture recorded nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
