//! Adding a project to a Space (ADE-263): T1, T2, and the links a Mono's list
//! writes (T3, T4). A link never appears for a path at or under the Giga
//! workspace.

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Runtime, State, WebviewWindow};

use super::project_removal::{begin, key_of, session_facts, ProjectRefusal};
use super::projects::{emit_changed, holders, in_workspace, workspace_roots, InWorkspace};
use super::{project_key, project_space_in, shown_space};
use crate::session_store::SessionStore;

/// How a project joined the Space.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Added {
    /// T1: a new project, which shows only in this Space.
    Assigned,
    /// T2: a known project; other Spaces do not change.
    Linked,
}

/// Whether a row, link, session or Mono knows the project. The window's own
/// lists are `known_to_window`: the backend cannot read them.
fn known_here(conn: &Connection, key: &str) -> Result<bool, ProjectRefusal> {
    let row: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_projects WHERE project_path = ?1)
             OR EXISTS(SELECT 1 FROM space_project_links WHERE project_path = ?1)",
        [key],
        |row| row.get(0),
    )?;
    Ok(row
        || holders(conn)?.iter().any(|holder| holder.project == key)
        || session_facts(conn, "1")?
            .iter()
            .any(|session| session.project.as_deref() == Some(key)))
}

/// T1 and T2. A known project already assigned to the Space writes nothing.
pub(crate) fn add(
    store: &SessionStore,
    space: &str,
    path: &str,
    known_to_window: bool,
) -> Result<Added, ProjectRefusal> {
    let key = key_of(path)?;
    let conn = store.lock_conn()?;
    let tx = begin(&conn)?;
    let added = if known_to_window || known_here(&tx, &key)? {
        if project_space_in(&tx, &key)? != space {
            tx.execute(
                "INSERT OR IGNORE INTO space_project_links (space_id, project_path)
                 VALUES (?1, ?2)",
                params![space, key],
            )?;
        }
        Added::Linked
    } else {
        tx.execute(
            "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)",
            params![key, space],
        )?;
        Added::Assigned
    };
    tx.commit()?;
    Ok(added)
}

/// T3 and T4: links each of these Mono's listed paths in its Space, on the
/// caller's transaction. A path that is no key, or at or under the workspace,
/// gets no link; a Space gone from `spaces` could not take one.
pub(crate) fn link_listed(
    conn: &Connection,
    space: &str,
    listed: &[String],
) -> rusqlite::Result<()> {
    let roots = workspace_roots();
    for path in listed {
        let Ok(key) = project_key(path) else {
            continue;
        };
        if in_workspace(&key, &roots) == InWorkspace::Outside {
            conn.execute(
                "INSERT OR IGNORE INTO space_project_links (space_id, project_path)
                 SELECT id, ?2 FROM spaces WHERE id = ?1",
                params![space, key],
            )?;
        }
    }
    Ok(())
}

/// The paths a Mono's data lists.
pub(crate) fn listed_paths(data: &Map<String, Value>) -> Vec<String> {
    data.get("projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[tauri::command(async)]
pub fn spaces_project_add<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    store: State<'_, SessionStore>,
    project_path: String,
    known_to_window: bool,
) -> Result<Added, ProjectRefusal> {
    let space = shown_space(&store, window.label())?;
    let added = add(&store, &space, &project_path, known_to_window)?;
    emit_changed(&app);
    Ok(added)
}
