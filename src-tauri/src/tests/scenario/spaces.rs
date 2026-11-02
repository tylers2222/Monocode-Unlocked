//! Spaces foundation (ADE-168). The app opens a database written before
//! Spaces existed, migrates it, and every Mono and session lands in Space 1;
//! a restart migrates again as a no-op. With `MONOCODE_SCENARIO_DB` it runs
//! on a copy of a real database.

use rusqlite::{params, Connection};

use super::world::World;
use crate::mono_roster::MonoRole;

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

/// Defect: a Mono or session from before Spaces, or one written after the
/// migration, lands outside Space 1; the migration writes a project row in
/// another form than the one rule (no row = Space 1); the schema caps a
/// grant's length or stores one that ends as it starts; the seeds miss Space 1,
/// the 6 hour TTL or a CLI's default login; or a restart migrates again and
/// reseeds or duplicates rows.
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
}
