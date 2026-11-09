//! ADE-278: the level-1 message to a provisioner goes at most once per
//! stalled agent per 15 active minutes, unless its cause changed.
//!
//! The limit holds that message and nothing else. A held episode has its row,
//! its level-1 send row and its escalation clock as any other, so `route::run`
//! escalates it on the same schedule and Tyler's notification is never held.
//!
//! The data: one row per agent and cause it was last alarmed for.
//! - `sent_active_ms` starts that cause's window. A cause with no send in the
//!   last 15 minutes is a changed cause, so causes that take turns cannot send
//!   more than one alarm each per window.
//! - `held_count` is the alarms held since the agent's last send. The next
//!   send of any cause carries the agent's total.
//! - `held_episode` is the open episode whose message waits for `release_due`.
//!   An agent has one open episode, so at most one of its rows names a live one.

use rusqlite::{params, Connection};

use super::route::{self, EpisodeRow};
use super::store::{Settings, MINUTE_MS};
use crate::mono_messages::Notices;

const WINDOW_MS: i64 = 15 * MINUTE_MS;

pub(crate) fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alarm_repeat_limit (
           actor          TEXT NOT NULL,
           cause          TEXT NOT NULL,
           sent_active_ms INTEGER NOT NULL,
           held_count     INTEGER NOT NULL DEFAULT 0,
           held_episode   TEXT,
           PRIMARY KEY (actor, cause)
         );",
    )
}

/// Holds `episode`'s level-1 message if its agent was alarmed for this cause
/// inside the window. A send stamped ahead of the clock holds nothing.
pub(super) fn holds(tx: &Connection, episode: &EpisodeRow, active: i64) -> rusqlite::Result<bool> {
    let held = tx.execute(
        "UPDATE alarm_repeat_limit SET held_count = held_count + 1, held_episode = ?3
          WHERE actor = ?1 AND cause = ?2 AND sent_active_ms <= ?4 AND ?4 - sent_active_ms < ?5",
        params![
            episode.actor.to_string(),
            episode.cause,
            episode.id,
            active,
            WINDOW_MS
        ],
    )?;
    Ok(held == 1)
}

/// The alarms the agent's next send carries.
pub(super) fn grouped(conn: &Connection, actor: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(held_count), 0) FROM alarm_repeat_limit WHERE actor = ?1",
        [actor],
        |row| row.get(0),
    )
}

/// A level-1 message goes now: its cause's window starts, and the count it
/// carries is spent.
pub(super) fn sent(tx: &Connection, episode: &EpisodeRow, active: i64) -> rusqlite::Result<()> {
    let actor = episode.actor.to_string();
    tx.execute(
        "UPDATE alarm_repeat_limit SET held_count = 0, held_episode = NULL WHERE actor = ?1",
        [&actor],
    )?;
    tx.execute(
        "INSERT INTO alarm_repeat_limit (actor, cause, sent_active_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (actor, cause) DO UPDATE SET sent_active_ms = excluded.sent_active_ms",
        params![actor, episode.cause, active],
    )?;
    Ok(())
}

/// One held message: tells its provisioner when the window is over, or when
/// the episode is about to escalate past it. An episode that closed, was acted
/// on or went up needs no message; its count stays for the next send.
fn release(
    tx: &Connection,
    notices: &mut Notices,
    id: &str,
    sent_at: i64,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<()> {
    let waits = route::episode(tx, id)?
        .filter(|episode| episode.open && episode.acted_active.is_none() && episode.level == 1);
    let Some(episode) = waits else {
        tx.execute(
            "UPDATE alarm_repeat_limit SET held_episode = NULL WHERE held_episode = ?1",
            [id],
        )?;
        return Ok(());
    };
    let escalating = episode.next_escalate.is_some_and(|due| active >= due);
    if active - sent_at >= WINDOW_MS || escalating {
        route::send_held(tx, notices, &episode, settings, active)?;
    }
    Ok(())
}

/// What ensures a held alarm reaches someone when no further alarm comes:
/// before `route::run` looks at escalation, each held message that is due goes
/// out. One message's fault must not stop the pass, since escalation follows.
pub(super) fn release_due(
    tx: &Connection,
    notices: &mut Notices,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<()> {
    let held = tx
        .prepare(
            "SELECT held_episode, sent_active_ms FROM alarm_repeat_limit
              WHERE held_episode IS NOT NULL",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, sent_at) in held {
        tx.execute_batch("SAVEPOINT alarm_release")?;
        match release(tx, notices, &id, sent_at, settings, active) {
            Ok(()) => tx.execute_batch("RELEASE alarm_release")?,
            Err(error) => {
                tx.execute_batch("ROLLBACK TO alarm_release; RELEASE alarm_release")?;
                eprintln!("Could not send the held alarm {id}: {error}");
            }
        }
    }
    Ok(())
}
