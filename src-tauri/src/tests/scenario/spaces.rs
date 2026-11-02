//! Spaces foundation (ADE-168). The app opens a database written before
//! Spaces existed, migrates it, and every Mono, session and project lands in
//! Space 1; the storage commands round-trip and survive a restart, which
//! migrates again as a no-op. Then `harness_spawn` on Tauri's mock runtime
//! starts a stand-in CLI through the spawn hook with the folder and login it
//! was given. With `MONOCODE_SCENARIO_DB` it runs on a copy of a real database.

use rusqlite::Connection;

use super::world::World;
use crate::mono_roster::MonoRole;
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, RouteGrant};
use crate::spaces::leases::{self, CheckoutLease};
use crate::spaces::{self, Provider, SettingsPatch, Space, SpaceSettings, FIRST_SPACE};

const SIX_HOURS_MS: i64 = 6 * 60 * 60 * 1000;

/// Takes the Spaces schema back out, as a database from before Spaces is.
fn drop_spaces(conn: &Connection) {
    conn.execute_batch(
        "DROP TABLE space_accounts;
         DROP TABLE account_route_grants;
         DROP TABLE checkout_leases;
         DROP TABLE space_projects;
         DROP TABLE space_settings;
         DROP TABLE provider_accounts;
         DROP TABLE spaces;
         ALTER TABLE monos DROP COLUMN space_id;
         ALTER TABLE sessions DROP COLUMN space_id;",
    )
    .unwrap();
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn backfilled_to_space_one(world: &World) {
    let conn = world.conn();
    for table in ["monos", "sessions"] {
        assert!(
            count(&conn, &format!("SELECT count(*) FROM {table}")) > 0,
            "{table}: no rows to backfill"
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT count(*) FROM {table} WHERE space_id IS NOT 'space-1'")
            ),
            0,
            "{table}: a row outside Space 1"
        );
    }
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM (SELECT DISTINCT cwd FROM sessions)
              WHERE cwd NOT IN (SELECT project_path FROM space_projects WHERE space_id = 'space-1')"
        ),
        0,
        "a session's project is not in Space 1"
    );
    eprintln!(
        "spaces: Space 1 holds {} monos, {} sessions, {} projects",
        count(
            &conn,
            "SELECT count(*) FROM monos WHERE space_id = 'space-1'"
        ),
        count(
            &conn,
            "SELECT count(*) FROM sessions WHERE space_id = 'space-1'"
        ),
        count(
            &conn,
            "SELECT count(*) FROM space_projects WHERE space_id = 'space-1'"
        )
    );
    assert_eq!(
        spaces::project_space(&conn, "/work/a").unwrap().as_deref(),
        Some(FIRST_SPACE)
    );
    let first = spaces::list(&conn).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|s| (s.id.as_str(), s.name.as_str()))
            .collect::<Vec<_>>(),
        [(FIRST_SPACE, "Space 1")]
    );
    assert_eq!(
        spaces::settings(&conn).unwrap(),
        SpaceSettings {
            active_space_id: FIRST_SPACE.into(),
            route_grant_ttl_ms: SIX_HOURS_MS
        },
        "settings: Space 1 active, a 6 hour route-grant TTL"
    );
    assert_eq!(
        account::accounts(&conn)
            .unwrap()
            .iter()
            .map(|a| (a.provider, a.id.as_str()))
            .collect::<Vec<_>>(),
        [
            (Provider::Claude, "default"),
            (Provider::Codex, "default"),
            (Provider::Devin, "default")
        ],
        "each CLI's own login"
    );
}

fn work_login() -> ProviderAccount {
    ProviderAccount {
        provider: Provider::Claude,
        id: "work".into(),
        label: "Work".into(),
    }
}

fn space_two_login() -> SpaceLogin {
    SpaceLogin {
        space_id: "space-2".into(),
        provider: Provider::Claude,
        account_id: "work".into(),
    }
}

fn grant(now: i64) -> RouteGrant {
    RouteGrant {
        session_id: "s2".into(),
        provider: Provider::Claude,
        from_account_id: "default".into(),
        to_account_id: "work".into(),
        granted_at: now,
        expires_at: now + SIX_HOURS_MS,
    }
}

fn commands_store_what_they_are_given(world: &World) {
    let now = world.now();
    let conn = world.conn();
    let two = spaces::create(&conn, "Client", None, now).unwrap();
    assert_eq!(two.id, "space-2", "the next Space number");
    spaces::update(&conn, "space-2", "Space 2", Some("#3366ff")).unwrap();
    assert!(
        spaces::update(&conn, "space-9", "Nobody", None).is_err(),
        "renamed a Space that does not exist"
    );
    let patch = SettingsPatch {
        active_space_id: Some("space-2".into()),
        route_grant_ttl_ms: Some(60_000),
    };
    spaces::set_settings(&conn, &patch).unwrap();
    let missing = SettingsPatch {
        active_space_id: Some("space-9".into()),
        ..SettingsPatch::default()
    };
    assert!(
        spaces::set_settings(&conn, &missing).is_err(),
        "a Space that does not exist became active"
    );
    spaces::set_project_space(&conn, "/work/b", "space-2").unwrap();

    account::save_account(&conn, &work_login(), now).unwrap();
    account::set_login(&conn, &space_two_login()).unwrap();
    let stranger = SpaceLogin {
        account_id: "nobody".into(),
        ..space_two_login()
    };
    assert!(
        account::set_login(&conn, &stranger).is_err(),
        "a Space took a login that does not exist"
    );
    grants::save_grant(&conn, &grant(now)).unwrap();
    leases::take_lease(&conn, "/work/b", "space-2", now).unwrap();
    assert!(
        leases::take_lease(&conn, "/work/b", FIRST_SPACE, now).is_err(),
        "a second Space took a leased checkout"
    );
}

fn the_store_holds(world: &World, at: i64) {
    let conn = world.conn();
    assert_eq!(
        spaces::list(&conn).unwrap()[1],
        Space {
            id: "space-2".into(),
            name: "Space 2".into(),
            color: Some("#3366ff".into()),
            created_at: at
        }
    );
    assert_eq!(
        spaces::settings(&conn).unwrap(),
        SpaceSettings {
            active_space_id: "space-2".into(),
            route_grant_ttl_ms: 60_000
        }
    );
    assert_eq!(
        spaces::project_space(&conn, "/work/b").unwrap().as_deref(),
        Some("space-2")
    );
    assert_eq!(
        spaces::project_space(&conn, "/work/a").unwrap().as_deref(),
        Some(FIRST_SPACE)
    );
    assert!(account::accounts(&conn).unwrap().contains(&work_login()));
    assert_eq!(account::logins(&conn).unwrap(), [space_two_login()]);
    assert_eq!(grants::grant(&conn, "s2").unwrap(), Some(grant(at)));
    assert_eq!(
        leases::leases(&conn).unwrap(),
        [CheckoutLease {
            checkout_path: "/work/b".into(),
            space_id: "space-2".into(),
            leased_at: at
        }]
    );
}

fn removing_a_login_drops_its_space_login_and_grant(world: &World) {
    let conn = world.conn();
    account::remove_account(&conn, Provider::Claude, "work").unwrap();
    assert_eq!(account::logins(&conn).unwrap(), []);
    assert_eq!(grants::grant(&conn, "s2").unwrap(), None);
    grants::save_grant(&conn, &grant(world.now())).unwrap_err();
    leases::release_lease(&conn, "/work/b").unwrap();
    assert_eq!(leases::leases(&conn).unwrap(), []);
}

/// Defect: a Mono, session or project from before Spaces lands outside
/// Space 1 or not at all; the settings miss Space 1 or the 6 hour TTL; a
/// restart migrates again and resets or duplicates rows; a command loses or
/// alters what it stored, or stores a row that names a missing Space or
/// login; or the spawn hook changes the folder or login a child starts with.
#[test]
fn spaces_backfill_round_trip_and_spawn_hook() {
    let mut world = World::new("spaces");
    world.add_mono("alpha", MonoRole::Mono);
    world.add_session("s1", "/work/a", None);
    world.add_session("s2", "/work/b", None);
    let started = world.now();
    world.kill();
    drop_spaces(&Connection::open(world.path()).unwrap());
    world.restart();
    backfilled_to_space_one(&world);

    world.add_session("s3", "/work/c", None);
    assert_eq!(
        count(
            &world.conn(),
            "SELECT count(*) FROM sessions WHERE id = 's3' AND space_id = 'space-1'"
        ),
        1,
        "a new session is outside Space 1"
    );
    commands_store_what_they_are_given(&world);
    the_store_holds(&world, started);
    world.down(60_000);
    the_store_holds(&world, started);
    assert_eq!(
        count(
            &world.conn(),
            "SELECT count(*) FROM provider_accounts WHERE id = 'default'"
        ),
        3,
        "a restart added the default logins again"
    );
    removing_a_login_drops_its_space_login_and_grant(&world);

    #[cfg(unix)]
    spawn::the_hook_keeps_the_folder_and_login();
}

#[cfg(unix)]
mod spawn {
    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app, script, wait_for};
    use crate::harness::HarnessAccount;

    /// `harness_spawn` with a stand-in fx that writes the folder it runs in,
    /// its arguments and its Claude config folder.
    pub(super) fn the_hook_keeps_the_folder_and_login() {
        let root = std::env::temp_dir().join(format!("ade168-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let fx = script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) printf '%s\n%s\n%s\n' "$(pwd -P)" "$*" "$CLAUDE_CONFIG_DIR" > ran.part && mv ran.part ran ;;
esac"#,
        );
        let data = root.join("app-data");
        std::fs::create_dir_all(&data).unwrap();
        let app = mock_app(&data);
        let workdir = root.join("project");
        std::fs::create_dir_all(&workdir).unwrap();
        let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let account = HarnessAccount {
            provider: "claude".into(),
            id: "work".into(),
        };
        crate::harness::harness_spawn(
            app.handle().clone(),
            app.state::<crate::harness::HarnessHost>(),
            "hooked".into(),
            text(&fx),
            vec!["acp".into()],
            text(&workdir),
            Some(account),
            Some("fx".into()),
            Some(text(&fx)),
            None,
            None,
            None,
        )
        .unwrap();
        let config = crate::harness::provider_account_path(app.handle(), "claude", "work").unwrap();
        assert_eq!(
            wait_for(&workdir.join("ran")),
            format!("{}\nacp\n{}\n", text(&workdir), text(&config)),
            "the CLI started in another folder, with other arguments or another login"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
