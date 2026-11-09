//! Spaces (ADE-168): one window holds several Spaces, each with its own
//! logins, Monos, projects and sessions. This module owns the schema, the
//! storage commands and the one spawn hook every harness child passes through. Each concern the hook
//! applies lives in its own file: `account` (the Space's login), `grants`
//! (usage-limit routing) and `leases` (one Space per checkout). `scope` is
//! what the app CLI shows a caller of its Space.

pub(crate) mod account;
pub(crate) mod grants;
pub(crate) mod leases;
pub(crate) mod membership;
pub(crate) mod scope;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tauri::{Runtime, State, WebviewWindow};

use crate::harness::{HarnessAccount, DEFAULT_PROVIDER_ACCOUNT_ID};
use crate::session_store::{ensure_column, now_millis, SessionStore};

/// The Space every row that predates Spaces belongs to.
pub(crate) const FIRST_SPACE: &str = "space-1";

const DEFAULT_ROUTE_GRANT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

/// The cap on the Settings default for a new grant's length, whose choices
/// are at most 24 hours. A grant itself may last longer ("until the first
/// login resets" can be a monthly limit); bounding it is the grant writer's
/// call (ADE-176), not the schema's.
const MAX_ROUTE_GRANT_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The CLIs that keep named logins (`PROVIDER_ACCOUNT_PROVIDERS` in TS). The
/// schema stores any name: this type is the one parse point, so a new CLI
/// needs no table rebuild.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Provider {
    Claude,
    Codex,
    Devin,
}

impl Provider {
    const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Devin];

    fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Devin => "devin",
        }
    }

    /// The provider a harness CLI is named by, `None` for one without logins.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.as_str() == name)
    }
}

impl ToSql for Provider {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Provider {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse(text)
            .ok_or_else(|| FromSqlError::Other(format!("unknown provider {text}").into()))
    }
}

/// One migration for every Spaces table, safe to run on each open. Monos and
/// sessions that predate Spaces land in Space 1 through the column default.
/// Projects need no backfill: `space_projects` holds only the folders
/// assigned out of Space 1, so a folder with no row is assigned to Space 1.
/// A Mono's own projects are in its Space as well (`project_spaces_in`), so
/// a project shared by several Spaces needs no row. Project paths are stored
/// in the front end's `pathKey()` form.
pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let fresh: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'spaces')",
        [],
        |row| row.get(0),
    )?;
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS spaces (
           id TEXT PRIMARY KEY
             CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^A-Za-z0-9-]*'),
           name TEXT NOT NULL CHECK (name = trim(name) AND name <> ''),
           color TEXT,
           created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS space_settings (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           active_space_id TEXT NOT NULL REFERENCES spaces(id),
           route_grant_ttl_ms INTEGER NOT NULL
             CHECK (route_grant_ttl_ms BETWEEN 1 AND {MAX_ROUTE_GRANT_TTL_MS})
         );
         CREATE TABLE IF NOT EXISTS space_projects (
           project_path TEXT PRIMARY KEY,
           space_id TEXT NOT NULL REFERENCES spaces(id)
         );
         CREATE TABLE IF NOT EXISTS provider_accounts (
           provider TEXT NOT NULL,
           id TEXT NOT NULL CHECK (length(id) BETWEEN 1 AND 80),
           label TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           PRIMARY KEY (provider, id)
         );
         CREATE TABLE IF NOT EXISTS space_accounts (
           space_id TEXT NOT NULL REFERENCES spaces(id),
           provider TEXT NOT NULL,
           account_id TEXT NOT NULL,
           PRIMARY KEY (space_id, provider),
           FOREIGN KEY (provider, account_id)
             REFERENCES provider_accounts(provider, id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS space_project_accounts (
           space_id TEXT NOT NULL REFERENCES spaces(id),
           project_path TEXT NOT NULL, -- '~' for no project
           provider TEXT NOT NULL,
           account_id TEXT NOT NULL,
           PRIMARY KEY (space_id, project_path, provider),
           FOREIGN KEY (provider, account_id)
             REFERENCES provider_accounts(provider, id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS account_route_grants (
           session_id TEXT PRIMARY KEY,
           provider TEXT NOT NULL,
           from_account_id TEXT NOT NULL,
           to_account_id TEXT NOT NULL,
           granted_at INTEGER NOT NULL,
           expires_at INTEGER NOT NULL CHECK (expires_at > granted_at),
           FOREIGN KEY (provider, from_account_id)
             REFERENCES provider_accounts(provider, id) ON DELETE CASCADE,
           FOREIGN KEY (provider, to_account_id)
             REFERENCES provider_accounts(provider, id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS checkout_leases (
           checkout_path TEXT PRIMARY KEY,
           space_id TEXT NOT NULL REFERENCES spaces(id),
           leased_at INTEGER NOT NULL
         );"
    ))?;
    // SQLite refuses ADD COLUMN with both REFERENCES and a non-NULL default,
    // so these two columns carry no foreign key.
    let default = format!("TEXT NOT NULL DEFAULT '{FIRST_SPACE}'");
    for table in ["monos", "sessions"] {
        ensure_column(&tx, table, "space_id", &default)?;
    }
    if fresh {
        tx.execute(
            "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 1', 0)",
            params![FIRST_SPACE],
        )?;
        tx.execute(
            "INSERT INTO space_settings (id, active_space_id, route_grant_ttl_ms)
             VALUES (1, ?1, ?2)",
            params![FIRST_SPACE, DEFAULT_ROUTE_GRANT_TTL_MS],
        )?;
        // Every CLI keeps its own login, so each provider has a default account.
        for provider in Provider::ALL {
            tx.execute(
                "INSERT INTO provider_accounts (provider, id, label, created_at)
                 VALUES (?1, ?2, 'Default account', 0)",
                params![provider, DEFAULT_PROVIDER_ACCOUNT_ID],
            )?;
        }
    }
    tx.commit()
}

fn with_conn<T>(
    store: &SessionStore,
    query: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> Result<T, String> {
    query(&*store.lock_conn()?).map_err(|e| e.to_string())
}

/// An absolute folder as the front end keys it (`pathKey()`, kept in step by
/// `mono_gate::path_key`'s fixture test). A relative path is refused.
pub(crate) fn folder_key(path: &str) -> Result<String, String> {
    let b = path.as_bytes();
    let drive =
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\');
    if drive || path.starts_with('/') || path.starts_with("\\\\") {
        Ok(crate::mono_gate::path_key(path))
    } else {
        Err(format!("Not an absolute folder: {path}"))
    }
}

/// A project key as the front end's `pathKey()` writes it
/// (src/shared/lib/paths.ts): `~` for no project (`selectionKey`), a remote
/// project (`remote://<host>/<path>`, `REMOTE_PATH_PREFIX`) as it is with its
/// trailing slash trimmed, or an absolute local folder.
fn project_key(path: &str) -> Result<String, String> {
    if path == "~" || path.starts_with("remote://") {
        Ok(crate::mono_gate::path_key(path))
    } else {
        folder_key(path)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Space {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) color: Option<String>,
    pub(crate) created_at: i64,
}

pub(crate) fn list(store: &SessionStore) -> Result<Vec<Space>, String> {
    with_conn(store, |conn| {
        conn.prepare("SELECT id, name, color, created_at FROM spaces ORDER BY created_at, id")?
            .query_map([], |row| {
                Ok(Space {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    color: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect()
    })
}

/// A new Space named `space-<n>`, one past the highest number in use, so a
/// Space's branches can live under that name. The schema refuses a name that
/// is empty once trimmed.
pub(crate) fn create(
    store: &SessionStore,
    name: &str,
    color: Option<&str>,
    now: i64,
) -> Result<Space, String> {
    with_conn(store, |conn| {
        let next: i64 = conn.query_row(
            "SELECT COALESCE(MAX(CAST(substr(id, 7) AS INTEGER)), 0) + 1
               FROM spaces WHERE id GLOB 'space-[0-9]*'",
            [],
            |row| row.get(0),
        )?;
        let space = Space {
            id: format!("space-{next}"),
            name: name.trim().to_owned(),
            color: color.map(str::to_owned),
            created_at: now,
        };
        conn.execute(
            "INSERT INTO spaces (id, name, color, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![space.id, space.name, space.color, space.created_at],
        )?;
        Ok(space)
    })
}

pub(crate) fn update(
    store: &SessionStore,
    id: &str,
    name: &str,
    color: Option<&str>,
) -> Result<(), String> {
    let updated = with_conn(store, |conn| {
        conn.execute(
            "UPDATE spaces SET name = ?2, color = ?3 WHERE id = ?1",
            params![id, name.trim(), color],
        )
    })?;
    match updated {
        0 => Err(format!("No Space {id}")),
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceSettings {
    pub(crate) active_space_id: String,
    pub(crate) route_grant_ttl_ms: i64,
}

/// The schema refuses a Space that does not exist and a TTL past
/// `MAX_ROUTE_GRANT_TTL_MS`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsPatch {
    pub(crate) active_space_id: Option<String>,
    pub(crate) route_grant_ttl_ms: Option<i64>,
}

pub(crate) fn settings(store: &SessionStore) -> Result<SpaceSettings, String> {
    with_conn(store, |conn| {
        conn.query_row(
            "SELECT active_space_id, route_grant_ttl_ms FROM space_settings WHERE id = 1",
            [],
            |row| {
                Ok(SpaceSettings {
                    active_space_id: row.get(0)?,
                    route_grant_ttl_ms: row.get(1)?,
                })
            },
        )
    })
}

pub(crate) fn set_settings(
    store: &SessionStore,
    patch: &SettingsPatch,
) -> Result<SpaceSettings, String> {
    with_conn(store, |conn| {
        conn.execute(
            "UPDATE space_settings
                SET active_space_id = COALESCE(?1, active_space_id),
                    route_grant_ttl_ms = COALESCE(?2, route_grant_ttl_ms)
              WHERE id = 1",
            params![patch.active_space_id, patch.route_grant_ttl_ms],
        )
    })?;
    settings(store)
}

/// The Space a project is assigned: the one whose copy is its own folder
/// (`leases::place`) and whose window lists it. A project with no row is in
/// Space 1. It is not every Space the project is in: see
/// [`project_spaces_in`].
pub(crate) fn project_space(store: &SessionStore, project_path: &str) -> Result<String, String> {
    project_space_in(&*store.lock_conn()?, project_path)
}

/// Every Space `project_path` is a project of (ADE-229): the Space it is
/// assigned ([`project_space_in`]), then the Space of each Mono that lists it,
/// so a Mono's own projects are always its projects in its Space. One project
/// may be in several Spaces; each works in its own copy (`leases::place`).
/// This is the rule the app CLI's lists and project check read
/// (`scope::scoped`).
pub(crate) fn project_spaces_in(
    conn: &Connection,
    project_path: &str,
) -> Result<Vec<String>, String> {
    let project_path = project_key(project_path)?;
    let mut spaces = vec![project_space_in(conn, &project_path)?];
    let listed: Vec<(String, String)> = conn
        .prepare(
            "SELECT m.space_id, j.value
               FROM monos m,
                    json_each(CASE WHEN json_valid(m.data_json) THEN m.data_json END, '$.projects') j
              WHERE j.type = 'text'",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .map_err(|e| e.to_string())?;
    for (space, listed) in listed {
        // A Mono's path that is no project key is in no Space.
        if project_key(&listed).as_deref() == Ok(project_path.as_str()) && !spaces.contains(&space)
        {
            spaces.push(space);
        }
    }
    Ok(spaces)
}

/// `project_space` on an open connection.
pub(crate) fn project_space_in(conn: &Connection, project_path: &str) -> Result<String, String> {
    let project_path = project_key(project_path)?;
    let space = conn
        .query_row(
            "SELECT space_id FROM space_projects WHERE project_path = ?1",
            [project_path],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(space.unwrap_or_else(|| FIRST_SPACE.to_owned()))
}

/// Assigns the project to Space `space_id`, out of the Space it was
/// assigned. Its old Space's Monos keep it (`project_spaces_in`); only which
/// Space owns its folder and lists it in the window changes. A Space gets a
/// shared project through its Mono, never through this (ADE-229).
pub(crate) fn set_project_space(
    store: &SessionStore,
    project_path: &str,
    space_id: &str,
) -> Result<(), String> {
    let project_path = project_key(project_path)?;
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)
             ON CONFLICT (project_path) DO UPDATE SET space_id = excluded.space_id",
            params![project_path, space_id],
        )
    })?;
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectLogin {
    pub(crate) space_id: String,
    pub(crate) project_path: String,
    pub(crate) provider: Provider,
    pub(crate) account_id: String,
}

/// The login a project uses in a Space: its own choice, else the Space's
/// login (`space_accounts`), else none.
pub(crate) fn project_login(
    store: &SessionStore,
    space_id: &str,
    project_path: &str,
    provider: Provider,
) -> Result<Option<String>, String> {
    let project_path = project_key(project_path)?;
    with_conn(store, |conn| {
        conn.query_row(
            "SELECT COALESCE(
               (SELECT account_id FROM space_project_accounts
                 WHERE space_id = ?1 AND project_path = ?2 AND provider = ?3),
               (SELECT account_id FROM space_accounts WHERE space_id = ?1 AND provider = ?3))",
            params![space_id, project_path, provider],
            |row| row.get(0),
        )
    })
}

/// Every project's own login choice in one Space (what ADE-171 lists).
pub(crate) fn project_logins(
    store: &SessionStore,
    space_id: &str,
) -> Result<Vec<ProjectLogin>, String> {
    with_conn(store, |conn| {
        conn.prepare(
            "SELECT space_id, project_path, provider, account_id FROM space_project_accounts
              WHERE space_id = ?1 ORDER BY project_path, provider",
        )?
        .query_map([space_id], |row| {
            Ok(ProjectLogin {
                space_id: row.get(0)?,
                project_path: row.get(1)?,
                provider: row.get(2)?,
                account_id: row.get(3)?,
            })
        })?
        .collect()
    })
}

pub(crate) fn set_project_login(store: &SessionStore, login: &ProjectLogin) -> Result<(), String> {
    let project_path = project_key(&login.project_path)?;
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO space_project_accounts (space_id, project_path, provider, account_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (space_id, project_path, provider)
             DO UPDATE SET account_id = excluded.account_id",
            params![
                login.space_id,
                project_path,
                login.provider,
                login.account_id
            ],
        )
    })?;
    Ok(())
}

/// Drops a project's own choice, so it uses the Space's login again.
pub(crate) fn clear_project_login(
    store: &SessionStore,
    space_id: &str,
    project_path: &str,
    provider: Provider,
) -> Result<(), String> {
    let project_path = project_key(project_path)?;
    with_conn(store, |conn| {
        conn.execute(
            "DELETE FROM space_project_accounts
              WHERE space_id = ?1 AND project_path = ?2 AND provider = ?3",
            params![space_id, project_path, provider],
        )
    })?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_list(store: State<'_, SessionStore>) -> Result<Vec<Space>, String> {
    list(&store)
}

#[tauri::command(async)]
pub fn spaces_create(
    store: State<'_, SessionStore>,
    name: String,
    color: Option<String>,
) -> Result<Space, String> {
    create(&store, &name, color.as_deref(), now_millis())
}

#[tauri::command(async)]
pub fn spaces_update(
    store: State<'_, SessionStore>,
    id: String,
    name: String,
    color: Option<String>,
) -> Result<(), String> {
    update(&store, &id, &name, color.as_deref())
}

/// The settings as window `label` reads them: `active_space_id` is the
/// window's own Space ([`window_space`]). A window's first read records the
/// app's active Space as its own.
#[tauri::command(async)]
pub fn spaces_settings_get<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
) -> Result<SpaceSettings, String> {
    let mut windows = window_spaces(&store)?;
    let settings = settings(&store)?;
    let space = windows
        .entry(window.label().to_owned())
        .or_insert(settings.active_space_id)
        .clone();
    Ok(SpaceSettings {
        active_space_id: space,
        ..settings
    })
}

/// Saves `patch`. A Space it names is the calling window's Space from now
/// on, the one that window's new rows join (ADE-244), and the app's active
/// Space, the one a new window opens in.
#[tauri::command(async)]
pub fn spaces_settings_set<R: Runtime>(
    window: WebviewWindow<R>,
    store: State<'_, SessionStore>,
    patch: SettingsPatch,
) -> Result<SpaceSettings, String> {
    let mut windows = window_spaces(&store)?;
    let settings = set_settings(&store, &patch)?;
    let label = window.label().to_owned();
    let space = match patch.active_space_id {
        Some(space) => {
            windows.insert(label, space.clone());
            space
        }
        None => windows
            .entry(label)
            .or_insert(settings.active_space_id.clone())
            .clone(),
    };
    Ok(SpaceSettings {
        active_space_id: space,
        ..settings
    })
}

/// Each window's Space (ADE-209): the one its last switch saved, else the
/// app's active Space when it first read the settings. A window's view shows
/// only what `spaces_settings_get` and `spaces_settings_set` answer it, so
/// this is the Space the window shows, and another window's switch does not
/// change it.
fn window_spaces(store: &SessionStore) -> Result<MutexGuard<'_, HashMap<String, String>>, String> {
    store
        .window_spaces
        .lock()
        .map_err(|_| "Window Spaces unavailable".into())
}

/// The Space window `label` shows; `None` for a window that never read the
/// settings, which shows no Space yet. Each reader picks its own fallback
/// for that: a new row, and an unsaved session's login, take the app's
/// active Space, as on main ([`membership::space_for_row`], ADE-244); the
/// ownerless-helper guard takes Space 1, as every window was before Spaces
/// ([`leases::unsaved_space`], ADE-209).
pub(crate) fn window_space(store: &SessionStore, label: &str) -> Result<Option<String>, String> {
    Ok(window_spaces(store)?.get(label).cloned())
}

/// Forgets a closed window's Space, so a later window never inherits it.
/// `destroy_window`, the end of every workspace window's close, calls it.
pub(crate) fn window_closed(store: &SessionStore, label: &str) {
    if let Ok(mut windows) = window_spaces(store) {
        windows.remove(label);
    }
}

#[tauri::command(async)]
pub fn spaces_project_get(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<String, String> {
    project_space(&store, &project_path)
}

/// Every Space the project is in, the assigned one first
/// ([`project_spaces_in`]). `spaces_project_get` gives only the assigned one.
#[tauri::command(async)]
pub fn spaces_project_spaces(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<Vec<String>, String> {
    project_spaces_in(&*store.lock_conn()?, &project_path)
}

#[tauri::command(async)]
pub fn spaces_project_set(
    store: State<'_, SessionStore>,
    project_path: String,
    space_id: String,
) -> Result<(), String> {
    set_project_space(&store, &project_path, &space_id)
}

#[tauri::command(async)]
pub fn spaces_project_login_get(
    store: State<'_, SessionStore>,
    space_id: String,
    project_path: String,
    provider: Provider,
) -> Result<Option<String>, String> {
    project_login(&store, &space_id, &project_path, provider)
}

#[tauri::command(async)]
pub fn spaces_project_logins_list(
    store: State<'_, SessionStore>,
    space_id: String,
) -> Result<Vec<ProjectLogin>, String> {
    project_logins(&store, &space_id)
}

#[tauri::command(async)]
pub fn spaces_project_login_set(
    store: State<'_, SessionStore>,
    login: ProjectLogin,
) -> Result<(), String> {
    set_project_login(&store, &login)
}

#[tauri::command(async)]
pub fn spaces_project_login_clear(
    store: State<'_, SessionStore>,
    space_id: String,
    project_path: String,
    provider: Provider,
) -> Result<(), String> {
    clear_project_login(&store, &space_id, &project_path, provider)
}

/// What a harness child starts with. The spawn hook may change either.
pub(crate) struct SpawnTarget {
    pub(crate) workdir: PathBuf,
    pub(crate) account: Option<HarnessAccount>,
}

/// What a harness child works for, as the window names it.
#[derive(Clone, Copy)]
pub(crate) enum Child<'a> {
    /// A session's own child, `S` or its numbered child `S#n`.
    Session(&'a str),
    /// A text, catalog, skills or login child keyed `key`, or a one-shot
    /// `harness_exec` (no key): the session the window says it works for.
    /// Without a stored one it runs in `neutral`, a folder that is no project
    /// checkout, unless [`OWNERLESS_KEEPS_CALLER_FOLDER`] names it.
    Helper {
        key: Option<&'a str>,
        owner: Option<&'a str>,
        neutral: &'a Path,
    },
}

impl Child<'_> {
    /// The session the spawn hook acts for: a session child's own key, or a
    /// helper's owner. `None` for a helper that names no owner.
    pub(crate) fn session(&self) -> Option<&str> {
        match *self {
            Child::Session(key) => Some(key),
            Child::Helper { owner, .. } => owner,
        }
    }
}

/// The ownerless-helper rule (permanent, ADE-189): a helper call that names
/// no owner keeps its caller's folder when its child key starts with one of
/// these prefixes, and runs in the neutral folder otherwise. A call with an
/// owner is placed for that owner's Space whether or not its key is listed.
/// These keys stay because some of their callers have no session to name:
/// commit and PR text (the git panels describe the folder they show), the
/// `/` picker's skills of a tab not saved yet, and the Grok text cleanup exec
/// after such a prompt. Titles, branch names and `/btw` pass their session
/// (ADE-189 4/4), so a key only they used is not here.
///
/// Such a call of no stored session is judged ([`leases::unsaved_space`],
/// ADE-209) as the Space its unsaved owner's row would join, which for a tab
/// is the Space of the window that called (ADE-244), or, when it names no
/// owner at all, as that window's Space ([`window_space`]). Its guard is the
/// checkout lease check ([`leases::keep_ownerless`]): it is refused in a
/// checkout another Space holds, and nothing is spawned there. It starts with
/// that Space's login (`account::resolve_in`), not the CLI's own, and an
/// unsaved owner's usage-limit grant is checked as a stored owner's is
/// (`grants::route`).
///
/// The Grok cleanup exec (`grokText.ts` `deleteTextSession`, `sessions
/// delete`) passes `monocode-grok-text` as its key, so it runs where its text
/// child ran: no one has shown that Grok finds a session by id alone. The
/// model-catalog probes are not here: their model lists are the user's, so
/// they run in the neutral folder (ADE-191).
const OWNERLESS_KEEPS_CALLER_FOLDER: [&str; 7] = [
    // Titles, commit, PR and branch text and /btw, when they name no owner,
    // and the Grok text cleanup exec. Cursor's key is `monocode-text`.
    "monocode-claude-text",
    "monocode-codex-text",
    "monocode-text",
    "monocode-grok-text",
    "monocode-opencode-text",
    // The `/` picker's skills of a tab not saved yet.
    "monocode-pi-skills-",
    "monocode-omp-skills-",
];

/// The `helper` field of `harness_spawn` and `harness_exec`: the child is a
/// helper of `owner`. An exec also names `key`, the helper child it acts for;
/// a spawn's key is its own id.
#[derive(Deserialize)]
pub struct Helper {
    pub(crate) owner: Option<String>,
    pub(crate) key: Option<String>,
}

#[cfg(test)]
thread_local! {
    /// Spawns on this thread that passed through the hook, so a scenario can
    /// prove `harness_spawn` calls it.
    pub(crate) static HOOKED_SPAWNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The one Spaces step on every harness child (`harness_spawn`,
/// `harness_exec`). The login is the session's own (a live usage-limit
/// grant's included), else its project's own login in its Space, else its
/// Space's (`account::resolve`); then the grant check (`grants::route`); then
/// the checkout lease (`leases::claim`). The owner a helper names is the
/// window's word: a stored session's Space counts, and the lease claim checks
/// it. `window` is the Space of the window that called ([`window_space`]): a
/// session not saved yet is in it, as its row will be. A helper under the
/// ownerless-helper rule of no stored session is judged as the Space
/// [`leases::unsaved_space`] names instead.
pub(crate) fn prepare_spawn(
    store: &SessionStore,
    child: Child<'_>,
    window: Option<&str>,
    provider: Option<Provider>,
    now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    #[cfg(test)]
    HOOKED_SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    let (no_space, kept) = match child {
        Child::Session(_) => (None, false),
        Child::Helper { key, neutral, .. } => {
            let keeps = key.is_some_and(|key| {
                OWNERLESS_KEEPS_CALLER_FOLDER
                    .iter()
                    .any(|listed| key.starts_with(listed))
            });
            ((!keeps).then_some(neutral), keeps)
        }
    };
    if kept {
        if let Some(space) = leases::unsaved_space(store, child.session(), window)? {
            leases::keep_ownerless(store, &target.workdir, &space)?;
            let target = account::resolve_in(store, &space, child.session(), provider, target)?;
            return match child.session() {
                Some(owner) => grants::route(store, owner, now, target),
                None => Ok(target),
            };
        }
    }
    let Some(session_id) = child.session() else {
        return Ok(SpawnTarget {
            workdir: no_space.map_or(target.workdir, Path::to_path_buf),
            ..target
        });
    };
    let target = account::resolve(store, session_id, window, provider, target)?;
    let target = grants::route(store, session_id, now, target)?;
    leases::claim(store, session_id, target, no_space)
}
