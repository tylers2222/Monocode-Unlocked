//! Sessions inherit their Space's login (ADE-172). Every new session row
//! records its Space at insert: a child its parent's (the lead that started
//! it, the Mono whose chat it is, the Mono chat that started a lead or a
//! habit run), a session or Mono the user starts the active Space, and never
//! a Space its caller names. Then `harness_spawn` on Tauri's mock runtime, on the
//! same database, starts a stand-in CLI with the session's own login, else
//! its Space's, else the CLI's own in Space 1, and refuses a Space with no
//! login for that CLI. A session with no row yet is in the Space it would
//! join. With `MONOCODE_SCENARIO_DB` it runs on a copy of a
//! real database.

use rusqlite::{params, Connection};
use serde_json::json;

use super::world::World;
use crate::mono_roster::MonoRole;
use crate::session_store::{self, SessionStore, SessionUpsert};
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::{self, ProjectLogin, Provider, FIRST_SPACE};

const P: &str = "/p";
const P2: &str = "/p2";
const SPACE_2: &str = "space-2";
const HABIT: &str = "habit-6f1d2c3a-1b2c-4d5e-8f90-a1b2c3d4e5f6";

fn space_of(world: &World, id: &str) -> String {
    world
        .conn()
        .query_row("SELECT space_id FROM sessions WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

fn activate(world: &World, space: &str) {
    world
        .conn()
        .execute("UPDATE space_settings SET active_space_id = ?1", [space])
        .unwrap();
}

fn login(id: &str) -> ProviderAccount {
    ProviderAccount {
        provider: Provider::Claude,
        id: id.into(),
        label: id.into(),
    }
}

/// Automation `automation-<session>` in `cwd` and its run naming `session`,
/// as `automations_upsert` and the awaited `automation_run_update` save them
/// before the run's first turn (`launchAutomation`).
fn save_automation_run(conn: &Connection, session: &str, cwd: &str) {
    let automation = format!("automation-{session}");
    let run = format!("run-{session}");
    conn.execute(
        "INSERT INTO automations (id, definition_json, enabled, next_run_at, updated_at)
         VALUES (?1, ?2, 1, 0, 0)",
        params![
            automation,
            json!({"id": automation, "cwd": cwd}).to_string()
        ],
    )
    .unwrap();
    let run_json =
        json!({"id": run, "automationId": automation, "status": "running", "sessionId": session});
    conn.execute(
        "INSERT INTO automation_runs (id, automation_id, created_at, run_json) VALUES (?1, ?2, 0, ?3)",
        params![run, automation, run_json.to_string()],
    )
    .unwrap();
}

/// Automation run session `id` in `cwd`, saved as the web app saves it.
fn save_run_session(conn: &Connection, id: &str, cwd: &str) {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": cwd,
        "harness": "claude",
        "model": "claude:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [],
        "automationId": format!("automation-{id}"),
    }))
    .unwrap();
    session_store::upsert_session(conn, &session).unwrap();
}

/// A run of an automation in `cwd`: its run row first, then its session.
fn automation_run(world: &World, id: &str, cwd: &str) {
    save_automation_run(&world.conn(), id, cwd);
    save_run_session(&world.conn(), id, cwd);
}

/// Claude login `id`, and `project`'s own choice of it in Space 2, keyed as
/// the front end writes it.
fn project_login_in_space_2(store: &SessionStore, project: &str, id: &str, now: i64) {
    account::save_account(store, &login(id), now).unwrap();
    let project_login = ProjectLogin {
        space_id: SPACE_2.into(),
        project_path: project.into(),
        provider: Provider::Claude,
        account_id: id.into(),
    };
    spaces::set_project_login(store, &project_login).unwrap();
}

/// P2 moves to Space 2 with its own Claude login there (ADE-171), and gets
/// a worker of `lead` (`p2w`). Automation runs, which have no Mono owner,
/// take their project's Space whichever Space is active. Leaves Space 1
/// active.
fn a_project_keeps_its_space_and_login(world: &World, lead: &str) {
    spaces::set_project_space(world.store(), P2, SPACE_2).unwrap();
    project_login_in_space_2(world.store(), P2, "proj2", world.now());
    world.add_session("p2w", P2, Some(lead));
    automation_run(world, "auto2", P2);
    assert_eq!(
        space_of(world, "auto2"),
        SPACE_2,
        "an automation run took the active Space, not its project's"
    );
    activate(world, SPACE_2);
    automation_run(world, "auto1", P);
    assert_eq!(
        space_of(world, "auto1"),
        FIRST_SPACE,
        "an automation run in a Space 1 project took the active Space"
    );
    activate(world, FIRST_SPACE);
}

/// Space 2 sessions in a `remote://` project and in a drive folder, each with
/// its own login there under another spelling of its key, and a Space 1
/// session in P2, which has no login of its own in Space 1.
fn projects_keyed_as_the_front_end_keys_them(world: &World, lead: &str) {
    for (session, cwd, key) in [
        ("remote2", "remote://box/srv/app/", "remote://box/srv/app"),
        ("mixed2", "C:/Work/Proj", "C:/WORK/PROJ"),
    ] {
        project_login_in_space_2(world.store(), key, session, world.now());
        world.add_session(session, cwd, Some(lead));
    }
    world.add_session("s1p2", P2, None);
}

/// A user session saved as the web app saves it, plus a `spaceId` the
/// store must not take.
fn user_session_naming(world: &World, id: &str, space: &str) {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": P,
        "harness": "claude",
        "model": "claude:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [],
        "spaceId": space,
    }))
    .unwrap();
    session_store::upsert_session(&world.conn(), &session).unwrap();
}

/// Defect: a session or a Mono created while Space 2 is active, or a
/// session that names Space 1 itself, lands outside Space 2; a later save
/// moves a session to the active Space; a Mono's chat, a lead its Mono started, that lead's
/// worker, a habit run or a session the habit run started lands in the
/// active Space instead of its parent's; a Space 1 session or an imported
/// Space 2 session runs on another Space's login; a session's own login
/// loses to its Space's; a Space with no login for a CLI starts the child on
/// the CLI's own login instead of refusing; or a session with no row yet
/// takes the caller's `default` login instead of the Space it would join,
/// its parent's or the active one, or loses the login its caller named; an
/// automation run takes the active Space instead of its project's, at its
/// save or at a spawn before it; a project's own login in its Space loses to
/// the Space's login, also for a `remote://`, drive or `~` key in another
/// spelling and for a session not saved yet in a work copy or `~`; or
/// another CLI, or the same project in Space 1, gets that project's login.
#[test]
fn a_session_takes_its_parents_space_and_runs_on_that_spaces_login() {
    let mut world = World::new("space-login");
    let alpha = world.add_mono_in("alpha", MonoRole::Mono, &[P]);
    world.add_session("s1", P, None);
    // Space 2 as `spaces_create` will write it (ADE-187), with its own
    // Claude login and none for Codex.
    world
        .conn()
        .execute(
            "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 2', 0)",
            [SPACE_2],
        )
        .unwrap();
    for id in ["space2", "work"] {
        account::save_account(world.store(), &login(id), world.now()).unwrap();
    }
    let space_login = SpaceLogin {
        space_id: SPACE_2.into(),
        provider: Provider::Claude,
        account_id: "space2".into(),
    };
    account::set_login(world.store(), &space_login).unwrap();

    activate(&world, SPACE_2);
    user_session_naming(&world, "u2", FIRST_SPACE);
    assert_eq!(
        space_of(&world, "u2"),
        SPACE_2,
        "a session started while Space 2 is active is outside Space 2, or took the Space its caller named"
    );
    let alpha_habit = format!("app-{}-{HABIT}", alpha.chat.as_str());
    world.add_session(&alpha_habit, P, None);
    assert_eq!(
        space_of(&world, &alpha_habit),
        FIRST_SPACE,
        "a Space 1 Mono's habit run took the active Space"
    );
    let beta = world.add_mono_in("beta", MonoRole::Mono, &[P]);
    let beta_space: String = world
        .conn()
        .query_row("SELECT space_id FROM monos WHERE id = 'beta'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        beta_space, SPACE_2,
        "a Mono created while Space 2 is active is outside Space 2"
    );

    activate(&world, FIRST_SPACE);
    user_session_naming(&world, "u2", FIRST_SPACE);
    assert_eq!(
        space_of(&world, "u2"),
        SPACE_2,
        "a later save moved a session to the active Space"
    );
    let beta = world.reset_mono_chat(&beta, "beta-chat-2");
    let lead = world.start_lead(&beta, "app-beta-chat-2-r1", P);
    let worker = world.add_session("w2", P, Some(lead.as_str()));
    let habit = format!("app-{}-{HABIT}", beta.chat.as_str());
    world.add_session(&habit, P, None);
    let habit_child = format!("app-{habit}-r2");
    world.add_session(&habit_child, P, None);
    for (id, what) in [
        (beta.chat.as_str(), "a Mono's chat"),
        (lead.as_str(), "a lead its Mono started"),
        (worker.as_str(), "a worker its Space 2 lead started"),
        (habit.as_str(), "a habit run"),
        (habit_child.as_str(), "a session a habit run started"),
    ] {
        assert_eq!(
            space_of(&world, id),
            SPACE_2,
            "{what} is outside its parent's Space"
        );
    }
    a_project_keeps_its_space_and_login(&world, lead.as_str());
    projects_keyed_as_the_front_end_keys_them(&world, lead.as_str());
    // A MonoCode 2 session as the import will write it (ADE-178).
    world.add_session("m2", P, None);
    world
        .conn()
        .execute(
            "UPDATE sessions SET space_id = ?1 WHERE id = 'm2'",
            [SPACE_2],
        )
        .unwrap();

    world.kill();
    #[cfg(unix)]
    spawn::children_run_on_their_spaces_login(&world, &habit);
}

#[cfg(unix)]
mod spawn {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app_with, script, wait_for};
    use super::{project_login_in_space_2, save_automation_run, save_run_session, World, P2};
    use crate::session_store::SessionStore;

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// The CLI's own Claude config folder, as a child on no login gets it.
    fn own() -> String {
        format!(
            "{}\n",
            std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default()
        )
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status();
        assert!(status.unwrap().success(), "git {args:?} failed");
    }

    /// `harness_spawn` on the World's database, through a stand-in fx that
    /// writes its Claude config folder to the file its second argument names.
    struct Spawner {
        root: PathBuf,
        fx: PathBuf,
        app: tauri::App<tauri::test::MockRuntime>,
    }

    impl Spawner {
        fn new(world: &World) -> Self {
            let root = std::env::temp_dir().join(format!("ade172-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(&root).unwrap();
            let fx = script(
                &root.join("fx"),
                r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) printf '%s\n' "$CLAUDE_CONFIG_DIR" > "$2.part" && mv "$2.part" "$2" ;;
esac"#,
            );
            let data = root.join("app-data");
            std::fs::create_dir_all(&data).unwrap();
            let store = SessionStore::open_at(world.path().to_path_buf(), world.now()).unwrap();
            Self {
                root,
                fx,
                app: mock_app_with(&data, store),
            }
        }

        fn store(&self) -> tauri::State<'_, SessionStore> {
            self.app.state::<SessionStore>()
        }

        /// Session `session`'s child in `cwd` as the front end passes it,
        /// asked to start on `provider` login `id`: the config folder it got.
        fn spawn_in(
            &self,
            session: &str,
            provider: &str,
            id: &str,
            cwd: &str,
        ) -> Result<String, String> {
            let out = self.root.join(uuid::Uuid::new_v4().to_string());
            let account = serde_json::json!({"provider": provider, "id": id});
            crate::harness::harness_spawn(
                self.app.handle().clone(),
                self.app.state::<crate::harness::HarnessHost>(),
                session.into(),
                text(&self.fx),
                vec!["acp".into(), text(&out)],
                cwd.into(),
                Some(serde_json::from_value(account).unwrap()),
                Some("fx".into()),
                Some(text(&self.fx)),
                None,
                None,
                None,
                None,
            )?;
            Ok(wait_for(&out))
        }

        /// `spawn_in` a new folder.
        fn spawn(&self, session: &str, provider: &str, id: &str) -> Result<String, String> {
            let workdir = self.root.join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&workdir).unwrap();
            self.spawn_in(session, provider, id, &text(&workdir))
        }

        fn config(&self, id: &str) -> String {
            let dir =
                crate::harness::provider_account_path(self.app.handle(), "claude", id).unwrap();
            format!("{}\n", text(&dir))
        }

        fn activate(&self, space: &str) {
            self.store()
                .lock_conn()
                .unwrap()
                .execute("UPDATE space_settings SET active_space_id = ?1", [space])
                .unwrap();
        }

        fn space_of(&self, id: &str) -> String {
            self.store()
                .lock_conn()
                .unwrap()
                .query_row("SELECT space_id FROM sessions WHERE id = ?1", [id], |row| {
                    row.get(0)
                })
                .unwrap()
        }

        /// A git project in `root` with one work copy: (project, copy).
        fn project_with_a_work_copy(&self) -> (PathBuf, PathBuf) {
            let project = self.root.join("wtproj");
            std::fs::create_dir_all(&project).unwrap();
            git(&project, &["init", "-q"]);
            git(
                &project,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    "init",
                    "--allow-empty",
                ],
            );
            let copy = self.root.join("wtproj-worktrees").join("w1");
            git(&project, &["worktree", "add", "-q", &text(&copy)]);
            (project, copy)
        }
    }

    impl Drop for Spawner {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn children_run_on_their_spaces_login(world: &World, habit: &str) {
        let spawner = Spawner::new(world);
        saved_sessions_run_on_their_projects_login(&spawner, habit);
        unsaved_sessions_run_on_the_login_they_would_get(&spawner, world.now());
    }

    fn saved_sessions_run_on_their_projects_login(spawner: &Spawner, habit: &str) {
        let spawn = |session: &str, provider: &str, id: &str| spawner.spawn(session, provider, id);
        let config = |id: &str| spawner.config(id);
        for (session, login, what) in [
            (
                "w2",
                "space2",
                "a Space 2 child did not get Space 2's CLAUDE_CONFIG_DIR",
            ),
            (
                "p2w",
                "proj2",
                "a project's own login in its Space lost to the Space's login",
            ),
            (
                "remote2",
                "remote2",
                "a remote:// project's own login missed its key",
            ),
            (
                "mixed2",
                "mixed2",
                "a drive folder's own login missed its key in another case",
            ),
            (
                "m2",
                "space2",
                "an imported Space 2 session did not run on Space 2's login",
            ),
        ] {
            assert_eq!(
                spawn(session, "claude", "default"),
                Ok(config(login)),
                "{what}"
            );
        }
        assert_eq!(
            spawn("s1", "claude", "default"),
            Ok(own()),
            "a Space 1 session left the CLI's own login"
        );
        assert_eq!(
            spawn("s1p2", "claude", "default"),
            Ok(own()),
            "a Space 1 session in P2 took P2's Space 2 login"
        );
        assert_eq!(
            spawn(habit, "claude", "work"),
            Ok(config("work")),
            "a session's own login lost to its Space's"
        );
        for session in ["w2", "p2w"] {
            assert_eq!(
                spawn(session, "codex", "default"),
                Err("Space 2 has no Codex login".into()),
                "another CLI took a Claude login, or a Space with no Codex login did not refuse"
            );
        }
    }

    fn unsaved_sessions_run_on_the_login_they_would_get(spawner: &Spawner, now: i64) {
        assert_eq!(
            spawner.spawn("unsaved", "claude", "default"),
            Ok(own()),
            "a session with no row, Space 1 active, left the CLI's own login"
        );
        save_automation_run(&spawner.store().lock_conn().unwrap(), "auto-race", P2);
        assert_eq!(
            spawner.spawn("auto-race", "claude", "default"),
            Ok(spawner.config("proj2")),
            "an automation run that spawned before its first save took the active Space's login"
        );
        save_run_session(&spawner.store().lock_conn().unwrap(), "auto-race", P2);
        assert_eq!(
            spawner.space_of("auto-race"),
            super::SPACE_2,
            "an automation run saved after its first spawn left its project's Space"
        );

        spawner.activate(super::SPACE_2);
        assert_eq!(
            spawner.spawn("unsaved", "claude", "default"),
            Ok(spawner.config("space2")),
            "a session with no row, Space 2 active, did not get Space 2's login"
        );
        assert_eq!(
            spawner.spawn("unsaved", "codex", "default"),
            Err("Space 2 has no Codex login".into()),
            "a session with no row in a Space with no Codex login was not refused"
        );
        assert_eq!(
            spawner.spawn("app-s1-r9", "claude", "default"),
            Ok(own()),
            "a session with no row took the active Space over its parent's"
        );
        assert_eq!(
            spawner.spawn("signin", "claude", "work"),
            Ok(spawner.config("work")),
            "a login its caller named lost to the active Space's"
        );

        let (project, copy) = spawner.project_with_a_work_copy();
        project_login_in_space_2(&spawner.store(), &text(&project), "wt2", now);
        project_login_in_space_2(&spawner.store(), "~", "home2", now);
        assert_eq!(
            spawner.spawn_in("wt-first", "claude", "default", &text(&copy)),
            Ok(spawner.config("wt2")),
            "a session not saved yet in a work copy missed its project's own login"
        );
        assert_eq!(
            spawner.spawn_in("home-first", "claude", "default", "~"),
            Ok(spawner.config("home2")),
            "a `~` session missed the `~` key's own login"
        );
    }
}
