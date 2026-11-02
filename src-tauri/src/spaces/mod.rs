//! Spaces (ADE-168): one window holds several Spaces, each with its own
//! logins, Monos, projects and sessions. This module owns the schema, the
//! storage commands and the one spawn hook every harness child passes through. Each concern the hook
//! applies lives in its own file: `account` (the Space's login), `grants`
//! (usage-limit routing) and `leases` (one Space per checkout).

pub(crate) mod account;
pub(crate) mod grants;
pub(crate) mod leases;

use std::path::PathBuf;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, ToSql, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::harness::{HarnessAccount, DEFAULT_PROVIDER_ACCOUNT_ID};
use crate::session_store::{ensure_column, SessionStore};

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
    if drive || path.starts_with(['/', '\\']) {
        Ok(crate::mono_gate::path_key(path))
    } else {
        Err(format!("Not an absolute folder: {path}"))
    }
}

/// What a harness child starts with. The spawn hook may change either.
pub(crate) struct SpawnTarget {
    pub(crate) workdir: PathBuf,
    pub(crate) account: Option<HarnessAccount>,
}

#[cfg(test)]
thread_local! {
    /// Spawns on this thread that passed through the hook, so a scenario can
    /// prove `harness_spawn` calls it.
    pub(crate) static HOOKED_SPAWNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The one Spaces step on every harness spawn (`harness_spawn`): the Space's
/// login, then any usage-limit grant on it, then the checkout lease.
pub(crate) fn prepare_spawn(
    store: &SessionStore,
    session_id: &str,
    provider: Option<Provider>,
    now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    #[cfg(test)]
    HOOKED_SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    let target = account::resolve(store, session_id, provider, target)?;
    let target = grants::route(store, session_id, now, target)?;
    leases::claim(store, session_id, target)
}
