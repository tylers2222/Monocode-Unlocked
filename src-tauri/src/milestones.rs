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
const OFF: &str = "milestones_off";

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
           ON milestones (project_path, sort_order);
         CREATE TABLE IF NOT EXISTS milestone_settings (
           project_path TEXT PRIMARY KEY
             CHECK (project_path = '/' OR (project_path <> '' AND project_path NOT GLOB '*/')),
           enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
           hidden_statuses TEXT NOT NULL DEFAULT '[]'
             CHECK (json_valid(hidden_statuses) AND json_type(hidden_statuses) = 'array'),
           updated_at INTEGER NOT NULL
         );",
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// No row means on, nothing hidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MilestoneSettings {
    pub enabled: bool,
    pub hidden_statuses: Vec<Status>,
}

impl Default for MilestoneSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hidden_statuses: Vec::new(),
        }
    }
}

/// Absent fields stay as they are. Unknown status names fail to deserialize.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub enabled: Option<bool>,
    pub hidden_statuses: Option<Vec<Status>>,
}

#[derive(Debug, Serialize)]
pub struct ProjectMilestones {
    pub settings: MilestoneSettings,
    pub milestones: Vec<Milestone>,
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

/// `by` is `user` for the app, else the agent (`mono:<id>`, `session:<id>`).
/// The app always writes; agents write only while the project's milestones are on.
pub(crate) fn check_writer(conn: &Connection, project: &str, by: &str) -> Result<(), String> {
    check_created_by(by)?;
    if by != "user" && !settings(conn, project)?.enabled {
        return Err(OFF.into());
    }
    Ok(())
}

pub(crate) fn settings(conn: &Connection, project: &str) -> Result<MilestoneSettings, String> {
    check_project(project)?;
    let row: Option<(bool, String)> = conn
        .query_row(
            "SELECT enabled, hidden_statuses FROM milestone_settings WHERE project_path = ?1",
            [project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((enabled, hidden)) = row else {
        return Ok(MilestoneSettings::default());
    };
    let hidden_statuses = serde_json::from_str(&hidden)
        .map_err(|e| format!("invalid milestone_settings.hidden_statuses: {e}"))?;
    Ok(MilestoneSettings {
        enabled,
        hidden_statuses,
    })
}

pub(crate) fn set_settings(
    conn: &Connection,
    project: &str,
    patch: SettingsPatch,
    now: i64,
) -> Result<MilestoneSettings, String> {
    let current = settings(conn, project)?;
    let mut hidden_statuses = patch.hidden_statuses.unwrap_or(current.hidden_statuses);
    hidden_statuses.sort_unstable();
    hidden_statuses.dedup();
    let next = MilestoneSettings {
        enabled: patch.enabled.unwrap_or(current.enabled),
        hidden_statuses,
    };
    let hidden = serde_json::to_string(&next.hidden_statuses).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO milestone_settings (project_path, enabled, hidden_statuses, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (project_path) DO UPDATE SET
           enabled = excluded.enabled,
           hidden_statuses = excluded.hidden_statuses,
           updated_at = excluded.updated_at",
        params![project, next.enabled, hidden, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(next)
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
    check_writer(conn, project, &input.created_by)?;
    let title = check_title(&input.title)?;
    check_note(&input.note)?;
    check_target(input.target.as_deref())?;
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
    by: &str,
    now: i64,
) -> Result<Milestone, String> {
    check_project(project)?;
    check_writer(conn, project, by)?;
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

pub(crate) fn remove(conn: &Connection, project: &str, id: &str, by: &str) -> Result<(), String> {
    check_project(project)?;
    check_writer(conn, project, by)?;
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
    by: &str,
) -> Result<Vec<Milestone>, String> {
    check_writer(conn, project, by)?;
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

/// One transaction, so a removed project never keeps half its milestone data.
pub(crate) fn remove_project(conn: &Connection, project: &str) -> Result<(), String> {
    check_project(project)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for table in ["milestones", "milestone_settings"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE project_path = ?1"),
            [project],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn emit_changed(app: &AppHandle, project: &str) {
    let _ = app.emit(CHANGED, json!({ "projectPath": project }));
}

/// Never refused: an agent may read while milestones are off and sees `enabled: false`.
#[tauri::command(async)]
pub fn milestones_list(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<ProjectMilestones, String> {
    let conn = store.lock_conn()?;
    Ok(ProjectMilestones {
        settings: settings(&conn, &project_path)?,
        milestones: list(&conn, &project_path)?,
    })
}

#[tauri::command(async)]
pub fn milestone_settings_get(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<MilestoneSettings, String> {
    settings(&*store.lock_conn()?, &project_path)
}

#[tauri::command(async)]
pub fn milestone_settings_set(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    patch: SettingsPatch,
) -> Result<MilestoneSettings, String> {
    let saved = set_settings(&*store.lock_conn()?, &project_path, patch, now_millis())?;
    emit_changed(&app, &project_path);
    Ok(saved)
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
    by: String,
) -> Result<Milestone, String> {
    let updated = update(
        &*store.lock_conn()?,
        &project_path,
        &id,
        patch,
        &by,
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
    by: String,
) -> Result<(), String> {
    remove(&*store.lock_conn()?, &project_path, &id, &by)?;
    emit_changed(&app, &project_path);
    Ok(())
}

#[tauri::command(async)]
pub fn milestones_reorder(
    app: AppHandle,
    store: State<'_, SessionStore>,
    project_path: String,
    ids: Vec<String>,
    by: String,
) -> Result<Vec<Milestone>, String> {
    let listed = reorder(&*store.lock_conn()?, &project_path, &ids, &by)?;
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
        reorder(&conn, "/p", &[c, a, b], "user").unwrap();
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
        assert_eq!(
            update(&conn, "/q", &mine.id, patch, "user", 2),
            Err(UNKNOWN.into())
        );
        assert_eq!(remove(&conn, "/q", &mine.id, "user"), Err(UNKNOWN.into()));
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
        let updated = update(&conn, "/p", &added.id, patch.unwrap(), "user", 2).unwrap();
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
            assert_eq!(
                reorder(&conn, "/p", &stale, "user"),
                Err(STALE_ORDER.into())
            );
        }
        assert_eq!(titles(&conn, "/p"), ["a", "b"]);
        assert_eq!(titles(&conn, "/q"), ["x"]);

        let once = reorder(&conn, "/p", &[b.clone(), a.clone()], "user").unwrap();
        assert_eq!(reorder(&conn, "/p", &[b, a], "user").unwrap(), once);
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
        for project in ["/p", "/q"] {
            set_settings(&conn, project, turn(false), 1).unwrap();
        }
        remove_project(&conn, "/p").unwrap();
        assert!(list(&conn, "/p").unwrap().is_empty());
        assert_eq!(settings(&conn, "/p").unwrap(), MilestoneSettings::default());
        assert_eq!(titles(&conn, "/q"), ["kept"]);
        assert!(!settings(&conn, "/q").unwrap().enabled);
    }

    fn turn(enabled: bool) -> SettingsPatch {
        SettingsPatch {
            enabled: Some(enabled),
            ..Default::default()
        }
    }

    fn by(writer: &str, title: &str) -> NewMilestone {
        NewMilestone {
            created_by: writer.into(),
            ..new(title)
        }
    }

    #[test]
    fn settings_default_when_no_row() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let default = MilestoneSettings {
            enabled: true,
            hidden_statuses: vec![],
        };
        assert_eq!(settings(&conn, "/p").unwrap(), default);
        add(&conn, "/p", by("mono:a", "agent may write"), 1).unwrap();

        let hide: SettingsPatch =
            serde_json::from_value(json!({ "hiddenStatuses": ["done", "idea", "done"] })).unwrap();
        let hidden = set_settings(&conn, "/p", hide, 1).unwrap();
        assert_eq!(hidden.hidden_statuses, [Status::Idea, Status::Done]);
        assert!(hidden.enabled, "a hiddenStatuses-only patch keeps enabled");
        let off = set_settings(&conn, "/p", turn(false), 2).unwrap();
        assert_eq!(
            off.hidden_statuses,
            [Status::Idea, Status::Done],
            "an enabled-only patch keeps hiddenStatuses"
        );
        assert_eq!(settings(&conn, "/p").unwrap(), off);

        conn.execute("DELETE FROM milestone_settings", []).unwrap();
        assert_eq!(settings(&conn, "/p").unwrap(), default);
    }

    #[test]
    fn off_project_refuses_writes_but_lists() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let first = add(&conn, "/p", new("first"), 1).unwrap();
        let second = add(&conn, "/p", new("second"), 1).unwrap();
        set_settings(&conn, "/p", turn(false), 1).unwrap();
        let rows = list(&conn, "/p").unwrap();
        let reversed = [second.id.clone(), first.id.clone()];

        for agent in ["mono:alpha", "session:s1"] {
            let patch = MilestonePatch {
                title: Some("changed".into()),
                ..Default::default()
            };
            let refused = [
                ("add", add(&conn, "/p", by(agent, "new"), 2).map(|_| ())),
                (
                    "update",
                    update(&conn, "/p", &first.id, patch, agent, 2).map(|_| ()),
                ),
                ("remove", remove(&conn, "/p", &first.id, agent)),
                (
                    "reorder",
                    reorder(&conn, "/p", &reversed, agent).map(|_| ()),
                ),
            ];
            for (write, result) in refused {
                assert_eq!(result, Err(OFF.into()), "{agent} {write}");
            }
        }
        assert_eq!(
            list(&conn, "/p").unwrap(),
            rows,
            "a refused write changed rows"
        );
        assert!(!settings(&conn, "/p").unwrap().enabled);

        let patch = MilestonePatch {
            title: Some("by the user".into()),
            ..Default::default()
        };
        update(&conn, "/p", &first.id, patch, "user", 3).unwrap();
        reorder(&conn, "/p", &reversed, "user").unwrap();
        remove(&conn, "/p", &second.id, "user").unwrap();
        add(&conn, "/p", new("user add"), 3).unwrap();
        assert_eq!(titles(&conn, "/p"), ["by the user", "user add"]);

        set_settings(&conn, "/p", turn(true), 4).unwrap();
        add(&conn, "/p", by("mono:alpha", "back on"), 4).unwrap();
    }

    #[test]
    fn off_in_one_project_leaves_another_on() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        set_settings(&conn, "/p", turn(false), 1).unwrap();
        assert_eq!(
            add(&conn, "/p", by("mono:alpha", "refused"), 1).map(|_| ()),
            Err(OFF.into())
        );
        add(&conn, "/q", by("mono:alpha", "allowed"), 1).unwrap();
        assert_eq!(titles(&conn, "/q"), ["allowed"]);
        assert!(settings(&conn, "/q").unwrap().enabled);
    }

    #[test]
    fn hidden_only_patch_keeps_project_off() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        set_settings(&conn, "/p", turn(false), 1).unwrap();
        let hide: SettingsPatch =
            serde_json::from_value(json!({ "hiddenStatuses": ["done"] })).unwrap();
        let saved = set_settings(&conn, "/p", hide, 2).unwrap();
        let expected = MilestoneSettings {
            enabled: false,
            hidden_statuses: vec![Status::Done],
        };
        assert_eq!(saved, expected);
        assert_eq!(settings(&conn, "/p").unwrap(), expected);
        assert_eq!(
            add(&conn, "/p", by("mono:alpha", "refused"), 2).map(|_| ()),
            Err(OFF.into())
        );
    }

    #[test]
    fn hidden_statuses_rejects_unknown_status() {
        let unknown = json!({ "hiddenStatuses": ["idea", "doing"] });
        let error = serde_json::from_value::<SettingsPatch>(unknown).unwrap_err();
        assert!(
            error.to_string().contains("unknown variant `doing`"),
            "{error}"
        );

        let store = store();
        let conn = store.lock_conn().unwrap();
        conn.execute(
            "INSERT INTO milestone_settings (project_path, hidden_statuses, updated_at)
             VALUES ('/p', '[\"doing\"]', 1)",
            [],
        )
        .unwrap();
        let error = settings(&conn, "/p").unwrap_err();
        assert!(error.contains("hidden_statuses"), "{error}");
        assert!(add(&conn, "/p", by("mono:alpha", "x"), 1).is_err());
    }

    #[test]
    fn settings_checks_reject_bad_rows() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let insert = |path: &str, enabled: i64, hidden: &str| {
            conn.execute(
                "INSERT INTO milestone_settings (project_path, enabled, hidden_statuses, updated_at)
                 VALUES (?1, ?2, ?3, 1)",
                params![path, enabled, hidden],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        };
        for (path, enabled, hidden) in [
            ("/p", 1, "not json"),
            ("/p", 1, "{\"idea\": true}"),
            ("/p", 2, "[]"),
            ("/a/b/", 1, "[]"),
            ("", 1, "[]"),
        ] {
            let error = insert(path, enabled, hidden).unwrap_err();
            assert!(
                error.contains("CHECK constraint failed"),
                "{path} {enabled} {hidden}: {error}"
            );
        }
        insert("/", 0, "[\"done\"]").unwrap();
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM milestones", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn bad_input_is_invalid_input_and_writes_nothing() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        for project in ["/a/b/", "", "C:\\proj", "C:\\", "/a\\b"] {
            let error = add(&conn, project, new("x"), 1).unwrap_err();
            assert!(error.starts_with("invalid_input"), "{project:?}: {error}");
        }
        let long_title = "t".repeat(TITLE_MAX + 1);
        let long_note = "n".repeat(NOTE_MAX + 1);
        let bad_inputs = [
            (
                "blank title",
                json!({ "title": "   ", "createdBy": "user" }),
            ),
            (
                "long title",
                json!({ "title": long_title, "createdBy": "user" }),
            ),
            (
                "long note",
                json!({ "title": "t", "note": long_note, "createdBy": "user" }),
            ),
            (
                "mono: with no id",
                json!({ "title": "t", "createdBy": "mono:" }),
            ),
            (
                "session: with no id",
                json!({ "title": "t", "createdBy": "session:" }),
            ),
            (
                "unknown author",
                json!({ "title": "t", "createdBy": "bot" }),
            ),
        ];
        for (case, input) in bad_inputs {
            let milestone: NewMilestone = serde_json::from_value(input).unwrap();
            let error = add(&conn, "/p", milestone, 1).unwrap_err();
            assert!(error.starts_with("invalid_input"), "{case}: {error}");
        }
        assert_eq!(row_count(&conn), 0);

        let kept = add(&conn, "/p", new("kept"), 1).unwrap();
        for (case, patch) in [
            ("blank title", json!({ "title": "  " })),
            ("long note", json!({ "note": "n".repeat(NOTE_MAX + 1) })),
        ] {
            let parsed: MilestonePatch = serde_json::from_value(patch).unwrap();
            let error = update(&conn, "/p", &kept.id, parsed, "user", 2).unwrap_err();
            assert!(error.starts_with("invalid_input"), "update {case}: {error}");
        }
        assert_eq!(list(&conn, "/p").unwrap(), [kept]);
    }

    #[test]
    fn add_allows_the_root_path_and_stores_the_title_trimmed() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let added = add(&conn, "/", new("  ship  "), 1).unwrap();
        assert_eq!(added.title, "ship");
        assert_eq!(list(&conn, "/").unwrap(), [added]);
    }

    // M-9a1 real-state proof. Defect: the page is tested on rows the real
    // store never sends (a target shape the schema refuses, another field
    // name or order), so a display bug on real rows goes unseen. Every target
    // shape the real schema accepts goes in by SQL, comes out through `list`
    // and `settings` as `milestones_list` answers, and must equal the rows
    // tests/browser/milestones-dates.spec.ts renders. RECORD_MILESTONE_LIST=1
    // rewrites the file.
    #[test]
    fn list_reply_for_every_target_shape_matches_the_page_fixture() {
        let store = store();
        let conn = store.lock_conn().unwrap();
        let project = "/Users/test/orbit";
        let insert = |order: i64, status: &str, target: Option<&str>| {
            conn.execute(
                &format!(
                    "INSERT INTO milestones ({COLUMNS})
                     VALUES (printf('real-%031d', ?1), ?2, printf('Real row %d', ?1), '', ?3, ?4, ?1, 'user', ?1, ?1)"
                ),
                params![order, project, status, target],
            )
        };
        for refused in ["", "2026-1-5", "12/11/2026"] {
            let error = insert(99, "planned", Some(refused)).unwrap_err();
            assert!(
                error.to_string().contains("CHECK constraint failed"),
                "{refused:?}: {error}"
            );
        }
        let rows = [
            ("done", Some("2026-10-15")),
            ("done", None),
            ("in_progress", Some("2026-10-18")),
            ("in_progress", Some("2026-10-19")),
            ("planned", Some("2026-12-12")),
            ("planned", Some("2026-01-01")),
            ("planned", None),
            ("planned", Some("2027-01-09")),
            ("planned", Some("2025-01-05")),
            ("idea", Some("2028-02-29")),
            ("idea", Some("2026-12-31")),
        ];
        for (order, (status, target)) in rows.into_iter().enumerate() {
            insert(order as i64, status, target).unwrap();
        }
        let reply = ProjectMilestones {
            settings: settings(&conn, project).unwrap(),
            milestones: list(&conn, project).unwrap(),
        };
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/browser/fixtures/milestones-real-list.json"
        );
        if std::env::var_os("RECORD_MILESTONE_LIST").is_some() {
            let json = serde_json::to_string_pretty(&reply).unwrap();
            std::fs::write(path, format!("{json}\n")).unwrap();
        }
        // Values, not text: a Windows checkout gives the file CRLF line ends.
        let committed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&reply).unwrap(), committed);
    }

    // Defect: the page sends a write the real command refuses or misreads
    // ("" for Later, a field serde rejects, a patch that clears a field it
    // did not name, an add with the wrong writer). The calls are what
    // milestones-editor.spec.ts captured from the page; each goes through the
    // command's own argument types.
    #[test]
    fn page_writes_replay_on_the_real_store() {
        let calls: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../tests/browser/fixtures/milestones-page-writes.json"
        ))
        .unwrap();
        let store = store();
        let conn = store.lock_conn().unwrap();
        let project = "/Users/test/orbit";
        // The rows the spec seeds: ids "orbit-<order>" padded with 0 to 36.
        let mut ids = std::collections::HashMap::new();
        for order in 0..6 {
            let id = format!("{:0<36}", format!("orbit-{order}"));
            let (status, target) = match order {
                2 => ("planned", Some("2026-11-21")),
                4 => ("idea", None),
                _ => ("planned", None),
            };
            conn.execute(
                &format!(
                    "INSERT INTO milestones ({COLUMNS})
                     VALUES (?1, ?2, ?3, 'Before', ?4, ?5, ?6, 'user', 1, 1)"
                ),
                params![id, project, format!("Row {order}"), status, target, order],
            )
            .unwrap();
            ids.insert(id.clone(), id);
        }
        let mut history = Vec::new();
        for call in &calls {
            let arg = |name: &str| call["args"][name].clone();
            let path: String = serde_json::from_value(arg("projectPath")).unwrap();
            let row = match call["command"].as_str().unwrap() {
                "milestones_add" => {
                    let added = add(
                        &conn,
                        &path,
                        serde_json::from_value(arg("milestone")).unwrap(),
                        2,
                    )
                    .unwrap();
                    assert_eq!(added.created_by, "user");
                    assert_eq!(added.order, ids.len() as i64, "a new row goes last");
                    let page_id: String =
                        serde_json::from_value(call["reply"]["id"].clone()).unwrap();
                    ids.insert(page_id, added.id.clone());
                    added
                }
                "milestones_update" => {
                    let page_id: String = serde_json::from_value(arg("id")).unwrap();
                    let by: String = serde_json::from_value(arg("by")).unwrap();
                    let patch: MilestonePatch = serde_json::from_value(arg("patch")).unwrap();
                    update(&conn, &path, &ids[&page_id], patch, &by, 3).unwrap()
                }
                other => panic!("unexpected command {other}"),
            };
            history.push((row.title, row.note, row.status, row.target));
        }
        let shown = |title: &str, note: &str, status, target: Option<&str>| {
            (
                title.to_string(),
                note.to_string(),
                status,
                target.map(String::from),
            )
        };
        let (list, turn) = (
            "Milestone list in the Mono overview",
            "Shown when a Mono starts a turn",
        );
        let (v2, allowlist) = ("Giga Mono harness (change H, v2)", "Tool allowlist only.");
        assert_eq!(
            history,
            [
                shown(list, turn, Status::InProgress, Some("2026-12-12")),
                shown("Someday", "", Status::Planned, None),
                shown(list, turn, Status::InProgress, None),
                shown(v2, allowlist, Status::Planned, Some("2026-11-21")),
                shown(v2, allowlist, Status::Planned, None),
                shown(v2, allowlist, Status::Done, Some("2028-02-29")),
                shown("Row 4", "Before", Status::Idea, Some("2025-01-05")),
            ]
        );
    }
}
