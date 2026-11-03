//! ADE-175: what `monos.list`, `sessions.list` and `messages.get_monos` show
//! a caller, and (ADE-194) which Monos `monos.get` and `monos.read` answer
//! for. Its Space and role are the ones `check_action` resolved for the Space
//! rule (ADE-174's `caller_space`, from saved rows only). A regular Mono also
//! sees the regular Monos of other Spaces, marked with their Space's name. A
//! project is in its assigned Space and in the Space of each Mono that lists
//! it (ADE-229), so a project several Spaces share is listed in each.
//! `sessions.list` gets only the caller's Space's sessions in the caller's
//! projects, never another Space's session ids.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use super::project_spaces_in;
use crate::mono_gate::{path_key, Caller, Speaker};
use crate::mono_messages::{MonoId, SessionId};
use crate::mono_roster::{self, MonoRole};

const SCOPED_ACTIONS: [&str; 5] = [
    "monos.list",
    "monos.get",
    "monos.read",
    "sessions.list",
    "messages.get_monos",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceScope {
    pub space: String,
    /// The caller's own projects in its Space: a Mono's (always in its Space,
    /// ADE-229), or a session's folder.
    pub projects: Vec<String>,
    pub monos: Vec<ScopedMono>,
    /// `sessions.list` only: the saved sessions of the caller's Space whose
    /// folder is one of `projects`. It lists no other session.
    pub sessions: Vec<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedMono {
    pub mono_id: MonoId,
    /// The name of the Mono's Space; `None` in the caller's Space.
    pub other_space: Option<String>,
    /// Its projects in the caller's Space.
    pub projects: Vec<String>,
}

/// A saved session as the rule reads it.
struct SessionRow {
    id: SessionId,
    space: String,
    cwd: String,
}

/// A project path and every Space it is in (`project_spaces_in`); none when
/// the path is no project key.
type Placed = (String, Vec<String>);

/// A `monos` row as the rule reads it.
struct MonoRow {
    id: MonoId,
    role: MonoRole,
    space: String,
    projects: Vec<Placed>,
}

/// The caller as the rule reads it.
struct CallerRow {
    space: String,
    /// A regular Mono's chat or habit run: not a Giga, not a session.
    regular_mono: bool,
    projects: Vec<Placed>,
}

/// The rule, over plain rows. `monos` is in roster order; `spaces` maps a
/// Space id to its name.
fn decide(
    caller: &CallerRow,
    monos: &[MonoRow],
    sessions: &[SessionRow],
    spaces: &HashMap<String, String>,
) -> SpaceScope {
    let ours = |projects: &[Placed]| -> Vec<String> {
        projects
            .iter()
            .filter(|(_, spaces)| spaces.contains(&caller.space))
            .map(|(path, _)| path.clone())
            .collect()
    };
    let name = |space: &String| spaces.get(space).unwrap_or(space).clone();
    let monos = monos
        .iter()
        .filter(|mono| {
            mono.space == caller.space || (caller.regular_mono && mono.role == MonoRole::Mono)
        })
        .map(|mono| ScopedMono {
            mono_id: mono.id.clone(),
            other_space: (mono.space != caller.space).then(|| name(&mono.space)),
            projects: ours(&mono.projects),
        })
        .collect();
    let projects = ours(&caller.projects);
    let keys: Vec<_> = projects.iter().map(|path| path_key(path)).collect();
    SpaceScope {
        space: caller.space.clone(),
        sessions: sessions
            .iter()
            .filter(|row| row.space == caller.space && keys.contains(&path_key(&row.cwd)))
            .map(|row| row.id.clone())
            .collect(),
        projects,
        monos,
    }
}

/// The scope of `action` for `caller`, whose Space and Mono role (`None`
/// for a session) `check_action` resolved; `None` for an action that shows
/// every Space's rows.
pub(crate) fn scoped(
    conn: &Connection,
    caller: &Caller,
    action: &str,
    space: &str,
    role: Option<MonoRole>,
) -> rusqlite::Result<Option<SpaceScope>> {
    if !SCOPED_ACTIONS.contains(&action) {
        return Ok(None);
    }
    let monos = mono_rows(conn)?;
    let projects = match &caller.speaker {
        Speaker::Mono { mono_id, .. } => monos
            .iter()
            .find(|mono| mono.id == *mono_id)
            .map(|mono| mono.projects.clone())
            .unwrap_or_default(),
        Speaker::Session { cwd, .. } => vec![placed(conn, cwd)],
    };
    let called = CallerRow {
        space: space.to_owned(),
        regular_mono: role == Some(MonoRole::Mono),
        projects,
    };
    let sessions = if action == "sessions.list" {
        session_rows(conn)?
    } else {
        Vec::new()
    };
    let spaces: HashMap<_, _> = pairs(conn, "SELECT id, name FROM spaces")?;
    Ok(Some(decide(&called, &monos, &sessions, &spaces)))
}

/// The two text columns of each row `sql` selects.
fn pairs<T: FromIterator<(String, String)>>(conn: &Connection, sql: &str) -> rusqlite::Result<T> {
    conn.prepare(sql)?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect()
}

/// A key `project_spaces_in` cannot read is in no Space, so no caller sees it.
fn placed(conn: &Connection, path: &str) -> Placed {
    let spaces = project_spaces_in(conn, path).unwrap_or_default();
    (path.to_owned(), spaces)
}

/// Every Mono in roster order, each project as `data_json` holds it.
fn mono_rows(conn: &Connection) -> rusqlite::Result<Vec<MonoRow>> {
    let spaces: HashMap<_, _> = pairs(conn, "SELECT id, space_id FROM monos")?;
    Ok(mono_roster::list(conn)?
        .into_iter()
        .filter_map(|row| {
            Some(MonoRow {
                id: MonoId::parse(&row.id)?,
                role: row.role,
                space: spaces.get(&row.id)?.clone(),
                // TS `parseMono` drops an empty path.
                projects: row
                    .data
                    .get("projects")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(|path| placed(conn, path))
                    .collect(),
            })
        })
        .collect())
}

fn session_rows(conn: &Connection) -> rusqlite::Result<Vec<SessionRow>> {
    let rows = conn
        .prepare("SELECT id, space_id, cwd FROM sessions")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, space, cwd)| {
            Some(SessionRow {
                id: SessionId::parse(&id)?,
                space,
                cwd,
            })
        })
        .collect())
}
