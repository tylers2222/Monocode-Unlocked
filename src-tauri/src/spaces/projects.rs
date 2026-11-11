//! Which projects a Space shows (ADE-235, spec `space-projects/state-machine`).
//! For a pair of Space and project there are three facts: `A`, the Space is the
//! project's assigned Space (`space_projects`, no row is Space 1); `L`, a
//! `space_project_links` row; `M`, a Mono in the Space lists the project, read
//! live from `monos.data_json`. The project is in the Space when any holds.
//! [`super::project_spaces_in`] answers that for one project and is what the
//! app CLI reads; [`list`] answers it for one Space in one read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use super::{leases, project_key, with_conn, FIRST_SPACE};
use crate::session_store::SessionStore;

/// Every write to a Space's projects emits this, so each window reads its list
/// again (T13).
pub(crate) const CHANGED: &str = "spaces:projects-changed";

pub(crate) fn emit_changed<R: Runtime>(app: &AppHandle<R>) {
    if let Err(error) = app.emit(CHANGED, ()) {
        eprintln!("monocode: {CHANGED} event: {error}");
    }
}

/// Where a project key sits against the Giga workspace (`~/MonoCode`). No path
/// at or under it ever gets a link, an assigned row, a Remove, an Archive or a
/// Delete, and a path under it is never listed: it holds Monos' own chats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InWorkspace {
    Outside,
    Root,
    Under,
}

/// The workspace root's keys, as written and with its links resolved.
pub(crate) fn workspace_roots() -> Vec<String> {
    let written =
        crate::dirs_home().map(|home| crate::giga_workspace::workspace_root(Path::new(&home)));
    [written, leases::shared_workspace()]
        .into_iter()
        .flatten()
        .map(|root| crate::mono_gate::path_key(&crate::fs::path_to_js(&root)))
        .collect()
}

pub(crate) fn in_workspace(key: &str, roots: &[String]) -> InWorkspace {
    for root in roots {
        match key.strip_prefix(root.as_str()) {
            Some("") => return InWorkspace::Root,
            Some(rest) if rest.starts_with('/') => return InWorkspace::Under,
            _ => {}
        }
    }
    InWorkspace::Outside
}

/// The three text columns of each row `sql` selects.
pub(super) fn three_texts(
    conn: &Connection,
    sql: &str,
) -> rusqlite::Result<Vec<(String, String, String)>> {
    conn.prepare(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect()
}

/// A project a Mono lists, under the project's key.
pub(super) struct Holder {
    pub(super) space: String,
    pub(super) mono: String,
    pub(super) project: String,
}

/// Every Mono's projects, in roster order. A listed path that is no project
/// key is in no Space.
pub(super) fn holders(conn: &Connection) -> rusqlite::Result<Vec<Holder>> {
    let listed = three_texts(
        conn,
        "SELECT m.space_id, m.name, j.value
               FROM monos m,
                    json_each(CASE WHEN json_valid(m.data_json) THEN m.data_json END, '$.projects') j
              WHERE j.type = 'text'
              ORDER BY m.sort_order, m.created_at, m.id",
    )?;
    Ok(listed
        .into_iter()
        .filter_map(|(space, mono, path)| {
            Some(Holder {
                space,
                mono,
                project: project_key(&path).ok()?,
            })
        })
        .collect())
}

/// The Spaces with a link to `project` or a Mono that lists it, oldest Space
/// first. `project` is a key.
pub(crate) fn held_by(conn: &Connection, project: &str) -> rusqlite::Result<Vec<String>> {
    let order = conn
        .prepare("SELECT id FROM spaces ORDER BY created_at, id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut held = conn
        .prepare("SELECT space_id FROM space_project_links WHERE project_path = ?1")?
        .query_map([project], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    held.extend(
        holders(conn)?
            .into_iter()
            .filter(|holder| holder.project == project)
            .map(|holder| holder.space),
    );
    held.sort_by_key(|space| {
        let rank = order.iter().position(|id| id == space);
        (rank.unwrap_or(order.len()), space.clone())
    });
    held.dedup();
    Ok(held)
}

/// One link per Mono's Space and project (T15), for a database from before the
/// links. A link only repeats an `M` that holds, so no Space's list changes. A
/// Mono whose Space is gone from `spaces` is left out: its link could not
/// reference it, and a failed migration would stop the app from opening. So is
/// a path at or under the Giga workspace, which never becomes a row.
pub(super) fn backfill_links(conn: &Connection) -> rusqlite::Result<()> {
    let roots = workspace_roots();
    for holder in holders(conn)? {
        if in_workspace(&holder.project, &roots) != InWorkspace::Outside {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO space_project_links (space_id, project_path)
             SELECT id, ?2 FROM spaces WHERE id = ?1",
            params![holder.space, holder.project],
        )?;
    }
    Ok(())
}

/// A project with a stored fact in this Space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceProject {
    pub(crate) path: String,
    pub(crate) assigned: bool,
    pub(crate) linked: bool,
    /// The names of the Space's Monos that list it, in roster order: `M` holds
    /// when it is not empty.
    pub(crate) monos: Vec<String>,
}

/// One Space's projects in one read. Space 1 is also every project with no
/// assigned row, which no list can name: a project not in `projects` is in
/// the Space when `no_row_is_here` holds and `assigned_elsewhere` leaves it
/// out. A path under the Giga workspace is never here, and its root only with
/// a Mono of the Space that lists it (I1).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceProjects {
    pub(crate) space_id: String,
    pub(crate) projects: Vec<SpaceProject>,
    /// The paths of `projects` a Mono of the Space lists: an archived project
    /// stays shown when it is here.
    pub(crate) mono_held: Vec<String>,
    pub(crate) no_row_is_here: bool,
    pub(crate) assigned_elsewhere: Vec<String>,
}

pub(crate) fn list(store: &SessionStore, space_id: &str) -> Result<SpaceProjects, String> {
    with_conn(store, |conn| list_in(conn, space_id))?.ok_or_else(|| format!("No Space {space_id}"))
}

fn list_in(conn: &Connection, space_id: &str) -> rusqlite::Result<Option<SpaceProjects>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM spaces WHERE id = ?1)",
        [space_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let rows = conn
        .prepare("SELECT project_path, space_id FROM space_projects ORDER BY project_path")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let no_row_is_here = space_id == FIRST_SPACE;
    let roots = workspace_roots();
    let linked = conn
        .prepare("SELECT project_path FROM space_project_links WHERE space_id = ?1")?
        .query_map([space_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let mut monos: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for holder in holders(conn)? {
        if holder.space == space_id {
            let names = monos.entry(holder.project).or_default();
            if !names.contains(&holder.mono) {
                names.push(holder.mono);
            }
        }
    }
    let paths: BTreeSet<&String> = rows
        .iter()
        .filter(|(_, assigned)| *assigned == space_id)
        .map(|(path, _)| path)
        .chain(&linked)
        .chain(monos.keys())
        .filter(|path| match in_workspace(path, &roots) {
            InWorkspace::Outside => true,
            InWorkspace::Root => monos.contains_key(*path),
            InWorkspace::Under => false,
        })
        .collect();
    let mono_held = paths
        .iter()
        .filter(|path| monos.contains_key(**path))
        .map(|path| (*path).clone())
        .collect();
    let projects = paths
        .iter()
        .map(|path| SpaceProject {
            path: (*path).clone(),
            assigned: rows
                .get(*path)
                .map_or(no_row_is_here, |assigned| assigned == space_id),
            linked: linked.contains(*path),
            monos: monos.get(*path).cloned().unwrap_or_default(),
        })
        .collect();
    let assigned_elsewhere = rows
        .keys()
        .filter(|path| !paths.contains(path) && in_workspace(path, &roots) == InWorkspace::Outside)
        .cloned()
        .collect();
    Ok(Some(SpaceProjects {
        space_id: space_id.to_owned(),
        projects,
        mono_held,
        no_row_is_here,
        assigned_elsewhere,
    }))
}

/// A project with a stored fact, and every Space it is in. The window's add
/// list and known-project check read these; a recent with no entry is Space 1's.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnownProject {
    pub(crate) path: String,
    pub(crate) spaces: Vec<String>,
}

/// Every project any Space has through a row, a link or a Mono, by path. No path
/// at or under the Giga workspace: it is never offered.
pub(crate) fn known(store: &SessionStore) -> Result<Vec<KnownProject>, String> {
    let conn = store.lock_conn()?;
    let stored = |sql: &str| -> rusqlite::Result<Vec<String>> {
        conn.prepare(sql)?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect()
    };
    let read = || -> rusqlite::Result<BTreeSet<String>> {
        let mut paths: BTreeSet<String> = stored("SELECT project_path FROM space_projects")?
            .into_iter()
            .collect();
        paths.extend(stored("SELECT project_path FROM space_project_links")?);
        paths.extend(holders(&conn)?.into_iter().map(|holder| holder.project));
        Ok(paths)
    };
    let roots = workspace_roots();
    read()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|path| in_workspace(path, &roots) == InWorkspace::Outside)
        .map(|path| {
            Ok(KnownProject {
                spaces: super::project_spaces_in(&conn, &path)?,
                path,
            })
        })
        .collect()
}
