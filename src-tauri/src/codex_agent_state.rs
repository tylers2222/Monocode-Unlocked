//! A Mono's closed agents (ADE-224): Codex keeps agent closure only in its
//! SQLite state database, so a copy into a Mono home saves the closed edges of
//! the login's database as a snapshot, and the first private start restores it.

use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};

use crate::codex_mono_store::validate_thread_id;
use crate::mono_home::Dir;

// Rollouts reconstruct the graph, but explicit agent closure lives only in
// SQLite. Apply that small snapshot after private app-server initialization
// and before it restores agent identities while resuming the root.
#[derive(Serialize, Deserialize)]
pub(crate) struct AgentEdge {
    pub(crate) parent: String,
    pub(crate) child: String,
    pub(crate) status: String,
}

/// The folder of a Mono's agent closure snapshots, `<thread id>.json`.
pub(crate) const AGENT_STATE: &str = ".monocode-agent-state";

/// The newest regular `state_N.sqlite` in `home`: an agent may write the home
/// and a linked login, so a link, folder or FIFO it plants is never picked. The
/// path is under the home's real path, as SQLITE_OPEN_NOFOLLOW refuses a link at
/// any folder on it (macOS's /var, a linked login or home).
fn state_db(home: &Path) -> Result<Option<PathBuf>, String> {
    let home = match home.canonicalize() {
        Ok(home) => home,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut databases = Vec::new();
    for entry in std::fs::read_dir(home).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(version) = name
            .strip_prefix("state_")
            .and_then(|n| n.strip_suffix(".sqlite"))
            .and_then(|n| n.parse::<u32>().ok())
        {
            if entry.file_type().map_err(|e| e.to_string())?.is_file() {
                databases.push((version, entry.path()));
            }
        }
    }
    Ok(databases
        .into_iter()
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path))
}

/// The files SQLite may open beside a database: the first read opens a hot
/// `-journal` read-only, and a WAL database opens `-wal` and `-shm`. A FIFO an
/// agent plants at one blocks that open with no writer.
const SIDECARS: [&str; 3] = ["-journal", "-wal", "-shm"];

/// The state database `state_db` picked, refusing a link swapped in at its
/// name, and anything but a file at a name beside it.
pub(crate) fn open_state(path: &Path, access: OpenFlags) -> Result<rusqlite::Connection, String> {
    for sidecar in SIDECARS {
        let mut name = path.as_os_str().to_owned();
        name.push(sidecar);
        if std::fs::symlink_metadata(&name).is_ok_and(|meta| !meta.is_file()) {
            return Err(format!("{} is not a file", Path::new(&name).display()));
        }
    }
    let flags = access.union(OpenFlags::SQLITE_OPEN_NOFOLLOW);
    let db = rusqlite::Connection::open_with_flags(path, flags).map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(db)
}

pub(crate) fn closed_agent_edges(home: &Path, thread_id: &str) -> Result<Vec<AgentEdge>, String> {
    validate_thread_id(thread_id)?;
    let Some(path) = state_db(home)? else {
        return Ok(Vec::new());
    };
    let db = open_state(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='thread_spawn_edges')", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut statement = db.prepare("WITH RECURSIVE subtree(child) AS (SELECT child_thread_id FROM thread_spawn_edges WHERE parent_thread_id = ?1 UNION SELECT edge.child_thread_id FROM thread_spawn_edges edge JOIN subtree ON edge.parent_thread_id = subtree.child) SELECT parent_thread_id, child_thread_id, status FROM thread_spawn_edges WHERE child_thread_id IN subtree AND status <> 'open'").map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([thread_id], |row| {
            Ok(AgentEdge {
                parent: row.get(0)?,
                child: row.get(1)?,
                status: row.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub(crate) fn restore_agent_edges(home: &Path, thread_id: &str) -> Result<(), String> {
    let snapshot = format!("{thread_id}.json");
    let Some(state) = Dir::open(home, &[AGENT_STATE])? else {
        return Ok(());
    };
    let Some(bytes) = state.read(&snapshot)? else {
        return Ok(());
    };
    let edges: Vec<AgentEdge> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let path = state_db(home)?.ok_or("Codex has not initialized the private Mono index")?;
    let mut db = open_state(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    for edge in edges {
        validate_thread_id(&edge.parent)?;
        validate_thread_id(&edge.child)?;
        tx.execute("INSERT INTO thread_spawn_edges(parent_thread_id, child_thread_id, status) VALUES (?1, ?2, ?3) ON CONFLICT(child_thread_id) DO UPDATE SET status=excluded.status WHERE parent_thread_id=excluded.parent_thread_id", rusqlite::params![edge.parent, edge.child, edge.status]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    state.remove(&snapshot)
}
