//! Taking a project out of a Space and deleting its data (ADE-263, ADE-267):
//! T5 to T11 and T16 of `space-projects/state-machine`. Membership guards and
//! writes are one `BEGIN IMMEDIATE`, so a refused event changes nothing, and
//! running turns are read before it. Only [`delete`] deletes sessions: the
//! Space's own, never a Mono's chat. No event moves a lease or touches a file.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use tauri::{AppHandle, Runtime, State, WebviewWindow};

use super::projects::{
    emit_changed, held_by, holders, in_workspace, three_texts, workspace_roots, InWorkspace,
};
use super::{project_key, project_space_in, project_spaces_in, shown_space};
use crate::control::ControlHost;
use crate::session_store::{session_delete, SessionStore, LISTED_SESSION};

/// The sessions with a turn, read from `ControlHost` before any transaction.
pub(crate) type Turns = HashSet<String>;

/// Why a project stays. `message` is the text the window shows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub(crate) enum ProjectRefusal {
    /// T7, T9, T10, T11: these Monos list it.
    MonoHeld {
        project: String,
        monos: Vec<String>,
        message: String,
    },
    /// T8, T10, T11: a session runs in it.
    SessionRunning { project: String, message: String },
    /// T9, T10, T11: a Mono's chat has this folder.
    MonoChat {
        project: String,
        monos: Vec<String>,
        message: String,
    },
    /// T6, T11: an automation runs in the project's assigned Space.
    Automation {
        project: String,
        automations: Vec<String>,
        message: String,
    },
    /// The project is assigned here and in no other Space: Archive or Delete.
    OnlySpace { project: String, message: String },
    /// At or under the Giga workspace.
    Workspace { project: String, message: String },
    /// The read or write failed, or the path is no project. Nothing changed.
    Failed { message: String },
}

impl From<String> for ProjectRefusal {
    fn from(message: String) -> Self {
        Self::Failed { message }
    }
}

impl From<rusqlite::Error> for ProjectRefusal {
    fn from(error: rusqlite::Error) -> Self {
        error.to_string().into()
    }
}

/// The folder's last name, as the window names a project.
fn display_name(key: &str) -> String {
    match key.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => key.to_owned(),
    }
}

/// `A`, `A and B`, `A, B and C`.
fn names(all: &[String]) -> String {
    match all {
        [first @ .., last] if !first.is_empty() => format!("{} and {last}", first.join(", ")),
        _ => all.concat(),
    }
}

/// A project's key. `~` is the key of no project, and a path at or under the
/// Giga workspace has no Remove, Archive or Delete.
fn key_of(path: &str) -> Result<String, ProjectRefusal> {
    let key = project_key(path)?;
    if key == "~" {
        return Err(format!("Not a project: {path}").into());
    }
    if in_workspace(&key, &workspace_roots()) != InWorkspace::Outside {
        return Err(ProjectRefusal::Workspace {
            project: display_name(&key),
            message: "This folder is part of the MonoCode workspace, so it stays.".into(),
        });
    }
    Ok(key)
}

/// T7 and T9: refuses when a Mono lists the project, in `space` or, with no
/// `space`, in any Space.
fn refuse_mono(conn: &Connection, key: &str, space: Option<&str>) -> Result<(), ProjectRefusal> {
    let mut monos: Vec<String> = Vec::new();
    for holder in holders(conn)? {
        let here = space.is_none_or(|space| space == holder.space);
        if here && holder.project == key && !monos.contains(&holder.mono) {
            monos.push(holder.mono);
        }
    }
    if monos.is_empty() {
        return Ok(());
    }
    let project = display_name(key);
    let (verb, object) = if monos.len() == 1 {
        ("uses", "that Mono")
    } else {
        ("use", "those Monos")
    };
    Err(ProjectRefusal::MonoHeld {
        message: format!(
            "{} {verb} {project}. Take it off {object} first.",
            names(&monos)
        ),
        project,
        monos,
    })
}

/// T9: Archive hides a project in every Space, so a Mono of any Space stops it,
/// and one of another Space is named with its Space.
fn refuse_archive(conn: &Connection, key: &str, space: &str) -> Result<(), ProjectRefusal> {
    let space_names = space_names(conn)?;
    let (mut monos, mut labels): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for holder in holders(conn)? {
        if holder.project != key || monos.contains(&holder.mono) {
            continue;
        }
        labels.push(if holder.space == space {
            holder.mono.clone()
        } else {
            let name = space_names.get(&holder.space).unwrap_or(&holder.space);
            format!("{} in {name}", holder.mono)
        });
        monos.push(holder.mono);
    }
    if monos.is_empty() {
        return Ok(());
    }
    let project = display_name(key);
    let (verb, object) = if monos.len() == 1 {
        ("uses", "that Mono")
    } else {
        ("use", "those Monos")
    };
    Err(ProjectRefusal::MonoHeld {
        message: format!(
            "{} {verb} {project}, and Archive hides it in every Space. Take it off {object} first.",
            names(&labels)
        ),
        project,
        monos,
    })
}

fn space_names(conn: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    conn.prepare("SELECT id, name FROM spaces")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// A session as the guards read it.
#[derive(Clone, Debug)]
pub(crate) struct SessionFacts {
    pub(crate) id: String,
    pub(crate) space: String,
    /// `None` for a folder that is no project key.
    pub(crate) project: Option<String>,
}

fn session_facts(conn: &Connection, condition: &str) -> rusqlite::Result<Vec<SessionFacts>> {
    let rows = three_texts(
        conn,
        &format!("SELECT id, space_id, cwd FROM sessions WHERE {condition}"),
    )?;
    Ok(rows
        .into_iter()
        .map(|(id, space, cwd)| SessionFacts {
            id,
            space,
            project: project_key(&cwd).ok(),
        })
        .collect())
}

/// The spec's `running`, as a pure function: the Spaces of the project's
/// sessions in the running set, which is the sessions with a turn or an
/// `in_flight_sessions` row. `running(S,P)` is whether it holds S.
pub(crate) fn running_spaces(
    sessions: &[SessionFacts],
    running_set: &HashSet<String>,
    key: &str,
) -> Vec<String> {
    let mut spaces: Vec<String> = sessions
        .iter()
        .filter(|session| {
            session.project.as_deref() == Some(key) && running_set.contains(&session.id)
        })
        .map(|session| session.space.clone())
        .collect();
    spaces.sort();
    spaces.dedup();
    spaces
}

/// T8. A session of `space` is "here", which wins; `any_space` also stops for
/// a session of another Space, which is named. Reads every session and the
/// in-flight rows inside the caller's transaction.
fn refuse_running(
    conn: &Connection,
    key: &str,
    space: &str,
    any_space: bool,
    turns: &Turns,
) -> Result<(), ProjectRefusal> {
    let mut running_set = turns.clone();
    running_set.extend(
        conn.prepare("SELECT session_id FROM in_flight_sessions")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    let spaces = running_spaces(&session_facts(conn, "1")?, &running_set, key);
    let place = if spaces.iter().any(|running| running == space) {
        "here".to_owned()
    } else {
        match spaces.first().filter(|_| any_space) {
            Some(other) => {
                let names = space_names(conn)?;
                format!("in {}", names.get(other).unwrap_or(other))
            }
            None => return Ok(()),
        }
    };
    let project = display_name(key);
    Err(ProjectRefusal::SessionRunning {
        message: format!("A session is running in {project} {place}. Stop it first."),
        project,
    })
}

/// The ids of every Mono's chat, with the Mono's name, in roster order.
fn mono_chats(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare(
        "SELECT name, json_extract(data_json, '$.sessionId') FROM monos
          WHERE json_valid(data_json) AND json_extract(data_json, '$.sessionId') IS NOT NULL
          ORDER BY sort_order, created_at, id",
    )?
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
    .collect()
}

/// F1: a Mono's chat in the project stops T9, T10 and T11, in any Space.
fn refuse_mono_chat(conn: &Connection, key: &str) -> Result<(), ProjectRefusal> {
    let here: HashSet<String> = session_facts(conn, "1")?
        .into_iter()
        .filter(|session| session.project.as_deref() == Some(key))
        .map(|session| session.id)
        .collect();
    let monos: Vec<String> = mono_chats(conn)?
        .into_iter()
        .filter(|(_, chat)| here.contains(chat))
        .map(|(name, _)| name)
        .collect();
    let message = match monos.as_slice() {
        [] => return Ok(()),
        [mono] => format!("{mono}'s chat is in this project."),
        _ => format!("{} have chats in this project.", names(&monos)),
    };
    Err(ProjectRefusal::MonoChat {
        project: display_name(key),
        monos,
        message,
    })
}

/// F13: T6 and T11 change the Space an automation in the project runs in.
fn refuse_automation(conn: &Connection, key: &str) -> Result<(), ProjectRefusal> {
    let rows: Vec<(String, Option<String>)> = conn
        .prepare(
            "SELECT COALESCE(json_extract(definition_json, '$.name'), id),
                    json_extract(definition_json, '$.cwd')
               FROM automations WHERE json_valid(definition_json) ORDER BY id",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let automations: Vec<String> = rows
        .into_iter()
        .filter(|(_, cwd)| {
            cwd.as_deref()
                .is_some_and(|cwd| project_key(cwd).as_deref() == Ok(key))
        })
        .map(|(name, _)| name)
        .collect();
    if automations.is_empty() {
        return Ok(());
    }
    let project = display_name(key);
    let message = match automations.as_slice() {
        [automation] => {
            format!("The automation {automation} runs in {project}. Move or delete it first.")
        }
        _ => format!(
            "The automations {} run in {project}. Move or delete them first.",
            names(&automations)
        ),
    };
    Err(ProjectRefusal::Automation {
        project,
        automations,
        message,
    })
}

fn begin(conn: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
}

/// T5, T6, T7 and T8, on the caller's transaction. Whether a row changed: a
/// project with no row of its own in the Space stays as it is.
fn leave(tx: &Connection, space: &str, key: &str, turns: &Turns) -> Result<bool, ProjectRefusal> {
    refuse_mono(tx, key, Some(space))?;
    let assigned_here = project_space_in(tx, key)? == space;
    let linked: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_project_links WHERE space_id = ?1 AND project_path = ?2)",
        params![space, key],
        |row| row.get(0),
    )?;
    if !assigned_here && !linked {
        return Ok(false);
    }
    let next = held_by(tx, key)?.into_iter().find(|other| other != space);
    if assigned_here && next.is_none() {
        let project = display_name(key);
        return Err(ProjectRefusal::OnlySpace {
            message: format!("{project} is only in this Space. Archive or delete it instead."),
            project,
        });
    }
    if assigned_here {
        refuse_automation(tx, key)?;
    }
    refuse_running(tx, key, space, false, turns)?;
    if let (true, Some(next)) = (assigned_here, next) {
        tx.execute(
            "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)
             ON CONFLICT (project_path) DO UPDATE SET space_id = excluded.space_id",
            params![key, next],
        )?;
    }
    tx.execute(
        "DELETE FROM space_project_links WHERE space_id = ?1 AND project_path = ?2",
        params![space, key],
    )?;
    Ok(true)
}

/// Read before the transaction: `ControlHost` is never locked inside the store
/// lock. Unreadable turns refuse, so a guard never passes for lack of an answer.
fn turns_now(turns: &impl Fn() -> Option<Turns>) -> Result<Turns, ProjectRefusal> {
    turns().ok_or_else(|| "Running sessions unavailable".to_owned().into())
}

/// Whether the project left the Space (false: it was not in it).
pub(crate) fn remove(
    store: &SessionStore,
    space: &str,
    path: &str,
    turns: impl Fn() -> Option<Turns>,
) -> Result<bool, ProjectRefusal> {
    let key = key_of(path)?;
    let turns = turns_now(&turns)?;
    let conn = store.lock_conn()?;
    let tx = begin(&conn)?;
    let left = leave(&tx, space, &key, &turns)?;
    tx.commit()?;
    Ok(left)
}

/// T9: a friendly refusal while a Mono lists the project or has its chat there,
/// or a session of any Space runs in it: archiving closes every tab in P. The
/// protection for a Mono is the list, which keeps its project shown when archived.
pub(crate) fn archive_check(
    store: &SessionStore,
    space: &str,
    path: &str,
    turns: impl Fn() -> Option<Turns>,
) -> Result<(), ProjectRefusal> {
    let key = key_of(path)?;
    let turns = turns_now(&turns)?;
    let conn = store.lock_conn()?;
    refuse_archive(&conn, &key, space)?;
    refuse_mono_chat(&conn, &key)?;
    refuse_running(&conn, &key, space, true, &turns)
}

/// What the window's dialog counts: this Space's chats.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeletePreview {
    pub(crate) last_space: bool,
    pub(crate) session_count: usize,
    pub(crate) other_spaces: Vec<String>,
    /// The chats other Spaces hold in the project. They stay: T11's dialog says
    /// so only when this is above 0.
    pub(crate) hidden_session_count: usize,
}

/// What a delete did. When `last_space` holds (T11) the window clears the
/// project's logo, settings and recents. The milestones are already gone: the
/// window never calls `milestones_remove_project`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDeleted {
    pub(crate) deleted_session_ids: Vec<String>,
    pub(crate) last_space: bool,
}

/// The other Spaces that have the project.
fn other_spaces(conn: &Connection, space: &str, key: &str) -> Result<Vec<String>, ProjectRefusal> {
    Ok(project_spaces_in(conn, key)?
        .into_iter()
        .filter(|other| other != space)
        .collect())
}

/// T10 and T11's guards, and whether this is the project's last Space. The last
/// Space's delete waits for a session of any Space (I5). An automation stops
/// whichever delete takes the assignment away (T6, T11).
fn guard_delete(
    conn: &Connection,
    space: &str,
    key: &str,
    turns: &Turns,
) -> Result<bool, ProjectRefusal> {
    let last_space = other_spaces(conn, space, key)?.is_empty();
    refuse_mono(conn, key, Some(space))?;
    refuse_mono_chat(conn, key)?;
    refuse_running(conn, key, space, last_space, turns)?;
    if project_space_in(conn, key)? == space {
        refuse_automation(conn, key)?;
    }
    Ok(last_space)
}

/// The sessions of `space` in the project that `condition` selects, never a
/// Mono's chat.
fn own_sessions(
    conn: &Connection,
    space: &str,
    key: &str,
    condition: &str,
) -> rusqlite::Result<Vec<String>> {
    let chats: HashSet<String> = mono_chats(conn)?.into_iter().map(|(_, id)| id).collect();
    Ok(session_facts(conn, condition)?
        .into_iter()
        .filter(|session| {
            session.space == space
                && session.project.as_deref() == Some(key)
                && !chats.contains(&session.id)
        })
        .map(|session| session.id)
        .collect())
}

pub(crate) fn delete_preview(
    store: &SessionStore,
    space: &str,
    path: &str,
    turns: impl Fn() -> Option<Turns>,
) -> Result<DeletePreview, ProjectRefusal> {
    let key = key_of(path)?;
    let turns = turns_now(&turns)?;
    let conn = store.lock_conn()?;
    let listed = session_facts(&conn, LISTED_SESSION)?;
    Ok(DeletePreview {
        last_space: guard_delete(&conn, space, &key, &turns)?,
        session_count: own_sessions(&conn, space, &key, LISTED_SESSION)?.len(),
        other_spaces: other_spaces(&conn, space, &key)?,
        hidden_session_count: listed
            .iter()
            .filter(|session| {
                session.space != space && session.project.as_deref() == Some(key.as_str())
            })
            .count(),
    })
}

/// T11's rows: the milestones and settings under any form of the path, and the
/// assigned and link rows.
fn purge_rows(tx: &Connection, key: &str) -> Result<(), ProjectRefusal> {
    for table in ["milestones", "milestone_settings"] {
        let forms = tx
            .prepare(&format!("SELECT DISTINCT project_path FROM {table}"))?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for form in forms {
            if project_key(&form).as_deref() == Ok(key) {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE project_path = ?1"),
                    [form],
                )?;
            }
        }
    }
    tx.execute("DELETE FROM space_projects WHERE project_path = ?1", [key])?;
    tx.execute(
        "DELETE FROM space_project_links WHERE project_path = ?1",
        [key],
    )?;
    Ok(())
}

/// T10, T11 and T16. Every guard passes before the first delete. `end` ends
/// and deletes one session and takes the store lock itself, so a turn that
/// starts in a session still to go stops the delete there. The membership write
/// commits last, with its guards read again: a failure halfway leaves the
/// project listed, and a retry finishes it.
pub(crate) fn delete(
    store: &SessionStore,
    space: &str,
    path: &str,
    turns: impl Fn() -> Option<Turns>,
    mut end: impl FnMut(&str) -> Result<(), String>,
) -> Result<ProjectDeleted, ProjectRefusal> {
    let key = key_of(path)?;
    let first = turns_now(&turns)?;
    let sessions = {
        let conn = store.lock_conn()?;
        guard_delete(&conn, space, &key, &first)?;
        own_sessions(&conn, space, &key, "1")?
    };
    for session in &sessions {
        if turns_now(&turns)?.contains(session) {
            refuse_running(&*store.lock_conn()?, &key, space, false, &turn_set(session))?;
        }
        end(session)?;
    }
    let now = turns_now(&turns)?;
    let conn = store.lock_conn()?;
    let tx = begin(&conn)?;
    let last_space = guard_delete(&tx, space, &key, &now)?;
    if last_space {
        purge_rows(&tx, &key)?;
    } else {
        leave(&tx, space, &key, &now)?;
    }
    tx.commit()?;
    Ok(ProjectDeleted {
        deleted_session_ids: sessions,
        last_space,
    })
}

fn turn_set(session: &str) -> Turns {
    HashSet::from([session.to_owned()])
}

fn host_turns(control: &ControlHost) -> impl Fn() -> Option<Turns> + '_ {
    move || {
        control
            .active_turns()
            .map(|turns| turns.into_keys().collect())
    }
}

#[tauri::command(async)]
pub fn spaces_project_remove<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    store: State<'_, SessionStore>,
    control: State<'_, ControlHost>,
    project_path: String,
) -> Result<bool, ProjectRefusal> {
    let space = shown_space(&store, window.label())?;
    let removed = remove(&store, &space, &project_path, host_turns(&control))?;
    if removed {
        emit_changed(&app);
    }
    Ok(removed)
}

/// T9, for the window to call before it archives a project.
#[tauri::command(async)]
pub fn spaces_project_archive_check<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    control: State<'_, ControlHost>,
    project_path: String,
) -> Result<(), ProjectRefusal> {
    let space = shown_space(&store, window.label())?;
    archive_check(&store, &space, &project_path, host_turns(&control))
}

/// T9 writes only the window's own storage, so the window calls this after an
/// archive or unarchive, for the other windows.
#[tauri::command(async)]
pub fn spaces_project_notify_changed<R: Runtime>(app: AppHandle<R>) {
    emit_changed(&app);
}

#[tauri::command(async)]
pub fn spaces_project_delete_preview<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    control: State<'_, ControlHost>,
    project_path: String,
) -> Result<DeletePreview, ProjectRefusal> {
    let space = shown_space(&store, window.label())?;
    delete_preview(&store, &space, &project_path, host_turns(&control))
}

/// The window tombstones and flushes this Space's sessions first, then drops
/// its own copies of `deletedSessionIds`. Only when `lastSpace` holds does it
/// clear the project's logo, settings and recents. Each session goes through
/// `session_delete`, as a single chat's delete does.
#[tauri::command(async)]
pub fn spaces_project_delete(
    window: WebviewWindow,
    app: AppHandle,
    store: State<'_, SessionStore>,
    control: State<'_, ControlHost>,
    project_path: String,
) -> Result<ProjectDeleted, ProjectRefusal> {
    let space = shown_space(&store, window.label())?;
    let deleted = delete(
        &store,
        &space,
        &project_path,
        host_turns(&control),
        |session| {
            session_delete(
                app.clone(),
                store.clone(),
                session.to_owned(),
                Vec::new(),
                true,
            )
        },
    )?;
    crate::milestones::emit_changed(&app, &project_path);
    emit_changed(&app);
    Ok(deleted)
}
