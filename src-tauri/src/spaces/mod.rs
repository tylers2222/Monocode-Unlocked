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

use std::path::{Path, PathBuf};

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tauri::State;

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
/// Projects need no backfill: `space_projects` holds only the folders moved
/// out of Space 1, so a folder with no row is in Space 1. Project paths are
/// stored in the front end's `pathKey()` form.
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

/// The Space a project belongs to. A project with no row is in Space 1: the
/// one rule every reader goes through.
pub(crate) fn project_space(store: &SessionStore, project_path: &str) -> Result<String, String> {
    project_space_in(&*store.lock_conn()?, project_path)
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

#[tauri::command(async)]
pub fn spaces_settings_get(store: State<'_, SessionStore>) -> Result<SpaceSettings, String> {
    settings(&store)
}

#[tauri::command(async)]
pub fn spaces_settings_set(
    store: State<'_, SessionStore>,
    patch: SettingsPatch,
) -> Result<SpaceSettings, String> {
    set_settings(&store, &patch)
}

#[tauri::command(async)]
pub fn spaces_project_get(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<String, String> {
    project_space(&store, &project_path)
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
pub(crate) enum Child<'a> {
    /// A session's own child, `S` or its numbered child `S#n`.
    Session(&'a str),
    /// A text, catalog, skills or login child keyed `key`, or a one-shot
    /// `harness_exec` (no key): the session the window says it works for.
    /// Without a stored one it runs in `neutral`, a folder that is no project
    /// checkout, unless [`KEEPS_CALLER_FOLDER`] names it.
    Helper {
        key: Option<&'a str>,
        owner: Option<&'a str>,
        neutral: &'a Path,
    },
}

/// INTERIM (ADE-189 4/4): every helper that still keeps its caller's
/// folder, in one place. Each ran in its caller's project folder before
/// ADE-189, and its owner is not wired yet, so it keeps that folder whenever it
/// has no Space, as a session's own child does, and keeps its project
/// context. No Space 2 session exists until ADE-179, so none of them lets a
/// Space into another's checkout today. ADE-191 (the catalog probes) and
/// ADE-192 (the owner of the Claude, Cursor, Grok and OpenCode text children)
/// shrink this, and part 4/4 deletes it. Every other helper of no stored
/// session runs in the neutral folder.
///
/// - [`KEEPS_CALLER_FOLDER`]: helpers matched by a prefix of their child key,
///   including the Grok text cleanup exec (`grokText.ts`
///   `deleteTextSession`, `sessions delete`). That exec passes
///   `monocode-grok-text` as its key, so it runs where its text child ran: no
///   one has shown that Grok finds a session by id alone.
/// - Still spawned as `spawnChild` (a session's own child, which keeps its
///   folder by ADE-173's rule): the Devin, Codex and Hermes catalog probes
///   (`devinCatalog.ts`, `codexCatalog.ts`, `hermesCatalog.ts`, the home dir).
const KEEPS_CALLER_FOLDER: [&str; 10] = [
    // Titles, commit, PR and branch text, /btw side questions, and the Grok
    // text cleanup exec. Claude, Cursor (`monocode-text`), Grok and OpenCode
    // text pass no owner yet: owner wiring: ADE-192.
    "monocode-claude-text",
    "monocode-codex-text",
    "monocode-text",
    "monocode-grok-text",
    "monocode-opencode-text",
    "monocode-pi-text",
    "monocode-omp-text",
    "monocode-devin-text-",
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
/// `harness_exec`): the Space's login, then any usage-limit grant on it, then
/// the checkout lease. The owner a helper names is the window's word: only a
/// stored session's Space counts, and the lease claim is the one place that
/// checks it.
pub(crate) fn prepare_spawn(
    store: &SessionStore,
    child: Child<'_>,
    provider: Option<Provider>,
    now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    #[cfg(test)]
    HOOKED_SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    let (session_id, no_space) = match child {
        Child::Session(key) => (key, None),
        Child::Helper {
            key,
            owner,
            neutral,
        } => {
            let keeps = key.is_some_and(|key| {
                KEEPS_CALLER_FOLDER
                    .iter()
                    .any(|listed| key.starts_with(listed))
            });
            let no_space = (!keeps).then_some(neutral);
            let Some(owner) = owner else {
                return Ok(SpawnTarget {
                    workdir: no_space.map_or(target.workdir, Path::to_path_buf),
                    ..target
                });
            };
            (owner, no_space)
        }
    };
    let target = account::resolve(store, session_id, provider, target)?;
    let target = grants::route(store, session_id, now, target)?;
    leases::claim(store, session_id, target, no_space)
}
