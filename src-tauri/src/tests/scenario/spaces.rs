//! Spaces foundation (ADE-168). The app opens a database written before
//! Spaces existed, migrates it, and every Mono and session lands in Space 1;
//! a restart migrates again as a no-op. The Space, settings, project, login,
//! grant and lease storage round-trips, survives a restart and refuses what
//! the rest of the app cannot use. Then `harness_spawn` on Tauri's mock
//! runtime starts a stand-in CLI through the spawn hook with the folder and
//! login it was given. With `MONOCODE_SCENARIO_DB` it runs on a copy of a
//! real database.

use rusqlite::{params, Connection};

use super::world::World;
use crate::mono_roster::MonoRole;
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, GrantLength, GrantRequest, RouteGrant};
use crate::spaces::leases::{self, CheckoutLease};
use crate::spaces::{
    self, ProjectLogin, Provider, SettingsPatch, Space, SpaceSettings, FIRST_SPACE,
};

/// A grant until a monthly limit resets: longer than any weekly cap.
const THIRTY_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const EIGHT_DAYS_MS: i64 = 8 * 24 * 60 * 60 * 1000;

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

fn spaces_settings_and_projects_store_what_they_are_given(world: &World) {
    let (store, now) = (world.store(), world.now());
    let two = spaces::create(store, "  Client ", None, now).unwrap();
    assert_eq!(
        (two.id.as_str(), two.name.as_str()),
        ("space-2", "Client"),
        "the next Space number, its name trimmed"
    );
    assert!(
        spaces::create(store, "   ", None, now).is_err(),
        "a Space with no name"
    );
    spaces::update(store, "space-2", "Space 2", Some("#3366ff")).unwrap();
    assert_eq!(
        spaces::update(store, "space-9", "Nobody", None),
        Err("No Space space-9".into())
    );
    let patch = SettingsPatch {
        active_space_id: Some("space-2".into()),
        route_grant_ttl_ms: Some(60_000),
    };
    spaces::set_settings(store, &patch).unwrap();
    for (bad, defect) in [
        (
            SettingsPatch {
                active_space_id: Some("space-9".into()),
                ..SettingsPatch::default()
            },
            "a Space that does not exist became active",
        ),
        (
            SettingsPatch {
                route_grant_ttl_ms: Some(EIGHT_DAYS_MS),
                ..SettingsPatch::default()
            },
            "the route-grant TTL passed the 7 day cap",
        ),
        (
            SettingsPatch {
                route_grant_ttl_ms: Some(0),
                ..SettingsPatch::default()
            },
            "a route-grant TTL of 0",
        ),
        (
            SettingsPatch {
                route_grant_ttl_ms: Some(-1),
                ..SettingsPatch::default()
            },
            "a negative route-grant TTL",
        ),
    ] {
        assert!(spaces::set_settings(store, &bad).is_err(), "{defect}");
    }
    assert_eq!(
        spaces::project_space(store, "/work/c").unwrap(),
        FIRST_SPACE,
        "a folder first used after the migration is outside Space 1"
    );
    spaces::set_project_space(store, "/work/b/", "space-2").unwrap();
    for relative in ["work/b", "http://env/home/dev/repo"] {
        assert_eq!(
            spaces::set_project_space(store, relative, "space-2"),
            Err(format!("Not an absolute folder: {relative}"))
        );
    }
    assert_eq!(
        spaces::project_space(store, REMOTE).unwrap(),
        FIRST_SPACE,
        "a remote project with no row is outside Space 1"
    );
    spaces::set_project_space(store, &format!("{REMOTE}/"), "space-2").unwrap();
    spaces::set_project_space(store, "C:\\Work\\A\\", FIRST_SPACE).unwrap();
    spaces::set_project_space(store, "c:/work/a", "space-2").unwrap();
}

fn spaces_settings_and_projects_hold(world: &World, at: i64) {
    let store = world.store();
    assert_eq!(
        spaces::list(store).unwrap()[1],
        Space {
            id: "space-2".into(),
            name: "Space 2".into(),
            color: Some("#3366ff".into()),
            created_at: at
        }
    );
    assert_eq!(
        spaces::settings(store).unwrap(),
        SpaceSettings {
            active_space_id: "space-2".into(),
            route_grant_ttl_ms: 60_000
        }
    );
    for (folder, space) in [
        ("/work/a", FIRST_SPACE),
        ("/work/b", "space-2"),
        (REMOTE, "space-2"),
        ("C:/Work/A", "space-2"),
    ] {
        assert_eq!(
            spaces::project_space(store, folder).unwrap(),
            space,
            "{folder} is in another Space"
        );
    }
}

/// A remote project's key (`remote://<host>/<path>`).
const REMOTE: &str = "remote://env/home/dev/repo";

fn project_login(project: &str) -> ProjectLogin {
    ProjectLogin {
        space_id: FIRST_SPACE.into(),
        project_path: project.into(),
        provider: Provider::Claude,
        account_id: "default".into(),
    }
}

fn claude_in(world: &World, space: &str, project: &str) -> Option<String> {
    spaces::project_login(world.store(), space, project, Provider::Claude).unwrap()
}

fn a_project_login_overrides_the_space_login(world: &World) {
    let store = world.store();
    assert_eq!(
        spaces::project_logins(store, "space-2").unwrap(),
        [],
        "a Space with no project choices lists some"
    );
    assert_eq!(
        claude_in(world, FIRST_SPACE, "/work/a").as_deref(),
        Some("work"),
        "a project without its own choice does not use the Space's login"
    );
    let repick = ProjectLogin {
        account_id: "work".into(),
        ..project_login("/work/a")
    };
    spaces::set_project_login(store, &repick).unwrap();
    spaces::set_project_login(store, &project_login("/work/a/")).unwrap();
    assert_eq!(
        claude_in(world, FIRST_SPACE, "/work/a").as_deref(),
        Some("default"),
        "a re-pick of a project's login was lost"
    );
    assert_eq!(
        claude_in(world, "space-2", "/work/a"),
        None,
        "a Space 1 project choice leaked into Space 2"
    );
    assert_eq!(
        spaces::project_login(store, FIRST_SPACE, "/work/a", Provider::Codex).unwrap(),
        None,
        "a Claude project choice leaked to Codex"
    );
    spaces::set_project_login(store, &project_login("~")).unwrap();
    let remote = ProjectLogin {
        space_id: "space-2".into(),
        account_id: "work".into(),
        ..project_login(&format!("{REMOTE}/"))
    };
    spaces::set_project_login(store, &remote).unwrap();
    let two = ProjectLogin {
        space_id: "space-2".into(),
        project_path: "/work/b".into(),
        account_id: "work".into(),
        ..project_login("/work/b")
    };
    spaces::set_project_login(store, &two).unwrap();
    let stranger = ProjectLogin {
        account_id: "nobody".into(),
        ..project_login("/work/c")
    };
    assert!(
        spaces::set_project_login(store, &stranger).is_err(),
        "a project took a login that does not exist"
    );
    assert_eq!(
        spaces::project_login(store, FIRST_SPACE, "/work/c", Provider::Codex).unwrap(),
        None
    );
}

fn project_logins_hold(world: &World) {
    assert_eq!(
        claude_in(world, FIRST_SPACE, "/work/a").as_deref(),
        Some("default"),
        "the project's own choice did not override the Space's login"
    );
    assert_eq!(
        claude_in(world, "space-2", "/work/b").as_deref(),
        Some("work")
    );
    assert_eq!(
        claude_in(world, FIRST_SPACE, "~").as_deref(),
        Some("default"),
        "the no-project choice was lost"
    );
    assert_eq!(
        claude_in(world, "space-2", REMOTE).as_deref(),
        Some("work"),
        "a remote project's login was lost"
    );
    assert_eq!(
        spaces::project_logins(world.store(), FIRST_SPACE).unwrap(),
        [project_login("/work/a"), project_login("~")],
        "Space 1's project choices are missing, out of order, or include Space 2's"
    );
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
    let six_hours = request(GrantLength::SixHours, None);
    assert_eq!(
        (account.unwrap(), login.unwrap(), route.unwrap()),
        (work_login(), space_one_login(), six_hours),
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
    spaces::clear_project_login(store, FIRST_SPACE, "/work/a", Provider::Claude).unwrap();
    assert_eq!(
        claude_in(world, FIRST_SPACE, "/work/a").as_deref(),
        Some("work"),
        "a cleared project choice still overrides the Space's login"
    );
    assert_eq!(
        account::remove_account(store, Provider::Claude, "default"),
        Err("The default provider account cannot be removed".into()),
        "removed a CLI's own login"
    );
    account::remove_account(store, Provider::Claude, "work").unwrap();
    assert_eq!(account::logins(store).unwrap(), []);
    assert_eq!(
        claude_in(world, "space-2", "/work/b"),
        None,
        "a project kept a removed login"
    );
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
/// reseeds or duplicates rows; a Space, the settings or a project's Space is
/// lost or altered, a blank name, a missing active Space, a TTL outside 1 ms
/// to 7 days, or a relative or http project path is stored, a remote or
/// Windows project is refused or keyed otherwise than `pathKey()`, or a
/// project with no row is outside Space 1; a project's own login does not
/// override the Space's, is lost on a re-pick, leaks to another Space or
/// provider, survives being cleared, or outlives its login; a login, grant or lease is lost or altered,
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

    let at = world.now();
    spaces_settings_and_projects_store_what_they_are_given(&world);
    logins_grants_and_leases_store_what_they_are_given(&world);
    a_project_login_overrides_the_space_login(&world);
    world.down(60_000);
    spaces_settings_and_projects_hold(&world, at);
    logins_grants_and_leases_hold(&world, at);
    project_logins_hold(&world);
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

/// The grant `s2` holds from `granted_at` until `expires_at`.
fn grant_between(granted_at: i64, expires_at: i64) -> RouteGrant {
    RouteGrant {
        granted_at,
        expires_at,
        ..grant(granted_at)
    }
}

/// `check_turn` gives the TS `routeGrantEnded` answer and the message
/// `expiredRouteGrant` parses at the same times (`routeGrant.fixtures.json`).
fn check_turn_matches_the_ts_decision_on_the_shared_fixture(world: &World) {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../src/features/monos/model/routeGrant.fixtures.json"
    ))
    .unwrap();
    let at = |key: &str| fixture["grant"][key].as_i64().unwrap();
    let pinned = grant_between(at("grantedAt"), at("expiresAt"));
    assert_eq!(
        (
            fixture["grant"]["fromAccountId"].as_str(),
            fixture["grant"]["toAccountId"].as_str()
        ),
        (
            Some(pinned.from_account_id.as_str()),
            Some(pinned.to_account_id.as_str())
        )
    );
    for turn in fixture["turns"].as_array().unwrap() {
        let (now, decision) = (turn[0].as_i64().unwrap(), turn[1].as_str().unwrap());
        grants::save_grant(world.store(), &pinned).unwrap();
        let expected = match decision {
            "route" => Ok(()),
            _ => Err(fixture["expired"].as_str().unwrap().to_owned()),
        };
        assert_eq!(
            grants::check_turn(
                world.store(),
                &crate::harness::HarnessHost::new(),
                "s2",
                now
            ),
            expected,
            "check_turn at {now} disagrees with the TS routeGrantEnded ({decision})"
        );
    }
    grants::remove_grant(world.store(), "s2").unwrap();
}

/// The grant `s2`'s turns and spawns see at the World's time.
fn held(world: &World) -> Option<RouteGrant> {
    grants::grant(world.store(), "s2").unwrap()
}

/// The writer at `t0`: each card length, the reset choice and its refusals.
fn the_writer_takes_only_the_cards_lengths(world: &World, t0: i64) -> i64 {
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
        assert_eq!(held(world), Some(grant));
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
        held(world).map(|grant| grant.expires_at),
        Some(in_twenty_days),
        "a refused request replaced the grant"
    );
    in_twenty_days
}

/// A 3 h grant picked at `t0`: turns run until 3 h; the first turn at 3 h is
/// refused once, the grant goes and its child stops; the next turn starts
/// fresh on the session's own login.
fn the_first_turn_after_the_grant_is_refused_once(world: &mut World, t0: i64, picked: RouteGrant) {
    let first_login = picked.from_account_id.clone();
    let children = crate::harness::HarnessHost::new();
    let turn = |world: &World| grants::check_turn(world.store(), &children, "s2", world.now());
    assert_eq!(turn(world), Ok(()), "a turn under the grant was refused");
    assert_eq!(
        spawn_on(world, "s2", "claude", "work"),
        Ok("work".into()),
        "the routing's own respawn on the granted login was refused or rewritten"
    );
    for (session, login) in [("monocode-claude-text", "default"), ("s1", "default")] {
        assert_eq!(spawn_on(world, session, "claude", login), Ok(login.into()));
    }
    assert_eq!(
        held(world),
        Some(picked),
        "a helper child, another session or the routing's own respawn ended the grant"
    );
    world.jump_wall(3 * HOUR_MS - 1);
    assert_eq!(turn(world), Ok(()), "the 3 h grant ended before 3 h");
    #[cfg(unix)]
    let mut child = children.insert_test_child("s2");
    world.jump_wall(1);
    let refused = turn(world).unwrap_err();
    assert_eq!(
        refused,
        format!(
            "route_grant_expired: the grant from default to work granted at {t0} ended at {}",
            t0 + 3 * HOUR_MS
        ),
        "the first turn after 3 h was sent"
    );
    assert_eq!(
        held(world),
        None,
        "the refused grant stayed, so it blocks its login again"
    );
    #[cfg(unix)]
    assert!(
        crate::harness::tests::exits_soon(&mut child),
        "the child on the granted login kept running"
    );
    assert_eq!(
        turn(world),
        Ok(()),
        "the turn after the stop was refused again"
    );
    // The login the session goes back to, as `leaveExpiredRouteGrant` reads
    // it from this refusal (`expiredRouteGrant`, pinned by the fixture).
    let back_to = refused.split(' ').nth(4).unwrap();
    assert_eq!(
        back_to, first_login,
        "the refusal does not send the session back to its first login"
    );
    assert_eq!(
        spawn_on(world, "s2", "claude", back_to),
        Ok(first_login),
        "the next turn did not start fresh on the session's own login"
    );
}

/// The user's own login pick ends a live grant, and the ended grant never
/// blocks the granted login.
fn the_users_switch_ends_the_grant(world: &mut World) {
    let now = world.now();
    grants::write_grant(world.store(), &request(GrantLength::OneHour, None), now).unwrap();
    assert_eq!(
        spawn_on(world, "s2", "claude", "default"),
        Ok("default".into()),
        "the user's own pick of a login was rewritten"
    );
    assert_eq!(
        held(world),
        None,
        "the user's login switch did not end the grant"
    );
    world.jump_wall(2 * HOUR_MS);
    let children = crate::harness::HarnessHost::new();
    assert_eq!(
        (
            grants::check_turn(world.store(), &children, "s2", world.now()),
            spawn_on(world, "s2", "claude", "work")
        ),
        (Ok(()), Ok("work".into())),
        "the ended grant blocked the granted login"
    );
}

/// An expired grant the per-turn check has not seen: it moves no child, and
/// the spawn hook refuses the granted login once, then removes it.
fn an_expired_grant_is_inert_after_one_stop(world: &World) {
    let now = world.now();
    grants::save_grant(world.store(), &grant_between(now - HOUR_MS, now - 1)).unwrap();
    assert_eq!(
        spawn_on(world, "s2", "claude", "default"),
        Ok("default".into()),
        "an expired grant moved the child"
    );
    assert_eq!(held(world), None, "a switch away kept the expired grant");
    grants::save_grant(world.store(), &grant_between(now - HOUR_MS, now - 1)).unwrap();
    assert!(
        spawn_on(world, "s2", "claude", "work")
            .is_err_and(|error| error.starts_with("route_grant_expired: ")),
        "a spawn on the granted login after the grant ended started"
    );
    assert_eq!(
        (held(world), spawn_on(world, "s2", "claude", "work")),
        (None, Ok("work".into())),
        "the spawn hook's stop kept the grant, so it blocks the login again"
    );
}

/// Defect: a grant lasts another length than the one picked (the Settings
/// 6 h for a 3 h pick, a wrong hour count, the first login's reset for a fixed
/// length); "until the first login resets" guesses a missing reset, takes a
/// past one or caps a far one; a refused request still replaces the grant; a
/// turn (also one on a live child, through `control_authorize_turn`) is sent
/// after the grant ends, a millisecond early or late, or keeps its child or
/// the grant; the turn after that stop is refused again or does not start
/// fresh on the session's own login; a helper child, another session or the
/// routing's own respawn ends the grant, or the user's own login switch does
/// not; the spawn hook rewrites a login, lets an ended grant start on the
/// granted login, or lets an ended grant block it again; or `check_turn` and
/// the TS `routeGrantEnded` disagree on the same times.
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
    let in_twenty_days = the_writer_takes_only_the_cards_lengths(&world, t0);
    let picked = grants::write_grant(
        world.store(),
        &request(GrantLength::ThreeHours, Some(in_twenty_days)),
        t0,
    )
    .unwrap();
    assert_eq!(
        picked.expires_at,
        t0 + 3 * HOUR_MS,
        "the 3 h pick took another length"
    );
    the_first_turn_after_the_grant_is_refused_once(&mut world, t0, picked);
    the_users_switch_ends_the_grant(&mut world);
    an_expired_grant_is_inert_after_one_stop(&world);
    check_turn_matches_the_ts_decision_on_the_shared_fixture(&world);

    #[cfg(unix)]
    {
        spawn::every_turn_checks_the_grant(&world);
        spawn::the_granted_login_starts_and_an_ended_grant_stops_it();
    }
}

/// Defect: `spaces_grant_save`, the command the usage-limit card calls,
/// returns without storing, stores another session, login or length than
/// the request's, or stores a request its writer refuses.
#[cfg(not(windows))]
#[test]
fn the_grant_command_stores_what_the_card_sends() {
    use tauri::Manager;

    let app = tauri::test::mock_builder()
        .manage(crate::session_store::SessionStore::open_in_memory().unwrap())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let store = || app.state::<crate::session_store::SessionStore>();
    account::save_account(&store(), &work_login(), 0).unwrap();
    let saved = grants::spaces_grant_save(store(), request(GrantLength::OneHour, None)).unwrap();
    assert_eq!(
        (
            saved.session_id.as_str(),
            saved.from_account_id.as_str(),
            saved.to_account_id.as_str(),
            saved.expires_at - saved.granted_at
        ),
        ("s2", "default", "work", HOUR_MS),
        "the command returned another session, login or length than the request's"
    );
    let held = || grants::spaces_grant_get(store(), "s2".into()).unwrap();
    assert_eq!(
        held(),
        Some(saved.clone()),
        "the command did not store the grant it returned"
    );
    assert_eq!(
        grants::spaces_grant_save(store(), request(GrantLength::UntilReset, None)),
        Err("The provider reported no reset time for the first login; pick a length".into()),
        "the command took \"until the first login resets\" with no reset"
    );
    assert_eq!(
        held(),
        Some(saved),
        "a refused request replaced the stored grant"
    );
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

    /// `control_authorize_turn`, which `sendHarnessTurn` calls before every
    /// turn, also one on a live child, through the app's IPC. It reads the
    /// real clock, so the grants sit around it.
    pub(super) fn every_turn_checks_the_grant(world: &super::World) {
        let app = tauri::test::mock_builder()
            .manage(SessionStore::open(world.path().to_path_buf()).unwrap())
            .manage(crate::harness::HarnessHost::new())
            .invoke_handler(tauri::generate_handler![
                crate::control::control_authorize_turn
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        crate::control::init(app.handle()).unwrap();
        let main = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
            .build()
            .unwrap();
        let cwd = world.path().parent().unwrap().to_str().unwrap().to_owned();
        let turn = |turn_id: &str| {
            let args = serde_json::json!(
                {"sessionId": "s2", "cwd": cwd, "appAccess": false, "turnId": turn_id}
            );
            crate::mono_messages::tests::invoke_command(&main, "control_authorize_turn", &args)
        };
        let store = app.state::<SessionStore>();
        let now = now_millis();
        grants::save_grant(&store, &grant(now)).unwrap();
        assert!(turn("s2-t1").is_ok(), "a turn under the grant was refused");
        grants::save_grant(&store, &grant(now - THIRTY_DAYS_MS - 1)).unwrap();
        let refused = turn("s2-t2").unwrap_err();
        assert!(
            refused
                .as_str()
                .is_some_and(|error| error.starts_with("route_grant_expired: ")),
            "a turn sent after the grant ended was authorized: {refused}"
        );
        assert_eq!(grants::grant(&store, "s2").unwrap(), None);
    }

    /// `harness_spawn` reads the real clock, so the grants sit around it.
    pub(super) fn the_granted_login_starts_and_an_ended_grant_stops_it() {
        let rig = Rig::new();
        let store = rig.app.state::<SessionStore>();
        account::save_account(&store, &work_login(), 0).unwrap();
        let granted = |granted_at| RouteGrant {
            session_id: "granted".into(),
            ..grant(granted_at)
        };
        grants::save_grant(&store, &granted(now_millis())).unwrap();
        let second = rig.folder("second-turn");
        rig.spawn("granted", &second, "work").unwrap();
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
