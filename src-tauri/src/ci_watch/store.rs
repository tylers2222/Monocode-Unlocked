//! The `ci_watches` rows. A watch is open while `outcome` is NULL; its end
//! writes the wake text and one `mono_outbox` row, keyed `ci-<id>`, in the
//! same transaction, so a restart loses neither.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};

use super::judge::{judge, Judged, Snapshot, Watch};
use crate::ask_gate::Actor;
use crate::mono_messages::Notices;

/// The poll reads each PR at most once in this much wall time.
pub(crate) const POLL_MS: i64 = 60_000;

/// One poll pass reads at most this many PRs. The thread passes every 15 s,
/// so the app makes at most 8 GraphQL reads a minute, 480 an hour, for all
/// watches together.
pub(crate) const READS_PER_PASS: i64 = 2;

/// At most this many watches are open at once, over all callers.
pub(crate) const OPEN_CAP: i64 = 30;

/// An ended watch, and its sent wake, are deleted this long after the end.
const KEEP_ENDED_MS: i64 = 7 * 24 * 60 * 60_000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ci_watches (
           id              TEXT PRIMARY KEY,
           repo            TEXT NOT NULL,
           pr              INTEGER NOT NULL,
           sha             TEXT NOT NULL,
           required        TEXT NOT NULL,
           recipient       TEXT NOT NULL,
           registered_by   TEXT NOT NULL,
           request_id      TEXT NOT NULL,
           created_wall_ms INTEGER NOT NULL,
           polled_wall_ms  INTEGER,
           settled         TEXT,
           settled_wall_ms INTEGER,
           outcome         TEXT,
           text            TEXT,
           ended_wall_ms   INTEGER,
           UNIQUE (registered_by, request_id)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS ci_watches_open
           ON ci_watches (repo, pr, sha, recipient) WHERE outcome IS NULL;",
    )
}

/// A new watch, as `ci.watch` checked it.
pub(super) struct New<'a> {
    pub repo: &'a str,
    pub pr: i64,
    pub sha: &'a str,
    pub required: &'a [String],
    pub recipient: &'a Actor,
    pub by: &'a Actor,
    pub request_id: &'a str,
}

/// What `register` did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Registered {
    New(String),
    /// The same request again, or an open watch of the same PR, sha and
    /// recipient.
    Before(String),
    /// `OPEN_CAP` watches are open.
    Full,
}

/// Registers `new` at wall time `wall`.
pub(super) fn register(conn: &Connection, new: &New, wall: i64) -> rusqlite::Result<Registered> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let before: Option<String> = tx
        .query_row(
            "SELECT id FROM ci_watches WHERE registered_by = ?1 AND request_id = ?2
             UNION ALL
             SELECT id FROM ci_watches WHERE repo = ?3 AND pr = ?4 AND sha = ?5
               AND recipient = ?6 AND outcome IS NULL
             LIMIT 1",
            params![
                new.by.to_string(),
                new.request_id,
                new.repo,
                new.pr,
                new.sha,
                new.recipient.to_string()
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = before {
        return Ok(Registered::Before(id));
    }
    let open: i64 = tx.query_row(
        "SELECT count(*) FROM ci_watches WHERE outcome IS NULL",
        [],
        |row| row.get(0),
    )?;
    if open >= OPEN_CAP {
        return Ok(Registered::Full);
    }
    let id = format!("cw_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    tx.execute(
        "INSERT INTO ci_watches (id, repo, pr, sha, required, recipient, registered_by,
           request_id, created_wall_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            new.repo,
            new.pr,
            new.sha,
            Value::from(new.required).to_string(),
            new.recipient.to_string(),
            new.by.to_string(),
            new.request_id,
            wall
        ],
    )?;
    tx.commit()?;
    Ok(Registered::New(id))
}

/// The open watches `caller` registered or is woken by.
pub(super) fn list(conn: &Connection, caller: &Actor) -> rusqlite::Result<Vec<Value>> {
    conn.prepare(
        "SELECT id, repo, pr, sha, recipient, registered_by, created_wall_ms, polled_wall_ms
           FROM ci_watches WHERE outcome IS NULL AND (registered_by = ?1 OR recipient = ?1)
          ORDER BY created_wall_ms, id",
    )?
    .query_map([caller.to_string()], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "repo": row.get::<_, String>(1)?,
            "pr": row.get::<_, i64>(2)?,
            "sha": row.get::<_, String>(3)?,
            "notify": row.get::<_, String>(4)?,
            "registeredBy": row.get::<_, String>(5)?,
            "createdAt": row.get::<_, i64>(6)?,
            "polledAt": row.get::<_, Option<i64>>(7)?,
        }))
    })?
    .collect()
}

/// Why a cancel was refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CancelRefused {
    Unknown,
    NotYours,
    Ended(String),
}

/// Ends watch `id` with no wake, for `caller` who registered it or is woken by it.
pub(super) fn cancel(
    conn: &Connection,
    caller: &Actor,
    id: &str,
    wall: i64,
) -> rusqlite::Result<Result<(), CancelRefused>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT recipient, registered_by, outcome FROM ci_watches WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let caller = caller.to_string();
    let refused = match row {
        None => Some(CancelRefused::Unknown),
        Some((recipient, by, _)) if recipient != caller && by != caller => {
            Some(CancelRefused::NotYours)
        }
        Some((_, _, Some(outcome))) => Some(CancelRefused::Ended(outcome)),
        Some(_) => None,
    };
    if let Some(refused) = refused {
        return Ok(Err(refused));
    }
    tx.execute(
        "UPDATE ci_watches SET outcome = 'cancelled', ended_wall_ms = ?2 WHERE id = ?1",
        params![id, wall],
    )?;
    tx.commit()?;
    Ok(Ok(()))
}

/// Up to `READS_PER_PASS` PRs with an open watch that was not read in the
/// last `POLL_MS`, each once, the one read longest ago first.
pub(crate) fn due(conn: &Connection, wall: i64) -> rusqlite::Result<Vec<(String, i64)>> {
    conn.prepare(
        "SELECT repo, pr FROM ci_watches
          WHERE outcome IS NULL AND (polled_wall_ms IS NULL OR polled_wall_ms <= ?1)
          GROUP BY repo, pr
          ORDER BY MIN(COALESCE(polled_wall_ms, 0)), MIN(created_wall_ms)
          LIMIT ?2",
    )?
    .query_map(params![wall - POLL_MS, READS_PER_PASS], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?
    .collect()
}

/// Marks the open watches of `repo` PR `pr` read at `wall`, when their
/// `apply` failed, so the PR waits its turn and the next PR is read.
pub(crate) fn mark_read(
    conn: &Connection,
    (repo, pr): (&str, i64),
    wall: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ci_watches SET polled_wall_ms = ?3
          WHERE repo = ?1 AND pr = ?2 AND outcome IS NULL",
        params![repo, pr, wall],
    )?;
    Ok(())
}

/// Deletes each watch that ended `KEEP_ENDED_MS` or more before `wall`, with
/// its wake once that wake was sent (a wake not sent keeps its watch, whose
/// text it reads).
pub(crate) fn prune(conn: &Connection, wall: i64) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let old = "SELECT cw.id FROM ci_watches cw
                 LEFT JOIN mono_outbox o ON o.key = 'ci-' || cw.id
                WHERE cw.outcome IS NOT NULL AND cw.ended_wall_ms <= ?1
                  AND (o.key IS NULL OR o.sent_at IS NOT NULL)";
    let ids = tx
        .prepare(old)?
        .query_map([wall - KEEP_ENDED_MS], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        tx.execute("DELETE FROM mono_outbox WHERE key = 'ci-' || ?1", [id])?;
        tx.execute("DELETE FROM ci_watches WHERE id = ?1", [id])?;
    }
    tx.commit()
}

/// Judges every open watch of `repo` PR `pr` on one read of it. A watch
/// that ends gets its wake as an outbox row, and its recipient's chat is
/// named so the window that owns it delivers the wake.
pub(crate) fn apply(
    conn: &Connection,
    (repo, pr): (&str, i64),
    read: &Result<Snapshot, String>,
    (active, wall): (i64, i64),
) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let watches = tx
        .prepare(
            "SELECT id, sha, required, created_wall_ms, settled, recipient, settled_wall_ms
               FROM ci_watches WHERE repo = ?1 AND pr = ?2 AND outcome IS NULL",
        )?
        .query_map(params![repo, pr], |row| {
            let required: String = row.get(2)?;
            Ok((
                Watch {
                    id: row.get(0)?,
                    repo: repo.to_string(),
                    pr,
                    sha: row.get(1)?,
                    required: serde_json::from_str(&required).unwrap_or_default(),
                    created_wall_ms: row.get(3)?,
                    settled: row.get(4)?,
                    settled_wall_ms: row.get(6)?,
                },
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    for (watch, recipient) in watches {
        match judge(&watch, read, wall) {
            Judged::Wait { settled } => {
                tx.execute(
                    "UPDATE ci_watches SET polled_wall_ms = ?2, settled = ?3,
                       settled_wall_ms = CASE
                         WHEN ?3 IS NULL THEN NULL
                         WHEN settled IS ?3 AND settled_wall_ms IS NOT NULL THEN settled_wall_ms
                         ELSE ?2 END
                     WHERE id = ?1",
                    params![watch.id, wall, settled],
                )?;
            }
            Judged::End { outcome, text } => {
                tx.execute(
                    "UPDATE ci_watches SET polled_wall_ms = ?2, outcome = ?3, text = ?4,
                       ended_wall_ms = ?2 WHERE id = ?1",
                    params![watch.id, wall, outcome.as_str(), text],
                )?;
                tx.execute(
                    "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
                     VALUES (?1, NULL, 'ci', ?2, ?3) ON CONFLICT (key) DO NOTHING",
                    params![format!("ci-{}", watch.id), recipient, active],
                )?;
                crate::ask_scheduler::name_chat(&tx, &mut notices, &recipient)?;
            }
        }
    }
    tx.commit()?;
    Ok(notices)
}
