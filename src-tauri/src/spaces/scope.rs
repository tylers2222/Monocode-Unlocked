//! ADE-175: what `monos.list`, `sessions.list` and `messages.get_monos` show
//! a caller. Its Space comes only from saved rows: its Mono's
//! `monos.space_id` (a Mono chat or a habit run), else its session's
//! `sessions.space_id`, Space 1 with no row. A regular Mono also sees the
//! regular Monos of other Spaces, marked with their Space's name.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use super::{project_space_in, FIRST_SPACE};
use crate::mono_gate::{Caller, Speaker};
use crate::mono_messages::{MonoId, SessionId};
use crate::mono_roster::{self, MonoRole};

const SCOPED_ACTIONS: [&str; 3] = ["monos.list", "sessions.list", "messages.get_monos"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceScope {
    pub space: String,
    /// The caller's own projects in its Space: a Mono's, or a session's folder.
    pub projects: Vec<String>,
    pub monos: Vec<ScopedMono>,
    /// Saved sessions of other Spaces, which `sessions.list` hides.
    pub foreign_sessions: Vec<SessionId>,
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

/// A project path and its Space; `None` when the path is no project key.
type Placed = (String, Option<String>);

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
    sessions: &[(SessionId, String)],
    spaces: &HashMap<String, String>,
) -> SpaceScope {
    let ours = |projects: &[Placed]| -> Vec<String> {
        projects
            .iter()
            .filter(|(_, space)| space.as_deref() == Some(caller.space.as_str()))
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
    SpaceScope {
        space: caller.space.clone(),
        projects: ours(&caller.projects),
        monos,
        foreign_sessions: sessions
            .iter()
            .filter(|(_, space)| *space != caller.space)
            .map(|(id, _)| id.clone())
            .collect(),
    }
}

/// The scope of `action` for `caller`; `None` for an action that shows
/// every Space's rows.
pub(crate) fn scoped(
    conn: &Connection,
    caller: &Caller,
    action: &str,
) -> rusqlite::Result<Option<SpaceScope>> {
    if !SCOPED_ACTIONS.contains(&action) {
        return Ok(None);
    }
    let monos = mono_rows(conn)?;
    let session_space = session_space(conn, &caller.session_id)?;
    let called = match &caller.speaker {
        Speaker::Mono { mono_id, .. } => match monos.iter().find(|mono| mono.id == *mono_id) {
            Some(mono) => CallerRow {
                space: mono.space.clone(),
                regular_mono: mono.role == MonoRole::Mono,
                projects: mono.projects.clone(),
            },
            None => CallerRow {
                space: session_space,
                regular_mono: false,
                projects: Vec::new(),
            },
        },
        Speaker::Session { cwd, .. } => CallerRow {
            space: session_space,
            regular_mono: false,
            projects: vec![placed(conn, cwd)],
        },
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
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect()
}

/// A key `project_space` cannot read is in no Space, so no caller sees it.
fn placed(conn: &Connection, path: &str) -> Placed {
    (path.to_owned(), project_space_in(conn, path).ok())
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

/// `sessions.space_id`; Space 1 with no row (a habit run, or a session
/// MonoCode never saved).
fn session_space(conn: &Connection, session: &SessionId) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            "SELECT space_id FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| FIRST_SPACE.to_owned()))
}

fn session_rows(conn: &Connection) -> rusqlite::Result<Vec<(SessionId, String)>> {
    let rows: Vec<_> = pairs(conn, "SELECT id, space_id FROM sessions")?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, space)| Some((SessionId::parse(&id)?, space)))
        .collect())
}
