//! Spaces (ADE-168): one window holds several Spaces, each with its own
//! logins, Monos, projects and sessions. This module owns the schema, the
//! storage commands and the one spawn hook every harness child passes through.
//! Each concern the hook applies lives in its own file: `account` (the
//! Space's login), `grants` (usage-limit routing) and `leases` (one Space per
//! checkout).

pub(crate) mod account;
pub(crate) mod grants;
pub(crate) mod leases;

use std::path::PathBuf;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::harness::HarnessAccount;
use crate::session_store::{ensure_column, now_millis, SessionStore};

/// The Space every row that predates Spaces belongs to.
pub(crate) const FIRST_SPACE: &str = "space-1";

const DEFAULT_ROUTE_GRANT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

/// The CLIs that keep named logins (`PROVIDER_ACCOUNT_PROVIDERS` in TS).
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
}

impl ToSql for Provider {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Provider {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::ALL
            .into_iter()
            .find(|provider| provider.as_str() == text)
            .ok_or_else(|| FromSqlError::Other(format!("unknown provider {text}").into()))
    }
}

/// One migration for every Spaces table, safe to run on each open. Rows that
/// predate Spaces land in Space 1: Monos and sessions through the column
/// default, projects through a one-time backfill from the sessions' folders.
pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let fresh: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'spaces')",
        [],
        |row| row.get(0),
    )?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS spaces (
           id TEXT PRIMARY KEY
             CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^A-Za-z0-9-]*'),
           name TEXT NOT NULL,
           color TEXT,
           created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS space_settings (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           active_space_id TEXT NOT NULL REFERENCES spaces(id),
           route_grant_ttl_ms INTEGER NOT NULL CHECK (route_grant_ttl_ms > 0)
         );
         CREATE TABLE IF NOT EXISTS space_projects (
           project_path TEXT PRIMARY KEY,
           space_id TEXT NOT NULL REFERENCES spaces(id)
         );
         CREATE TABLE IF NOT EXISTS provider_accounts (
           provider TEXT NOT NULL CHECK (provider IN ('claude', 'codex', 'devin')),
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
         );",
    )?;
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
        tx.execute(
            "INSERT INTO space_projects (project_path, space_id)
             SELECT DISTINCT cwd, ?1 FROM sessions",
            params![FIRST_SPACE],
        )?;
        // Every CLI keeps its own login, so each provider has a default account.
        for provider in Provider::ALL {
            tx.execute(
                "INSERT INTO provider_accounts (provider, id, label, created_at)
                 VALUES (?1, 'default', 'Default account', 0)",
                params![provider],
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Space {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) color: Option<String>,
    pub(crate) created_at: i64,
}

pub(crate) fn list(conn: &Connection) -> rusqlite::Result<Vec<Space>> {
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
}

/// A new Space named `space-<n>`, one past the highest number in use, so a
/// Space's branches can live under that name.
pub(crate) fn create(
    conn: &Connection,
    name: &str,
    color: Option<&str>,
    now: i64,
) -> rusqlite::Result<Space> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(CAST(substr(id, 7) AS INTEGER)), 0) + 1
           FROM spaces WHERE id GLOB 'space-[0-9]*'",
        [],
        |row| row.get(0),
    )?;
    let space = Space {
        id: format!("space-{next}"),
        name: name.to_owned(),
        color: color.map(str::to_owned),
        created_at: now,
    };
    conn.execute(
        "INSERT INTO spaces (id, name, color, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![space.id, space.name, space.color, space.created_at],
    )?;
    Ok(space)
}

pub(crate) fn update(
    conn: &Connection,
    id: &str,
    name: &str,
    color: Option<&str>,
) -> rusqlite::Result<()> {
    match conn.execute(
        "UPDATE spaces SET name = ?2, color = ?3 WHERE id = ?1",
        params![id, name, color],
    )? {
        0 => Err(rusqlite::Error::QueryReturnedNoRows),
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceSettings {
    pub(crate) active_space_id: String,
    pub(crate) route_grant_ttl_ms: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsPatch {
    pub(crate) active_space_id: Option<String>,
    pub(crate) route_grant_ttl_ms: Option<i64>,
}

pub(crate) fn settings(conn: &Connection) -> rusqlite::Result<SpaceSettings> {
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
}

pub(crate) fn set_settings(
    conn: &Connection,
    patch: &SettingsPatch,
) -> rusqlite::Result<SpaceSettings> {
    conn.execute(
        "UPDATE space_settings
            SET active_space_id = COALESCE(?1, active_space_id),
                route_grant_ttl_ms = COALESCE(?2, route_grant_ttl_ms)
          WHERE id = 1",
        params![patch.active_space_id, patch.route_grant_ttl_ms],
    )?;
    settings(conn)
}

/// The Space a project folder belongs to, `None` for a folder never assigned.
pub(crate) fn project_space(
    conn: &Connection,
    project_path: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT space_id FROM space_projects WHERE project_path = ?1",
        [project_path],
        |row| row.get(0),
    )
    .optional()
}

pub(crate) fn set_project_space(
    conn: &Connection,
    project_path: &str,
    space_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO space_projects (project_path, space_id) VALUES (?1, ?2)
         ON CONFLICT (project_path) DO UPDATE SET space_id = excluded.space_id",
        params![project_path, space_id],
    )?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_list(store: State<'_, SessionStore>) -> Result<Vec<Space>, String> {
    with_conn(&store, list)
}

#[tauri::command(async)]
pub fn spaces_create(
    store: State<'_, SessionStore>,
    name: String,
    color: Option<String>,
) -> Result<Space, String> {
    with_conn(&store, |conn| {
        create(conn, &name, color.as_deref(), now_millis())
    })
}

#[tauri::command(async)]
pub fn spaces_update(
    store: State<'_, SessionStore>,
    id: String,
    name: String,
    color: Option<String>,
) -> Result<(), String> {
    with_conn(&store, |conn| update(conn, &id, &name, color.as_deref()))
}

#[tauri::command(async)]
pub fn spaces_settings_get(store: State<'_, SessionStore>) -> Result<SpaceSettings, String> {
    with_conn(&store, settings)
}

#[tauri::command(async)]
pub fn spaces_settings_set(
    store: State<'_, SessionStore>,
    patch: SettingsPatch,
) -> Result<SpaceSettings, String> {
    with_conn(&store, |conn| set_settings(conn, &patch))
}

#[tauri::command(async)]
pub fn spaces_project_get(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<Option<String>, String> {
    with_conn(&store, |conn| project_space(conn, &project_path))
}

#[tauri::command(async)]
pub fn spaces_project_set(
    store: State<'_, SessionStore>,
    project_path: String,
    space_id: String,
) -> Result<(), String> {
    with_conn(&store, |conn| {
        set_project_space(conn, &project_path, &space_id)
    })
}

/// What a harness child starts with. The spawn hook may change either.
pub(crate) struct SpawnTarget {
    pub(crate) workdir: PathBuf,
    pub(crate) account: Option<HarnessAccount>,
}

/// The one Spaces step on every harness spawn (`harness_spawn`): the Space's
/// login, then any usage-limit grant on it, then the checkout lease.
pub(crate) fn prepare_spawn(
    store: &SessionStore,
    session_id: &str,
    provider: Option<&str>,
    now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    let target = account::resolve(store, session_id, provider, target)?;
    let target = grants::route(store, session_id, now, target)?;
    leases::claim(store, session_id, target)
}
