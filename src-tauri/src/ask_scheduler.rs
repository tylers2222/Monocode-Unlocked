//! The ask scheduler (Change R plan v2, R8). Each active clock tick turns
//! the open asks that fell due in active time into rows of `mono_outbox`,
//! one row per ask and stage, keyed `reminder:<ask>:<stage>`. Nothing lives
//! in memory, so a restart finds the pending reminders in the database.

use rusqlite::{params, Connection, Transaction, TransactionBehavior};

/// One stage of an open ask's ladder: its outbox name and when it falls
/// due, relative to the ask's deadline. Every rung here goes to the child.
struct Rung {
    stage: &'static str,
    from_due_ms: i64,
}

/// The ladder in order. `mono_asks.stage` counts the rungs already enqueued:
/// 0 is no reminder yet, 1 is `t5` enqueued.
const LADDER: &[Rung] = &[Rung {
    stage: "t5",
    from_due_ms: -5 * 60_000,
}];

/// `ask_id` is nullable: a later `resume:<session>:<boot>` row has no ask.
pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mono_outbox (
           key          TEXT PRIMARY KEY,
           ask_id       TEXT REFERENCES mono_asks(id),
           stage        TEXT NOT NULL CHECK (stage IN
                          ('t5', 'deadline', 'why', 'blocked', 'resume')),
           recipient    TEXT NOT NULL,
           at_active_ms INTEGER NOT NULL,
           claimed_by   TEXT,
           sent_at      INTEGER
         );",
    )
}

/// The next rung for an ask that has `done` rungs enqueued.
fn next_rung(done: i64) -> Option<&'static Rung> {
    LADDER.get(usize::try_from(done).ok()?)
}

/// When the next rung of an ask due at `due_active_ms` with `done` rungs
/// enqueued falls due; `None` past the last rung.
pub(crate) fn next_due(due_active_ms: i64, done: i64) -> Option<i64> {
    next_rung(done).map(|rung| due_active_ms + rung.from_due_ms)
}

/// Enqueues the reminder of each open ask whose next rung fell due by
/// `active_ms`, and moves the ask to the rung after it. A paused or untimed
/// ask has no next due, so it is never selected. A key enqueued before (its
/// stage write lost) keeps its one row; an ask that a later build moved past
/// the end of this ladder is left alone.
pub(crate) fn run(conn: &Connection, active_ms: i64) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let fell_due: Vec<(String, i64, i64, String)> = tx
        .prepare(
            "SELECT id, stage, due_active_ms, child_actor FROM mono_asks
              WHERE state = 'open' AND next_due_active_ms <= ?1",
        )?
        .query_map([active_ms], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, done, due_active_ms, child) in fell_due {
        let Some(rung) = next_rung(done) else {
            continue;
        };
        let key = format!("reminder:{id}:{}", rung.stage);
        tx.execute(
            "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (key) DO NOTHING",
            params![key, id, rung.stage, child, active_ms],
        )?;
        tx.execute(
            "UPDATE mono_asks SET stage = ?1, next_due_active_ms = ?2 WHERE id = ?3",
            params![done + 1, next_due(due_active_ms, done + 1), id],
        )?;
    }
    tx.commit()
}

/// Plan R8, when `SessionStore` opens: no window outlives the app, so a
/// reminder that a window claimed and did not send is pending again.
pub(crate) fn startup(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mono_outbox SET claimed_by = NULL
          WHERE sent_at IS NULL AND claimed_by IS NOT NULL",
        [],
    )?;
    Ok(())
}
