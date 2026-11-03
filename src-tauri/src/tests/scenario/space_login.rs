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

use serde_json::json;

use super::world::World;
use crate::mono_roster::MonoRole;
use crate::session_store::{self, SessionUpsert};
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

/// A run of automation `automation-1`, saved as the web app saves it.
fn automation_run(world: &World, id: &str, cwd: &str) {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": cwd,
        "harness": "claude",
        "model": "claude:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [],
        "automationId": "automation-1",
    }))
    .unwrap();
    session_store::upsert_session(&world.conn(), &session).unwrap();
}

/// P2 moves to Space 2 with its own Claude login there (ADE-171), and gets
/// a worker of `lead` (`p2w`). Automation runs, which have no Mono owner,
/// take their project's Space whichever Space is active. Leaves Space 1
/// active.
fn a_project_keeps_its_space_and_login(world: &World, lead: &str) {
    spaces::set_project_space(world.store(), P2, SPACE_2).unwrap();
    account::save_account(world.store(), &login("proj2"), world.now()).unwrap();
    let project_login = ProjectLogin {
        space_id: SPACE_2.into(),
        project_path: P2.into(),
        provider: Provider::Claude,
        account_id: "proj2".into(),
    };
    spaces::set_project_login(world.store(), &project_login).unwrap();
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
/// automation run takes the active Space instead of its project's; or a
/// project's own login in its Space loses to the Space's login.
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
    use std::path::Path;

    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app_with, script, wait_for};
    use super::World;
    use crate::session_store::SessionStore;

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// A stand-in fx that writes its Claude config folder to `ran`.
    fn stand_in_fx(root: &Path) -> std::path::PathBuf {
        script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) printf '%s\n' "$CLAUDE_CONFIG_DIR" > ran.part && mv ran.part ran ;;
esac"#,
        )
    }

    /// `harness_spawn` on the World's database with a stand-in fx that
    /// writes its Claude config folder.
    pub(super) fn children_run_on_their_spaces_login(world: &World, habit: &str) {
        let root = std::env::temp_dir().join(format!("ade172-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let fx = stand_in_fx(&root);
        let data = root.join("app-data");
        std::fs::create_dir_all(&data).unwrap();
        let store = SessionStore::open_at(world.path().to_path_buf(), world.now()).unwrap();
        let app = mock_app_with(&data, store);
        let spawn = |session: &str, provider: &str, id: &str| -> Result<String, String> {
            let workdir = root.join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&workdir).unwrap();
            let account = serde_json::json!({"provider": provider, "id": id});
            crate::harness::harness_spawn(
                app.handle().clone(),
                app.state::<crate::harness::HarnessHost>(),
                session.into(),
                text(&fx),
                vec!["acp".into()],
                text(&workdir),
                Some(serde_json::from_value(account).unwrap()),
                Some("fx".into()),
                Some(text(&fx)),
                None,
                None,
                None,
                None,
            )?;
            Ok(wait_for(&workdir.join("ran")))
        };
        let own = format!(
            "{}\n",
            std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default()
        );
        let config = |id: &str| {
            let dir = crate::harness::provider_account_path(app.handle(), "claude", id).unwrap();
            format!("{}\n", text(&dir))
        };

        assert_eq!(
            spawn("w2", "claude", "default"),
            Ok(config("space2")),
            "a Space 2 child did not get Space 2's CLAUDE_CONFIG_DIR"
        );
        assert_eq!(
            spawn("p2w", "claude", "default"),
            Ok(config("proj2")),
            "a project's own login in its Space lost to the Space's login"
        );
        assert_eq!(
            spawn("m2", "claude", "default"),
            Ok(config("space2")),
            "an imported Space 2 session did not run on Space 2's login"
        );
        assert_eq!(
            spawn("s1", "claude", "default"),
            Ok(own.clone()),
            "a Space 1 session left the CLI's own login"
        );
        assert_eq!(
            spawn(habit, "claude", "work"),
            Ok(config("work")),
            "a session's own login lost to its Space's"
        );
        assert_eq!(
            spawn("w2", "codex", "default"),
            Err("Space 2 has no Codex login".into()),
            "a Space with no Codex login started a child on another login"
        );
        assert_eq!(
            spawn("unsaved", "claude", "default"),
            Ok(own.clone()),
            "a session with no row, Space 1 active, left the CLI's own login"
        );

        app.state::<SessionStore>()
            .lock_conn()
            .unwrap()
            .execute("UPDATE space_settings SET active_space_id = 'space-2'", [])
            .unwrap();
        assert_eq!(
            spawn("unsaved", "claude", "default"),
            Ok(config("space2")),
            "a session with no row, Space 2 active, did not get Space 2's login"
        );
        assert_eq!(
            spawn("unsaved", "codex", "default"),
            Err("Space 2 has no Codex login".into()),
            "a session with no row in a Space with no Codex login was not refused"
        );
        assert_eq!(
            spawn("app-s1-r9", "claude", "default"),
            Ok(own),
            "a session with no row took the active Space over its parent's"
        );
        assert_eq!(
            spawn("signin", "claude", "work"),
            Ok(config("work")),
            "a login its caller named lost to the active Space's"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
