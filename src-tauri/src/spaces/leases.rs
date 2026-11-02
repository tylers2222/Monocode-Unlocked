//! Checkout leases (`checkout_leases`): each working copy has one owner Space.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{folder_key, with_conn, SpawnTarget};
use crate::session_store::{now_millis, SessionStore};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutLease {
    pub(crate) checkout_path: String,
    pub(crate) space_id: String,
    pub(crate) leased_at: i64,
}

pub(crate) fn leases(store: &SessionStore) -> Result<Vec<CheckoutLease>, String> {
    with_conn(store, read_leases)
}

fn read_leases(conn: &Connection) -> rusqlite::Result<Vec<CheckoutLease>> {
    conn.prepare(
        "SELECT checkout_path, space_id, leased_at FROM checkout_leases ORDER BY checkout_path",
    )?
    .query_map([], |row| {
        Ok(CheckoutLease {
            checkout_path: row.get(0)?,
            space_id: row.get(1)?,
            leased_at: row.get(2)?,
        })
    })?
    .collect()
}

/// The lease that holds `path`: the one on it, or on the nearest folder
/// above it, but not on a folder above `checkout`, the git checkout `path`
/// is in: a checkout nested in a leased folder is a checkout of its own. A
/// folder that only contains a leased checkout is not in it. Paths are
/// resolved, in `path_key` form.
pub(crate) fn lease_on<'a>(
    leases: &'a [CheckoutLease],
    path: &str,
    checkout: Option<&str>,
) -> Option<&'a CheckoutLease> {
    leases
        .iter()
        .filter(|lease| within(path, &lease.checkout_path))
        .filter(|lease| checkout.is_none_or(|top| within(&lease.checkout_path, top)))
        .max_by_key(|lease| lease.checkout_path.len())
}

fn within(path: &str, folder: &str) -> bool {
    path == folder
        || path
            .strip_prefix(folder.trim_end_matches('/'))
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Records `space_id` as the checkout's owner. A checkout another Space
/// already owns stays with it: the insert fails on the primary key.
pub(crate) fn take_lease(
    store: &SessionStore,
    checkout_path: &str,
    space_id: &str,
    now: i64,
) -> Result<(), String> {
    let checkout_path = folder_key(checkout_path)?;
    with_conn(store, |conn| {
        conn.execute(
            "INSERT INTO checkout_leases (checkout_path, space_id, leased_at) VALUES (?1, ?2, ?3)",
            params![checkout_path, space_id, now],
        )
    })?;
    Ok(())
}

/// Ends `space_id`'s lease on the checkout. Another Space's lease stays.
pub(crate) fn release_lease(
    store: &SessionStore,
    checkout_path: &str,
    space_id: &str,
) -> Result<(), String> {
    let checkout_path = folder_key(checkout_path)?;
    let released = with_conn(store, |conn| {
        conn.execute(
            "DELETE FROM checkout_leases WHERE checkout_path = ?1 AND space_id = ?2",
            params![checkout_path, space_id],
        )
    })?;
    match released {
        0 => Err(format!("{space_id} holds no lease on {checkout_path}")),
        _ => Ok(()),
    }
}

#[tauri::command(async)]
pub fn spaces_leases_list(store: State<'_, SessionStore>) -> Result<Vec<CheckoutLease>, String> {
    leases(&store)
}

#[tauri::command(async)]
pub fn spaces_lease_take(
    store: State<'_, SessionStore>,
    checkout_path: String,
    space_id: String,
) -> Result<(), String> {
    take_lease(&store, &checkout_path, &space_id, now_millis())
}

#[tauri::command(async)]
pub fn spaces_lease_release(
    store: State<'_, SessionStore>,
    checkout_path: String,
    space_id: String,
) -> Result<(), String> {
    release_lease(&store, &checkout_path, &space_id)
}

/// The folder a child starts in: see [`place`].
pub(super) fn claim(
    store: &SessionStore,
    session_id: &str,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(SpawnTarget {
        workdir: place(store, session_id, &target.workdir, now_millis())?,
        ..target
    })
}

/// The folder `child` (a session, or its numbered child `S#n`) runs in when
/// it asks for `workdir`: `workdir` itself, unless another Space holds the
/// checkout it is in. Then the same folder inside the session's own copy of
/// that checkout ([`crate::worktrees::space_worktree`]), leased to the
/// session's Space. A git checkout no Space holds goes to the session's
/// Space. Refused when another Space holds `workdir` and the session cannot
/// get its own copy. A child of no stored session (a text or catalog helper,
/// or a session not saved yet) has no Space: it takes no lease and stays.
pub(crate) fn place(
    store: &SessionStore,
    child: &str,
    workdir: &Path,
    now: i64,
) -> Result<PathBuf, String> {
    let session_id = crate::harness::child_session(child);
    // A missing folder is the caller's to report.
    let Some(real) = resolve(workdir) else {
        return Ok(workdir.to_path_buf());
    };
    let spot = crate::worktrees::checkout_spot(&real);
    let top = spot.as_ref().and_then(|spot| resolve(&spot.top));
    let top_key = top.as_deref().map(key);
    let held = with_conn(store, |conn| {
        let Some(space) = space_of(conn, session_id)? else {
            return Ok(None);
        };
        let owner = owner_or_take(conn, &key(&real), top_key.as_deref(), &space, now)?;
        Ok(owner.map(|owner| (space, owner)))
    })?;
    let Some((space, owner)) = held.filter(|(space, owner)| owner != space) else {
        return Ok(workdir.to_path_buf());
    };
    let refused = |why: &str| format!("{} is in {owner}'s checkout: {why}", real.display());
    let (Some(spot), Some(top)) = (spot, top) else {
        return Err(refused("it is not a git checkout, so it cannot be copied"));
    };
    let copy = {
        // A turn and a spawn of one session may place it at once.
        let _one = PLACING
            .lock()
            .map_err(|_| "Checkout placement unavailable")?;
        crate::worktrees::space_worktree(&top, &space, session_id)
            .and_then(|copy| resolve(&copy).ok_or_else(|| format!("{} is gone", copy.display())))
            .map_err(|e| refused(&e))?
    };
    let copy_key = key(&copy);
    let copy_owner = with_conn(store, |conn| {
        owner_or_take(conn, &copy_key, Some(&copy_key), &space, now)
    })?;
    if copy_owner.as_deref() != Some(space.as_str()) {
        return Err(refused(&format!("{} is not this Space's", copy.display())));
    }
    if spot.inside.as_os_str().is_empty() {
        return Ok(copy);
    }
    // An untracked folder is not in a new copy.
    let folder = copy.join(&spot.inside);
    std::fs::create_dir_all(&folder).map_err(|e| refused(&e.to_string()))?;
    Ok(folder)
}

static PLACING: Mutex<()> = Mutex::new(());

fn space_of(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT space_id FROM sessions WHERE id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .optional()
}

/// The Space that holds `path`. When none does and `path` is in the git
/// checkout `top`, `space_id` takes `top`.
fn owner_or_take(
    conn: &Connection,
    path: &str,
    top: Option<&str>,
    space_id: &str,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    if let Some(lease) = lease_on(&read_leases(conn)?, path, top) {
        return Ok(Some(lease.space_id.clone()));
    }
    let Some(top) = top else {
        return Ok(None);
    };
    conn.execute(
        "INSERT INTO checkout_leases (checkout_path, space_id, leased_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (checkout_path) DO NOTHING",
        params![top, space_id, now],
    )?;
    conn.query_row(
        "SELECT space_id FROM checkout_leases WHERE checkout_path = ?1",
        [top],
        |row| row.get(0),
    )
    .map(Some)
}

/// `path` with every link resolved, without the `\\?\` prefix Windows
/// `canonicalize` adds, which git and the front end never write.
fn resolve(path: &Path) -> Option<PathBuf> {
    let real = path.canonicalize().ok()?;
    let plain = {
        let text = real.to_string_lossy();
        if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
            Some(format!(r"\\{share}"))
        } else {
            text.strip_prefix(r"\\?\").map(str::to_owned)
        }
    };
    Some(plain.map_or(real, PathBuf::from))
}

fn key(path: &Path) -> String {
    crate::mono_gate::path_key(&crate::fs::path_to_js(path))
}
