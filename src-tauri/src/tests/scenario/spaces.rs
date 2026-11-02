//! Spaces foundation (ADE-168). The app opens a database written before
//! Spaces existed, migrates it, and every Mono and session lands in Space 1;
//! a restart migrates again as a no-op. The login, grant and lease storage
//! round-trips, survives a restart and refuses what the rest of the app
//! cannot use. Then `harness_spawn` on Tauri's mock
//! runtime starts a stand-in CLI through the spawn hook with the folder and
//! login it was given. With `MONOCODE_SCENARIO_DB` it runs on a copy of a
//! real database.

use rusqlite::{params, Connection};

use super::world::World;
use crate::mono_roster::MonoRole;
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, RouteGrant};
use crate::spaces::leases::{self, CheckoutLease};
use crate::spaces::{Provider, FIRST_SPACE};

/// A grant until a monthly limit resets: longer than any weekly cap.
const THIRTY_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Takes the Spaces schema back out, as a database from before Spaces is.
fn drop_spaces(conn: &Connection) {
    conn.execute_batch(
        "DROP TABLE space_accounts;
         DROP TABLE space_project_accounts;
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

fn rows(conn: &Connection, sql: &str) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
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
    eprintln!(
        "spaces: Space 1 holds {} monos, {} sessions, {} project folders",
        count(
            &conn,
            "SELECT count(*) FROM monos WHERE space_id = 'space-1'"
        ),
        count(
            &conn,
            "SELECT count(*) FROM sessions WHERE space_id = 'space-1'"
        ),
        count(&conn, "SELECT count(DISTINCT cwd) FROM sessions")
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM space_projects"),
        0,
        "the migration moved a project out of Space 1"
    );
}

/// The rows a migration seeds, one string each, in a fixed order.
fn seeded(conn: &Connection) -> Vec<String> {
    rows(
        conn,
        "SELECT 'space ' || id || ' ' || name FROM spaces
         UNION ALL
         SELECT 'settings ' || active_space_id || ' ' || route_grant_ttl_ms FROM space_settings
         UNION ALL
         SELECT * FROM (SELECT 'login ' || provider || ' ' || id FROM provider_accounts
                        ORDER BY provider, id)",
    )
}

/// A grant as long as its writer picks: "until the first login resets" on a
/// monthly limit is about 30 days.
fn a_grant_lasts_as_long_as_its_writer_says(world: &World) {
    let (conn, now) = (world.conn(), world.now());
    let grant = |expires_at: i64| {
        conn.execute(
            "INSERT INTO account_route_grants VALUES ('s1', 'codex', 'default', 'default', ?1, ?2)",
            params![now, expires_at],
        )
    };
    let thirty_days = 30 * 24 * 60 * 60 * 1000;
    assert_eq!(
        grant(now + thirty_days),
        Ok(1),
        "the schema refused a 30 day grant"
    );
    conn.execute("DELETE FROM account_route_grants", [])
        .unwrap();
    assert!(grant(now).is_err(), "stored a grant that ends as it starts");
}

fn work_login() -> ProviderAccount {
    ProviderAccount {
        provider: Provider::Claude,
        id: "work".into(),
        label: "Work".into(),
    }
}

fn space_one_login() -> SpaceLogin {
    SpaceLogin {
        space_id: FIRST_SPACE.into(),
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
        expires_at: now + THIRTY_DAYS_MS,
    }
}

fn logins_grants_and_leases_store_what_they_are_given(world: &World) {
    let (store, now) = (world.store(), world.now());
    account::save_account(store, &work_login(), now).unwrap();
    for id in ["../x", "a b", "", &"x".repeat(81)] {
        let bad = ProviderAccount {
            id: id.into(),
            ..work_login()
        };
        assert_eq!(
            account::save_account(store, &bad, now),
            Err("Invalid provider account id".into()),
            "stored the login id {id:?}"
        );
    }
    for label in ["", " \t "] {
        let blank = ProviderAccount {
            label: label.into(),
            ..work_login()
        };
        assert_eq!(
            account::save_account(store, &blank, now),
            Err("A provider account needs a label".into()),
            "stored the label {label:?}"
        );
    }
    let renamed = ProviderAccount {
        label: " Work \t laptop ".into(),
        ..work_login()
    };
    account::save_account(store, &renamed, now).unwrap();
    assert_eq!(
        account::accounts(store)
            .unwrap()
            .into_iter()
            .filter(|a| a.id == "work")
            .map(|a| a.label)
            .collect::<Vec<_>>(),
        ["Work laptop"],
        "a rename was lost, duplicated the login, or kept its stray spaces"
    );
    let ipc = |provider: &str| {
        let account = serde_json::json!({"provider": provider, "id": "work", "label": "Work"});
        let login =
            serde_json::json!({"spaceId": FIRST_SPACE, "provider": provider, "accountId": "work"});
        let grant = serde_json::json!({
            "sessionId": "s2", "provider": provider, "fromAccountId": "default",
            "toAccountId": "work", "grantedAt": now, "expiresAt": now + THIRTY_DAYS_MS
        });
        (
            serde_json::from_value::<ProviderAccount>(account),
            serde_json::from_value::<SpaceLogin>(login),
            serde_json::from_value::<RouteGrant>(grant),
        )
    };
    let (account, login, route) = ipc("claude");
    assert_eq!(
        (account.unwrap(), login.unwrap(), route.unwrap()),
        (work_login(), space_one_login(), grant(now)),
        "a command's arguments do not parse as the UI sends them"
    );
    let (account, login, route) = ipc("gemini");
    assert!(
        account.is_err() && login.is_err() && route.is_err(),
        "a command took a CLI that keeps no logins"
    );
    account::set_login(store, &space_one_login()).unwrap();
    let stranger = SpaceLogin {
        account_id: "nobody".into(),
        ..space_one_login()
    };
    assert!(
        account::set_login(store, &stranger).is_err(),
        "a Space took a login that does not exist"
    );

    grants::save_grant(store, &grant(now)).unwrap();
    let instant = RouteGrant {
        expires_at: now,
        ..grant(now)
    };
    assert!(
        grants::save_grant(store, &instant).is_err(),
        "the grant command stored a grant that ends as it starts"
    );

    leases::take_lease(store, "/work/b/", "space-2", now).unwrap();
    only_absolute_folders_are_leased(store, now);
    assert!(
        leases::take_lease(store, "/work/b", FIRST_SPACE, now).is_err(),
        "a second Space took a leased checkout"
    );
    assert_eq!(
        leases::release_lease(store, "/work/b", FIRST_SPACE),
        Err("space-1 holds no lease on /work/b".into()),
        "Space 1 released Space 2's lease"
    );
}

/// A relative folder is refused; each Windows form is leased under its
/// `pathKey()` form, so dropping the drive or the UNC arm fails here.
fn only_absolute_folders_are_leased(store: &crate::session_store::SessionStore, now: i64) {
    for relative in ["work/c", "\\work\\c"] {
        assert_eq!(
            leases::take_lease(store, relative, FIRST_SPACE, now),
            Err(format!("Not an absolute folder: {relative}")),
            "leased a relative folder"
        );
    }
    for (path, key) in [
        ("C:\\work\\d", "c:/work/d"),
        ("C:/work/e", "c:/work/e"),
        ("\\\\host\\share\\f", "//host/share/f"),
    ] {
        assert_eq!(
            leases::take_lease(store, path, FIRST_SPACE, now),
            Ok(()),
            "refused the Windows folder {path}"
        );
        assert_eq!(
            leases::release_lease(store, key, FIRST_SPACE),
            Ok(()),
            "{path} was not leased under its pathKey() form {key}"
        );
    }
}

fn logins_grants_and_leases_hold(world: &World, at: i64) {
    let store = world.store();
    assert_eq!(
        account::accounts(store)
            .unwrap()
            .iter()
            .map(|a| (a.provider, a.id.as_str()))
            .collect::<Vec<_>>(),
        [
            (Provider::Claude, "default"),
            (Provider::Claude, "work"),
            (Provider::Codex, "default"),
            (Provider::Devin, "default")
        ]
    );
    assert_eq!(account::logins(store).unwrap(), [space_one_login()]);
    assert_eq!(grants::grant(store, "s2").unwrap(), Some(grant(at)));
    assert_eq!(
        leases::leases(store).unwrap(),
        [CheckoutLease {
            checkout_path: "/work/b".into(),
            space_id: "space-2".into(),
            leased_at: at
        }],
        "the lease is lost, moved, or kept its trailing slash"
    );
}

fn removing_a_login_drops_its_space_login_and_grant(world: &World) {
    let store = world.store();
    assert_eq!(
        account::remove_account(store, Provider::Claude, "default"),
        Err("The default provider account cannot be removed".into()),
        "removed a CLI's own login"
    );
    account::remove_account(store, Provider::Claude, "work").unwrap();
    assert_eq!(account::logins(store).unwrap(), []);
    assert_eq!(grants::grant(store, "s2").unwrap(), None);
    grants::save_grant(store, &grant(world.now())).unwrap_err();
    leases::release_lease(store, "/work/b", "space-2").unwrap();
    assert_eq!(leases::leases(store).unwrap(), []);
    assert!(
        leases::release_lease(store, "/work/b", "space-2").is_err(),
        "released a lease nobody holds"
    );
}

/// Defect: a Mono or session from before Spaces, or one written after the
/// migration, lands outside Space 1; the migration writes a project row in
/// another form than the one rule (no row = Space 1); the schema caps a
/// grant's length or stores one that ends as it starts; the seeds miss Space 1,
/// the 6 hour TTL or a CLI's default login; or a restart migrates again and
/// reseeds or duplicates rows; a login, grant or lease is lost or altered,
/// survives the removal of its login, or is stored when its login id is not
/// a valid folder name or its label is blank, a rename is lost, a command
/// takes a CLI without logins or misreads the UI's arguments, a grant ends as
/// it starts or is refused for lasting 30 days, the `default` login goes, a
/// lease path is relative (a single leading backslash included), or one
/// Space releases another's lease; or a harness child starts without the
/// spawn hook, in another folder or with another login, or the folder check runs
/// before the hook.
#[test]
fn spaces_migration_backfills_space_one_and_reruns_as_a_no_op() {
    let mut world = World::new("spaces");
    world.add_mono("alpha", MonoRole::Mono);
    world.add_session("s1", "/work/a", None);
    world.add_session("s2", "/work/b", None);
    world.kill();
    drop_spaces(&Connection::open(world.path()).unwrap());
    world.restart();
    backfilled_to_space_one(&world);
    let seeds = [
        "space space-1 Space 1",
        "settings space-1 21600000",
        "login claude default",
        "login codex default",
        "login devin default",
    ];
    assert_eq!(
        seeded(&world.conn()),
        seeds,
        "Space 1, active, a 6 hour route-grant TTL and each CLI's own login"
    );
    a_grant_lasts_as_long_as_its_writer_says(&world);

    world.add_session("s3", "/work/c", None);
    assert_eq!(
        count(
            &world.conn(),
            "SELECT count(*) FROM sessions WHERE id = 's3' AND space_id = 'space-1'"
        ),
        1,
        "a new session is outside Space 1"
    );
    world
        .conn()
        .execute("UPDATE space_settings SET route_grant_ttl_ms = 60000", [])
        .unwrap();
    world.down(60_000);
    let mut kept = seeds;
    kept[1] = "settings space-1 60000";
    assert_eq!(
        seeded(&world.conn()),
        kept,
        "a restart reseeded or changed the Spaces rows"
    );
    backfilled_to_space_one(&world);

    // Space 2's row as `spaces_create` will write it (ADE-187).
    world
        .conn()
        .execute(
            "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Space 2', 0)",
            [],
        )
        .unwrap();
    let at = world.now();
    logins_grants_and_leases_store_what_they_are_given(&world);
    world.down(60_000);
    logins_grants_and_leases_hold(&world, at);
    removing_a_login_drops_its_space_login_and_grant(&world);

    #[cfg(unix)]
    spawn::the_hook_keeps_the_folder_and_login();
}

#[cfg(unix)]
mod spawn {
    use std::path::Path;

    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app, script, wait_for};
    use crate::spaces::HOOKED_SPAWNS;

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn hooked() -> u32 {
        HOOKED_SPAWNS.with(std::cell::Cell::get)
    }

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
        let spawn = |session: &str, workdir: &Path| {
            let account = serde_json::from_str(r#"{"provider":"claude","id":"work"}"#).unwrap();
            crate::harness::harness_spawn(
                app.handle().clone(),
                app.state::<crate::harness::HarnessHost>(),
                session.into(),
                text(&fx),
                vec!["acp".into()],
                text(workdir),
                Some(account),
                Some("fx".into()),
                Some(text(&fx)),
                None,
                None,
                None,
            )
        };

        let before = hooked();
        let workdir = root.join("project");
        std::fs::create_dir_all(&workdir).unwrap();
        spawn("hooked", &workdir).unwrap();
        assert_eq!(
            hooked(),
            before + 1,
            "harness_spawn started a child without the spawn hook, or ran it twice"
        );
        let config = crate::harness::provider_account_path(app.handle(), "claude", "work").unwrap();
        assert_eq!(
            wait_for(&workdir.join("ran")),
            format!("{}\nacp\n{}\n", text(&workdir), text(&config)),
            "the CLI started in another folder, with other arguments or another login"
        );
        let missing = root.join("missing");
        assert_eq!(
            spawn("lost", &missing).unwrap_err(),
            format!("Working directory does not exist: {}", text(&missing))
        );
        assert_eq!(
            hooked(),
            before + 2,
            "the folder check ran before the spawn hook"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
