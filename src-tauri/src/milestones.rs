use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::session_store::{now_millis, SessionStore};

pub(crate) const CHANGED: &str = "monocode:milestones-changed";

const TITLE_MAX: usize = 200;
const NOTE_MAX: usize = 4000;
const UNKNOWN: &str = "unknown_milestone";
const STALE_ORDER: &str = "stale_order";

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS milestones (
           id TEXT PRIMARY KEY CHECK (length(id) = 36),
           project_path TEXT NOT NULL
             CHECK (project_path = '/' OR (project_path <> '' AND project_path NOT GLOB '*/')),
           title TEXT NOT NULL CHECK (length(trim(title)) BETWEEN 1 AND 200),
           note TEXT NOT NULL DEFAULT '' CHECK (length(note) <= 4000),
           status TEXT NOT NULL CHECK (status IN ('idea', 'planned', 'in_progress', 'done')),
           target TEXT CHECK (target IS NULL OR (
             target GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]' AND date(target) = target)),
           sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
           created_by TEXT NOT NULL CHECK (
             created_by = 'user' OR created_by GLOB 'mono:?*' OR created_by GLOB 'session:?*'),
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS milestones_project_order
           ON milestones (project_path, sort_order);",
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Idea,
    Planned,
    InProgress,
    Done,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idea => "idea",
            Self::Planned => "planned",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Idea, Self::Planned, Self::InProgress, Self::Done]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub id: String,
    pub project_path: String,
    pub title: String,
    pub note: String,
    pub status: Status,
    pub target: Option<String>,
    pub order: i64,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewMilestone {
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub target: Option<String>,
    pub created_by: String,
}

/// Absent fields stay as they are; `target: null` clears the date.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MilestonePatch {
    pub title: Option<String>,
    pub note: Option<String>,
    pub status: Option<Status>,
    #[serde(default, deserialize_with = "present")]
    pub target: Option<Option<String>>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::deserialize(d).map(Some)
}

fn require(ok: bool, reason: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(format!("invalid_input: {reason}"))
    }
}

/// Paths arrive normalized by `normalizeProjectPath` (recents.ts), as sessions do.
fn check_project(path: &str) -> Result<(), String> {
    let trimmed = path == "/" || (!path.is_empty() && !path.ends_with('/'));
    require(
        trimmed && !path.contains('\\'),
        "project path must be normalized",
    )
}

fn check_title(title: &str) -> Result<String, String> {
    let title = title.trim();
    let length = title.chars().count();
    require(
        (1..=TITLE_MAX).contains(&length),
        "title must be 1 to 200 characters",
    )?;
    Ok(title.to_string())
}

fn check_note(note: &str) -> Result<(), String> {
    let ok = note.chars().count() <= NOTE_MAX;
    require(ok, "note must be 4000 characters or fewer")
}

fn check_target(target: Option<&str>) -> Result<(), String> {
    let ok = target.is_none_or(|target| parse_date(target).is_some());
    require(ok, "target must be a real YYYY-MM-DD date")
}

fn parse_date(value: &str) -> Option<time::Date> {
    let shaped = value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    let month = time::Month::try_from(value[5..7].parse::<u8>().ok()?).ok()?;
    time::Date::from_calendar_date(value[..4].parse().ok()?, month, value[8..].parse().ok()?).ok()
}

fn check_created_by(created_by: &str) -> Result<(), String> {
    let named = |prefix: &str| {
        created_by
            .strip_prefix(prefix)
            .is_some_and(|id| !id.is_empty())
    };
    let ok = created_by == "user" || named("mono:") || named("session:");
    require(ok, "createdBy must be user, mono:<id> or session:<id>")
}

const COLUMNS: &str =
    "id, project_path, title, note, status, target, sort_order, created_by, created_at, updated_at";

fn row_to_milestone(row: &rusqlite::Row<'_>) -> rusqlite::Result<Milestone> {
    let status: String = row.get(4)?;
    Ok(Milestone {
        id: row.get(0)?,
        project_path: row.get(1)?,
        title: row.get(2)?,
        note: row.get(3)?,
        status: Status::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                "invalid milestones.status".into(),
            )
        })?,
        target: row.get(5)?,
        order: row.get(6)?,
        created_by: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn get(conn: &Connection, project: &str, id: &str) -> Result<Milestone, String> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM milestones WHERE id = ?1 AND project_path = ?2"),
        params![id, project],
        row_to_milestone,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| UNKNOWN.to_string())
}

pub(crate) fn list(conn: &Connection, project: &str) -> Result<Vec<Milestone>, String> {
    check_project(project)?;
    conn.prepare(&format!(
        "SELECT {COLUMNS} FROM milestones WHERE project_path = ?1 ORDER BY sort_order, created_at"
    ))
    .and_then(|mut statement| statement.query_map([project], row_to_milestone)?.collect())
    .map_err(|e| e.to_string())
}

pub(crate) fn add(
    conn: &Connection,
    project: &str,
    input: NewMilestone,
    now: i64,
) -> Result<Milestone, String> {
    check_project(project)?;
    let title = check_title(&input.title)?;
    check_note(&input.note)?;
    check_target(input.target.as_deref())?;
    check_created_by(&input.created_by)?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO milestones
           (id, project_path, title, note, status, target, sort_order, created_by, created_at, updated_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, COALESCE(MAX(sort_order) + 1, 0), ?7, ?8, ?8
           FROM milestones WHERE project_path = ?2",
        params![
            id,
            project,
            title,
            input.note,
            input.status.as_str(),
            input.target,
            input.created_by,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    get(conn, project, &id)
}

pub(crate) fn update(
    conn: &Connection,
    project: &str,
    id: &str,
    patch: MilestonePatch,
    now: i64,
) -> Result<Milestone, String> {
    check_project(project)?;
    let current = get(conn, project, id)?;
    let title = check_title(patch.title.as_deref().unwrap_or(&current.title))?;
    let note = patch.note.unwrap_or(current.note);
    check_note(&note)?;
    let target = patch.target.unwrap_or(current.target);
    check_target(target.as_deref())?;
    let status = patch.status.unwrap_or(current.status);
    conn.execute(
        "UPDATE milestones SET title = ?3, note = ?4, status = ?5, target = ?6, updated_at = ?7
         WHERE id = ?1 AND project_path = ?2",
        params![id, project, title, note, status.as_str(), target, now],
    )
    .map_err(|e| e.to_string())?;
    get(conn, project, id)
}

pub(crate) fn remove(conn: &Connection, project: &str, id: &str) -> Result<(), String> {
    check_project(project)?;
    let removed = conn
        .execute(
            "DELETE FROM milestones WHERE id = ?1 AND project_path = ?2",
            params![id, project],
        )
        .map_err(|e| e.to_string())?;
    if removed == 0 {
        return Err(UNKNOWN.into());
    }
    Ok(())
}

/// `ids` must be exactly the project's ids, so a retried call converges.
pub(crate) fn reorder(
    conn: &Connection,
    project: &str,
    ids: &[String],
) -> Result<Vec<Milestone>, String> {
    let mut current: Vec<String> = list(conn, project)?.into_iter().map(|m| m.id).collect();
    let mut wanted = ids.to_vec();
    current.sort_unstable();
    wanted.sort_unstable();
    if current != wanted {
        return Err(STALE_ORDER.into());
    }
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for (order, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE milestones SET sort_order = ?3 WHERE id = ?1 AND project_path = ?2",
            params![id, project, order as i64],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    list(conn, project)
}

pub(crate) fn remove_project(conn: &Connection, project: &str) -> Result<(), String> {
    check_project(project)?;
    conn.execute("DELETE FROM milestones WHERE project_path = ?1", [project])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn emit_changed(app: &AppHandle, project: &str) {
    let _ = app.emit(CHANGED, json!({ "projectPath": project }));
}

#[tauri::command(async)]
pub fn milestones_list(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<Vec<Milestone>, String> {
    list(&*store.lock_conn()?, &project_path)
}

#[tauri::command(async)]
pub fn milestones_add(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    milestone: NewMilestone,
) -> Result<Milestone, String> {
    let added = add(&*store.lock_conn()?, &project_path, milestone, now_millis())?;
    emit_changed(&app, &project_path);
    Ok(added)
}

#[tauri::command(async)]
pub fn milestones_update(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    id: String,
    patch: MilestonePatch,
) -> Result<Milestone, String> {
    let updated = update(
        &*store.lock_conn()?,
        &project_path,
        &id,
        patch,
        now_millis(),
    )?;
    emit_changed(&app, &project_path);
    Ok(updated)
}

#[tauri::command(async)]
pub fn milestones_remove(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    id: String,
) -> Result<(), String> {
    remove(&*store.lock_conn()?, &project_path, &id)?;
    emit_changed(&app, &project_path);
    Ok(())
}

#[tauri::command(async)]
pub fn milestones_reorder(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    ids: Vec<String>,
) -> Result<Vec<Milestone>, String> {
    let listed = reorder(&*store.lock_conn()?, &project_path, &ids)?;
    emit_changed(&app, &project_path);
    Ok(listed)
}

#[tauri::command(async)]
pub fn milestones_remove_project(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<(), String> {
    remove_project(&*store.lock_conn()?, &project_path)?;
    emit_changed(&app, &project_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SessionStore {
        SessionStore::open_in_memory().unwrap()
    }

    fn new(title: &str) -> NewMilestone {
        NewMilestone {
            title: title.into(),
            note: String::new(),
            status: Status::Idea,
            target: None,
            created_by: "user".into(),
        }
    }

    fn titles(conn: &Connection, project: &str) -> Vec<String> {
        list(conn, project)
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect()
    }

    fn ids(conn: &Connection, project: &str) -> Vec<String> {
        list(conn, project)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    #[test]
    fn add_then_list_returns_rows_in_order() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        for title in ["a", "b", "c"] {
            add(&conn, "/p", new(title), 1).unwrap();
        }
        add(&conn, "/q", new("other project"), 1).unwrap();
        assert_eq!(titles(&conn, "/p"), ["a", "b", "c"]);
        assert_eq!(titles(&conn, "/q"), ["other project"]);

        let [a, b, c] = <[String; 3]>::try_from(ids(&conn, "/p")).unwrap();
        reorder(&conn, "/p", &[c, a, b]).unwrap();
        add(&conn, "/p", new("d"), 2).unwrap();
        assert_eq!(titles(&conn, "/p"), ["c", "a", "b", "d"]);
    }

    #[test]
    fn update_of_another_projects_id_is_unknown() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let mine = add(&conn, "/p", new("mine"), 1).unwrap();
        let patch = MilestonePatch {
            title: Some("stolen".into()),
            ..Default::default()
        };
        assert_eq!(update(&conn, "/q", &mine.id, patch, 2), Err(UNKNOWN.into()));
        assert_eq!(remove(&conn, "/q", &mine.id), Err(UNKNOWN.into()));
        assert_eq!(list(&conn, "/p").unwrap(), [mine]);
    }

    #[test]
    fn update_keeps_absent_fields_and_clears_target_on_null() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let mut input = new("ship");
        input.note = "kept".into();
        input.target = Some("2026-12-01".into());
        let added = add(&conn, "/p", input, 1).unwrap();

        let patch = serde_json::from_value(json!({ "status": "in_progress", "target": null }));
        let updated = update(&conn, "/p", &added.id, patch.unwrap(), 2).unwrap();
        assert_eq!(
            (updated.title.as_str(), updated.note.as_str()),
            ("ship", "kept")
        );
        assert_eq!(updated.status, Status::InProgress);
        assert_eq!(updated.target, None);
        assert_eq!(updated.updated_at, 2);
    }

    #[test]
    fn reorder_refuses_a_stale_id_set() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let a = add(&conn, "/p", new("a"), 1).unwrap().id;
        let b = add(&conn, "/p", new("b"), 1).unwrap().id;
        let foreign = add(&conn, "/q", new("x"), 1).unwrap().id;
        for stale in [
            vec![a.clone(), foreign.clone()],
            vec![a.clone()],
            vec![a.clone(), b.clone(), foreign.clone()],
            vec![a.clone(), a.clone()],
        ] {
            assert_eq!(reorder(&conn, "/p", &stale), Err(STALE_ORDER.into()));
        }
        assert_eq!(titles(&conn, "/p"), ["a", "b"]);
        assert_eq!(titles(&conn, "/q"), ["x"]);

        let once = reorder(&conn, "/p", &[b.clone(), a.clone()]).unwrap();
        assert_eq!(reorder(&conn, "/p", &[b, a]).unwrap(), once);
        let orders: Vec<i64> = once.iter().map(|m| m.order).collect();
        assert_eq!(orders, [0, 1]);
    }

    #[test]
    fn target_rejects_impossible_dates() {
        for bad in [
            "2026-02-30",
            "2025-02-29",
            "2026-13-01",
            "2026-2-03",
            "+026-02-03",
            "2026-02-03 ",
        ] {
            assert!(check_target(Some(bad)).is_err(), "{bad}");
        }
        for good in ["2024-02-29", "2026-12-31"] {
            assert_eq!(check_target(Some(good)), Ok(()), "{good}");
        }
        let store = store();
        let conn = store.lock_conn().unwrap();
        let mut input = new("dated");
        input.target = Some("2026-04-31".into());
        assert!(add(&conn, "/p", input, 1)
            .unwrap_err()
            .starts_with("invalid_input"));
        assert!(list(&conn, "/p").unwrap().is_empty());
    }

    #[test]
    fn checks_reject_bad_status_and_empty_title() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let insert = |column: &str, value: &str| {
            let mut row = [
                ("id", "00000000-0000-4000-8000-000000000000".to_string()),
                ("project_path", "/p".into()),
                ("title", "t".into()),
                ("note", String::new()),
                ("status", "idea".into()),
                ("target", "2026-01-01".into()),
                ("sort_order", "0".into()),
                ("created_by", "user".into()),
                ("created_at", "1".into()),
                ("updated_at", "1".into()),
            ];
            for (name, cell) in row.iter_mut() {
                if *name == column {
                    *cell = value.into();
                }
            }
            let names: Vec<&str> = row.iter().map(|(name, _)| *name).collect();
            let values: Vec<&String> = row.iter().map(|(_, cell)| cell).collect();
            conn.execute(
                &format!(
                    "INSERT INTO milestones ({}) VALUES ({})",
                    names.join(", "),
                    vec!["?"; names.len()].join(", ")
                ),
                rusqlite::params_from_iter(values),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        };
        let long_title = "t".repeat(201);
        let long_note = "n".repeat(4001);
        for (column, value) in [
            ("status", "doing"),
            ("title", "   "),
            ("title", long_title.as_str()),
            ("note", long_note.as_str()),
            ("target", "2026-02-30"),
            ("target", "later"),
            ("project_path", "/a/b/"),
            ("project_path", ""),
            ("created_by", "bot"),
            ("created_by", "mono:"),
            ("id", "short"),
            ("sort_order", "-1"),
        ] {
            let error = insert(column, value).unwrap_err();
            assert!(
                error.contains("CHECK constraint failed"),
                "{column}={value}: {error}"
            );
        }
        insert("status", "in_progress").unwrap();
    }

    #[test]
    fn remove_project_deletes_only_that_project() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        add(&conn, "/p", new("gone"), 1).unwrap();
        add(&conn, "/q", new("kept"), 1).unwrap();
        remove_project(&conn, "/p").unwrap();
        assert!(list(&conn, "/p").unwrap().is_empty());
        assert_eq!(titles(&conn, "/q"), ["kept"]);
    }
}
