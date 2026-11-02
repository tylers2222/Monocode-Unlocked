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
use crate::spaces::grants::{self, GrantLength, GrantRequest, RouteGrant};
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

/// The spawn hook knows a CLI's logins only by its exact name. Kills the
/// gate/mutation misses in `Provider::parse` (spaces/mod.rs:53, body replaced
/// with `None`; :55, `==` -> `!=`).
fn providers_parse_by_their_cli_name() {
    use crate::spaces::Provider;
    for (name, provider) in [
        ("claude", Provider::Claude),
        ("codex", Provider::Codex),
        ("devin", Provider::Devin),
    ] {
        assert_eq!(Provider::parse(name), Some(provider), "{name}");
    }
    for name in ["", "Claude", "claude ", "omp", "fx"] {
        assert_eq!(Provider::parse(name), None, "{name:?} named a login CLI");
    }
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

/// The Settings default for a grant's length may be 1 ms to exactly 7 days.
/// Kills the gate/mutation miss at spaces/mod.rs:19:39 (`*` -> `+` in
/// `MAX_ROUTE_GRANT_TTL_MS`, which makes the cap 1 day), the other `*` -> `+`
/// and `*` -> `/` mutants of that product, and a cap or floor moved off
/// 7 days and 1 ms.
fn the_settings_ttl_is_capped_at_seven_days(world: &World) {
    let conn = world.conn();
    let ttl = |ms: i64| {
        conn.execute(
            "UPDATE space_settings SET route_grant_ttl_ms = ?1",
            params![ms],
        )
    };
    let seven_days = 7 * 24 * 60 * 60 * 1000;
    assert_eq!(ttl(seven_days), Ok(1), "a 7 day TTL was refused");
    assert!(ttl(seven_days + 1).is_err(), "a TTL past 7 days was stored");
    assert_eq!(ttl(1), Ok(1), "a 1 ms TTL was refused");
    assert!(ttl(0).is_err(), "a TTL of 0 was stored");
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
        let request = serde_json::json!({
            "sessionId": "s2", "provider": provider, "fromAccountId": "default",
            "toAccountId": "work", "length": "6h", "firstLoginResetsAt": null
        });
        (
            serde_json::from_value::<ProviderAccount>(account),
            serde_json::from_value::<SpaceLogin>(login),
            serde_json::from_value::<GrantRequest>(request),
        )
    };
    let (account, login, route) = ipc("claude");
    assert_eq!(
        (account.unwrap(), login.unwrap(), route.unwrap()),
        (
            work_login(),
            space_one_login(),
            request(GrantLength::SixHours, None)
        ),
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
    providers_parse_by_their_cli_name();
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
    the_settings_ttl_is_capped_at_seven_days(&world);
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

const HOUR_MS: i64 = 60 * 60 * 1000;

fn request(length: GrantLength, first_login_resets_at: Option<i64>) -> GrantRequest {
    GrantRequest {
        session_id: "s2".into(),
        provider: Provider::Claude,
        from_account_id: "default".into(),
        to_account_id: "work".into(),
        length,
        first_login_resets_at,
    }
}

/// The spawn hook for `session`'s child on `provider`'s login `login` at the
/// World's time: the login the child starts with, or the error that stops it.
fn spawn_on(world: &World, session: &str, provider: &str, login: &str) -> Result<String, String> {
    let target = crate::spaces::SpawnTarget {
        workdir: "/work/b".into(),
        account: Some(crate::harness::HarnessAccount {
            provider: provider.into(),
            id: login.into(),
        }),
    };
    crate::spaces::prepare_spawn(world.store(), session, None, world.now(), target)
        .map(|target| target.account.unwrap().id)
}

/// Defect: a grant lasts another length than the one picked (the Settings
/// 6 h for a 3 h pick, a wrong hour count, the first login's reset for a fixed
/// length); "until the first login resets" guesses a missing reset, takes a
/// past one or caps a far one; a refused request still replaces the grant; or
/// the spawn hook leaves a granted session's child on its first login while
/// the grant lasts, moves another session, CLI or login, lets the child start
/// on the granted login once the grant ends (or a millisecond early or late),
/// or stops a session sent back to its first login.
#[test]
fn route_grants_last_the_picked_length_and_stop_the_turn_after() {
    let mut world = World::new("route-grants");
    world.add_session("s1", "/work/a", None);
    world.add_session("s2", "/work/b", None);
    account::save_account(world.store(), &work_login(), world.now()).unwrap();
    assert_eq!(
        seeded(&world.conn())[1],
        "settings space-1 21600000",
        "Settings no longer say 6 h, so the 3 h checkpoint proves nothing"
    );
    let t0 = world.now();
    let write =
        |length, resets_at| grants::write_grant(world.store(), &request(length, resets_at), t0);
    for (length, hours) in [
        (GrantLength::OneHour, 1),
        (GrantLength::ThreeHours, 3),
        (GrantLength::SixHours, 6),
        (GrantLength::TwelveHours, 12),
        (GrantLength::TwentyFourHours, 24),
    ] {
        let grant = write(length, None).unwrap();
        assert_eq!(
            (grant.granted_at, grant.expires_at),
            (t0, t0 + hours * HOUR_MS),
            "{length:?} does not last {hours} h from now"
        );
        assert_eq!(grants::grant(world.store(), "s2").unwrap(), Some(grant));
    }
    let in_twenty_days = t0 + 20 * 24 * HOUR_MS;
    assert_eq!(
        write(GrantLength::UntilReset, Some(in_twenty_days)).map(|grant| grant.expires_at),
        Ok(in_twenty_days),
        "a reset 20 days out was refused or capped"
    );
    let no_reset = "The provider reported no reset time for the first login; pick a length";
    let reset = "The first login has already reset; pick a length";
    for (resets_at, error) in [(None, no_reset), (Some(t0), reset), (Some(t0 - 1), reset)] {
        assert_eq!(
            write(GrantLength::UntilReset, resets_at),
            Err(error.into()),
            "\"until the first login resets\" took the reset {resets_at:?}"
        );
    }
    assert_eq!(
        grants::grant(world.store(), "s2")
            .unwrap()
            .map(|grant| grant.expires_at),
        Some(in_twenty_days),
        "a refused request replaced the grant"
    );

    let picked = write(GrantLength::ThreeHours, Some(in_twenty_days)).unwrap();
    assert_eq!(
        picked.expires_at,
        t0 + 3 * HOUR_MS,
        "the 3 h pick took another length"
    );
    assert_eq!(
        spawn_on(&world, "s2", "claude", "default"),
        Ok("work".into()),
        "the turn after the grant did not start on the granted login"
    );
    assert_eq!(spawn_on(&world, "s2", "claude", "work"), Ok("work".into()));
    for (session, provider, login) in [
        ("s2", "claude", "other"),
        ("s2", "codex", "default"),
        ("s1", "claude", "default"),
    ] {
        assert_eq!(
            spawn_on(&world, session, provider, login),
            Ok(login.into()),
            "the grant moved {session}'s {provider} child off the login {login}"
        );
    }
    world.jump_wall(3 * HOUR_MS - 1);
    assert_eq!(
        spawn_on(&world, "s2", "claude", "default"),
        Ok("work".into()),
        "the 3 h grant ended before 3 h"
    );
    world.jump_wall(1);
    let ended = spawn_on(&world, "s2", "claude", "work").unwrap_err();
    assert_eq!(
        ended,
        format!(
            "route_grant_expired: the grant to the claude login work ended at {}",
            t0 + 3 * HOUR_MS
        ),
        "the first turn after 3 h did not stop with route_grant_expired"
    );
    assert_eq!(
        spawn_on(&world, "s2", "claude", "default"),
        Ok("default".into()),
        "a session sent back to its first login did not start there"
    );

    #[cfg(unix)]
    spawn::a_grant_moves_the_child_and_its_end_stops_the_turn();
}

#[cfg(unix)]
mod spawn {
    use std::path::{Path, PathBuf};

    use tauri::test::MockRuntime;
    use tauri::Manager;

    use super::super::agent_sandbox::{mock_app, script, wait_for};
    use super::{grant, work_login, RouteGrant, THIRTY_DAYS_MS};
    use crate::session_store::{now_millis, SessionStore};
    use crate::spaces::{account, grants, HOOKED_SPAWNS};

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn hooked() -> u32 {
        HOOKED_SPAWNS.with(std::cell::Cell::get)
    }

    /// `harness_spawn` on Tauri's mock runtime with a stand-in fx that writes
    /// the folder it runs in, its arguments and its Claude config folder.
    struct Rig {
        root: PathBuf,
        fx: PathBuf,
        app: tauri::App<MockRuntime>,
    }

    impl Rig {
        fn new() -> Self {
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
            Self { root, fx, app }
        }

        fn folder(&self, name: &str) -> PathBuf {
            let folder = self.root.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            folder
        }

        fn spawn(&self, session: &str, workdir: &Path, login: &str) -> Result<u32, String> {
            let account = serde_json::json!({"provider": "claude", "id": login});
            crate::harness::harness_spawn(
                self.app.handle().clone(),
                self.app.state::<crate::harness::HarnessHost>(),
                session.into(),
                text(&self.fx),
                vec!["acp".into()],
                text(workdir),
                Some(serde_json::from_value(account).unwrap()),
                Some("fx".into()),
                Some(text(&self.fx)),
                None,
                None,
                None,
            )
        }

        /// What the stand-in CLI wrote when it ran in `workdir` with `login`.
        fn ran_with(&self, workdir: &Path, login: &str) -> String {
            let config =
                crate::harness::provider_account_path(self.app.handle(), "claude", login).unwrap();
            format!("{}\nacp\n{}\n", text(workdir), text(&config))
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn the_hook_keeps_the_folder_and_login() {
        let rig = Rig::new();
        let before = hooked();
        let workdir = rig.folder("project");
        rig.spawn("hooked", &workdir, "work").unwrap();
        assert_eq!(
            hooked(),
            before + 1,
            "harness_spawn started a child without the spawn hook, or ran it twice"
        );
        assert_eq!(
            wait_for(&workdir.join("ran")),
            rig.ran_with(&workdir, "work"),
            "the CLI started in another folder, with other arguments or another login"
        );
        let missing = rig.root.join("missing");
        assert_eq!(
            rig.spawn("lost", &missing, "work").unwrap_err(),
            format!("Working directory does not exist: {}", text(&missing))
        );
        assert_eq!(
            hooked(),
            before + 2,
            "the folder check ran before the spawn hook"
        );
    }

    /// `harness_spawn` reads the real clock, so the grants sit around it.
    pub(super) fn a_grant_moves_the_child_and_its_end_stops_the_turn() {
        let rig = Rig::new();
        let store = rig.app.state::<SessionStore>();
        account::save_account(&store, &work_login(), 0).unwrap();
        let granted = |granted_at| RouteGrant {
            session_id: "granted".into(),
            ..grant(granted_at)
        };
        grants::save_grant(&store, &granted(now_millis())).unwrap();
        let second = rig.folder("second-turn");
        rig.spawn("granted", &second, "default").unwrap();
        assert_eq!(
            wait_for(&second.join("ran")),
            rig.ran_with(&second, "work"),
            "the turn after the grant did not start with the granted login's folder"
        );
        grants::save_grant(&store, &granted(now_millis() - THIRTY_DAYS_MS - 1)).unwrap();
        let after = rig.folder("after-expiry");
        let error = rig.spawn("granted", &after, "work").unwrap_err();
        assert!(
            error.starts_with("route_grant_expired: "),
            "the first turn after the grant ended did not stop with route_grant_expired: {error}"
        );
        assert!(
            !after.join("ran").exists(),
            "the CLI started after the grant ended"
        );
    }
}
