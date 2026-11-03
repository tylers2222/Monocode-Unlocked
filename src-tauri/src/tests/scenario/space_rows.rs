//! The Space switcher's rows (ADE-179). The window shows only the active
//! Space, so every Mono and session it lists carries the Space its insert
//! stored: the roster (`monos_list`), a save's reply (`session_upsert`), a
//! project's sessions and the linked threads. Switching the active Space
//! only changes where new rows go; a row already saved keeps its Space
//! through later saves and a restart.

use serde::Serialize;
use serde_json::json;

use super::world::World;
use crate::mono_roster::{self, MonoCreate, MonoRole};
use crate::session_store::{self, SessionSummary, SessionUpsert};
use crate::spaces::{self, SettingsPatch, FIRST_SPACE};

const SPACE_2: &str = "space-2";
const PROJECT: &str = "/space-rows/project";

fn add_mono(world: &World, id: &str) {
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": id,
        "role": MonoRole::Mono,
        "data": {"name": id},
        "sortOrder": 0,
    }))
    .unwrap();
    let conn = world.conn();
    mono_roster::create(&conn, &mono_roster::validate_create(mono).unwrap()).unwrap();
}

/// A window save of session `id` in `PROJECT`, linked to a PR so the linked
/// list holds it too; the reply it gets.
fn save(world: &World, id: &str) -> SessionSummary {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": PROJECT,
        "harness": "codex",
        "model": "codex:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [{"id": "u1", "role": "user", "text": "Start"}],
        "linkedWorkItem": {"kind": "pr", "repo": "o/r", "number": 1, "url": "https://example.com/1"},
    }))
    .unwrap();
    session_store::upsert_session(&world.conn(), &session).unwrap()
}

fn switch_to(world: &World, space: &str) {
    let patch = SettingsPatch {
        active_space_id: Some(space.to_owned()),
        route_grant_ttl_ms: None,
    };
    spaces::set_settings(world.store(), &patch).unwrap();
}

/// `spaceId` as the window reads it from a row's JSON.
fn space_of(row: &impl Serialize) -> String {
    let value = serde_json::to_value(row).unwrap();
    value["spaceId"].as_str().unwrap_or("<none>").to_owned()
}

fn stored(world: &World, table: &str, id: &str) -> String {
    let sql = format!("SELECT space_id FROM {table} WHERE id = ?1");
    world
        .conn()
        .query_row(&sql, [id], |row| row.get(0))
        .unwrap()
}

/// Every listed row of `ids` as `id space`, from the roster and each session
/// list the window reads.
fn listed(world: &World, ids: &[&str]) -> Vec<String> {
    let conn = world.conn();
    let monos = mono_roster::list(&conn).unwrap();
    let by_project = session_store::list_by_project(&conn, PROJECT).unwrap();
    let linked = session_store::list_linked(&conn).unwrap();
    let mut rows: Vec<String> = monos
        .iter()
        .filter(|row| ids.contains(&row.id.as_str()))
        .map(|row| format!("mono {} {}", row.id, space_of(row)))
        .chain(
            by_project
                .iter()
                .filter(|row| ids.contains(&row.id.as_str()))
                .map(|row| format!("project-session {} {}", row.id, space_of(row))),
        )
        .chain(
            linked
                .iter()
                .filter(|row| ids.contains(&row.id.as_str()))
                .map(|row| format!("linked-session {} {}", row.id, space_of(row))),
        )
        .collect();
    rows.sort();
    rows
}

#[test]
fn each_listed_row_carries_its_saved_space() {
    let mut world = World::new("space-rows");
    let sql = "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 2', 0)";
    world.conn().execute(sql, [SPACE_2]).unwrap();
    let mut failed = Vec::new();

    // 1. Rows made while Space 1 is active are in Space 1, and say so.
    add_mono(&world, "rows-one");
    let reply = save(&world, "rows-s-one");
    if space_of(&reply) != FIRST_SPACE {
        failed.push(format!("1 Space 1 save replied {}", space_of(&reply)));
    }

    // 2. After a switch to Space 2, new rows are in Space 2, and say so.
    switch_to(&world, SPACE_2);
    add_mono(&world, "rows-two");
    let reply = save(&world, "rows-s-two");
    if space_of(&reply) != SPACE_2 {
        failed.push(format!("2 Space 2 save replied {}", space_of(&reply)));
    }

    let ids = ["rows-one", "rows-two", "rows-s-one", "rows-s-two"];
    let expected = vec![
        "linked-session rows-s-one space-1",
        "linked-session rows-s-two space-2",
        "mono rows-one space-1",
        "mono rows-two space-2",
        "project-session rows-s-one space-1",
        "project-session rows-s-two space-2",
    ];
    // 3. Every list the window reads gives each row its stored Space.
    if listed(&world, &ids) != expected {
        failed.push(format!(
            "3 lists after the switch: {:?}",
            listed(&world, &ids)
        ));
    }
    for (table, id) in [
        ("monos", "rows-one"),
        ("monos", "rows-two"),
        ("sessions", "rows-s-one"),
        ("sessions", "rows-s-two"),
    ] {
        let want = if id.ends_with("one") {
            FIRST_SPACE
        } else {
            SPACE_2
        };
        if stored(&world, table, id) != want {
            failed.push(format!(
                "3 {table} {id} stored {}",
                stored(&world, table, id)
            ));
        }
    }

    // 4. Switching back moves nothing: a later save of a Space 2 session
    // still replies Space 2, and every list is unchanged.
    switch_to(&world, FIRST_SPACE);
    let reply = save(&world, "rows-s-two");
    if space_of(&reply) != SPACE_2 {
        failed.push(format!(
            "4 re-save after switching back replied {}",
            space_of(&reply)
        ));
    }
    if listed(&world, &ids) != expected {
        failed.push(format!(
            "4 lists after switching back: {:?}",
            listed(&world, &ids)
        ));
    }

    // 5. A restart reads the same Spaces back.
    world.kill();
    world.restart();
    if listed(&world, &ids) != expected {
        failed.push(format!(
            "5 lists after a restart: {:?}",
            listed(&world, &ids)
        ));
    }

    assert!(
        failed.is_empty(),
        "space rows checkpoints failed:\n{}",
        failed.join("\n")
    );
}
