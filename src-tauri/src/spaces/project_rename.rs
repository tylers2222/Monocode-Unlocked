//! T12 (ADE-263): what follows a project when `session_rebase_project` moves it,
//! inside `rebase_project`'s transaction. If the new path already has rows the
//! two unite: the old path's assignment wins, and a login the new path already
//! has wins. A lease moves only here (I4).

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use super::projects::{held_by, in_workspace, workspace_roots, InWorkspace};
use super::{leases, project_key, FIRST_SPACE};
use crate::session_store::now_millis;

/// Every stored form of `path` in `table.column`: what a `project_key` compare
/// finds, plus `path` as written. A form that is no key can only be `path`.
pub(crate) fn stored_forms(
    conn: &Connection,
    table: &str,
    column: &str,
    path: &str,
) -> rusqlite::Result<Vec<String>> {
    let key = project_key(path).ok();
    let stored = conn
        .prepare(&format!("SELECT DISTINCT {column} FROM {table}"))?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut forms: Vec<String> = stored
        .into_iter()
        .filter(|form| key.is_some() && project_key(form).ok() == key)
        .collect();
    if !forms.iter().any(|form| form == path) {
        forms.push(path.to_owned());
    }
    Ok(forms)
}

/// Whether anything stored knows the old path. A rename of a path nothing knows,
/// such as a retry, has no assignment to hand over.
fn known(tx: &Connection, from: &str) -> rusqlite::Result<bool> {
    let row: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_projects WHERE project_path = ?1)",
        [from],
        |row| row.get(0),
    )?;
    if row || !held_by(tx, from)?.is_empty() {
        return Ok(true);
    }
    let folders = tx
        .prepare("SELECT DISTINCT cwd FROM sessions")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(folders
        .iter()
        .any(|cwd| project_key(cwd).as_deref() == Ok(from)))
}

/// The assigned row and links of a project outside the workspace.
fn rekey_membership(tx: &Connection, from: &str, to: &str) -> rusqlite::Result<()> {
    if !known(tx, from)? {
        return Ok(());
    }
    let assigned = |path: &str| -> rusqlite::Result<Option<String>> {
        tx.query_row(
            "SELECT space_id FROM space_projects WHERE project_path = ?1",
            [path],
            |row| row.get(0),
        )
        .optional()
    };
    let (old, new) = (assigned(from)?, assigned(to)?);
    let new_was_known = new.is_some() || !held_by(tx, to)?.is_empty();
    let winner = old.clone().unwrap_or_else(|| FIRST_SPACE.to_owned());
    if new_was_known {
        let loser = new.unwrap_or_else(|| FIRST_SPACE.to_owned());
        if loser != winner {
            tx.execute(
                "INSERT OR IGNORE INTO space_project_links (space_id, project_path)
                 SELECT id, ?2 FROM spaces WHERE id = ?1",
                params![loser, to],
            )?;
        }
    }
    tx.execute("DELETE FROM space_projects WHERE project_path = ?1", [from])?;
    tx.execute("DELETE FROM space_projects WHERE project_path = ?1", [to])?;
    if let Some(space) = old {
        tx.execute(
            "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
            params![to, space],
        )?;
    }
    tx.execute(
        "UPDATE OR IGNORE space_project_links SET project_path = ?2 WHERE project_path = ?1",
        params![from, to],
    )?;
    tx.execute(
        "DELETE FROM space_project_links WHERE project_path = ?1",
        [from],
    )
    .map(drop)
}

/// A login the new path already has wins over the old path's.
fn rekey_logins(tx: &Connection, from: &str, to: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE OR IGNORE space_project_accounts SET project_path = ?2 WHERE project_path = ?1",
        params![from, to],
    )?;
    tx.execute(
        "DELETE FROM space_project_accounts WHERE project_path = ?1",
        [from],
    )
    .map(drop)
}

/// Each Mono's list, with the new path once. A row that is not JSON stays.
fn rekey_mono_lists(tx: &Connection, from: &str, to: &str, written: &str) -> rusqlite::Result<()> {
    let rows = tx
        .prepare("SELECT id, data_json FROM monos")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, raw) in rows {
        let Ok(mut data) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(list) = data.get_mut("projects").and_then(Value::as_array_mut) else {
            continue;
        };
        let key_of = |item: &Value| item.as_str().and_then(|path| project_key(path).ok());
        if !list
            .iter()
            .any(|item| key_of(item).as_deref() == Some(from))
        {
            continue;
        }
        let mut seen_new = false;
        let mut moved = Vec::with_capacity(list.len());
        for item in list.drain(..) {
            let key = key_of(&item);
            if key.as_deref() == Some(from) || key.as_deref() == Some(to) {
                if !seen_new {
                    seen_new = true;
                    moved.push(Value::from(written));
                }
            } else {
                moved.push(item);
            }
        }
        *list = moved;
        tx.execute(
            "UPDATE monos SET data_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, data.to_string(), now_millis()],
        )?;
    }
    Ok(())
}

/// An automation's folder, so its runs keep their assigned Space.
fn rekey_automations(tx: &Connection, from: &str, written: &str) -> rusqlite::Result<()> {
    let rows = tx
        .prepare(
            "SELECT id, json_extract(definition_json, '$.cwd') FROM automations
              WHERE json_valid(definition_json)",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, cwd) in rows {
        if cwd.is_some_and(|cwd| project_key(&cwd).as_deref() == Ok(from)) {
            tx.execute(
                "UPDATE automations SET definition_json = json_set(definition_json, '$.cwd', ?2),
                        updated_at = ?3 WHERE id = ?1",
                params![id, written, now_millis()],
            )?;
        }
    }
    Ok(())
}

/// T12, on `rebase_project`'s transaction. `to_path` is the new path as the
/// window writes it. A path that is no project key has nothing to move, and `~`
/// is no project.
pub(crate) fn rekey(tx: &Connection, from_path: &str, to_path: &str) -> rusqlite::Result<()> {
    let (Ok(from), Ok(to)) = (project_key(from_path), project_key(to_path)) else {
        return Ok(());
    };
    if from == to || from == "~" || to == "~" {
        return Ok(());
    }
    let roots = workspace_roots();
    let outside = |key: &str| in_workspace(key, &roots) == InWorkspace::Outside;
    if outside(&from) && outside(&to) {
        rekey_membership(tx, &from, &to)?;
    }
    rekey_logins(tx, &from, &to)?;
    rekey_mono_lists(tx, &from, &to, to_path)?;
    rekey_automations(tx, &from, to_path)?;
    leases::move_with_folder(tx, &from, to_path)
}
