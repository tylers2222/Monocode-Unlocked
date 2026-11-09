//! ADE-276: what runs between a stall coming due and its alarm's message. A
//! missed wake is mechanical, so the app wakes the agent itself once before it
//! tells anyone, and no message goes for an agent the hub already shows moving.
//!
//! The retry names the agent's chat, as the turn end and the reminders do:
//! `mono-message-notices` makes the window that owns the chat reconcile it.
//! That queues only what no queue item or slot holds, and its pump opens the
//! turn, so a retry cannot deliver an item twice or run a finished one. It
//! writes no message or prompt, and no model call goes to the provisioner.
//!
//! Neither step can hide a stall. A retry writes no episode, so the next pass
//! judges the agent again; one that fails, or that starts no turn in its wait,
//! ends in the alarm the detector would have opened, with the same
//! stalled-since. The re-check stops only level 1's message: the episode, its
//! send row and its escalation clock stand, so a stall that lasts escalates.

use rusqlite::{params, Connection, OptionalExtension};

use super::activity;
use super::live::Live;
use super::route::{self, EpisodeRow};
use super::state::{self, Cause, Episode, Facts};
use crate::ask_gate::Actor;
use crate::mono_messages::{Notices, SessionId};

#[cfg(test)]
pub(crate) use super::state::Cause as StallCause;
#[cfg(test)]
pub(crate) use super::store::pass_before_record;

/// One row per agent: its last retry, and the stall it was for. A stall is
/// its stalled-since, which moves with a new turn end or a new wake, so a new
/// stall replaces the row and gets its own retry.
pub(crate) fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alarm_wake_retries (
           actor                   TEXT PRIMARY KEY,
           stalled_since_active_ms INTEGER NOT NULL,
           retried_active_ms       INTEGER NOT NULL
         );",
    )
}

/// Whether `actor`'s due wake_missed stall waits for its retry. The first pass
/// that sees the stall retries it; each pass after waits until `wait_ms` of
/// active time passed since. A failure here never waits: the alarm opens.
pub(super) fn held(
    tx: &Connection,
    notices: &mut Notices,
    (actor, session): (&Actor, &SessionId),
    stalled_since: i64,
    (active, wait_ms): (i64, i64),
) -> bool {
    match retry(
        tx,
        notices,
        (actor, session),
        stalled_since,
        (active, wait_ms),
    ) {
        Ok(held) => held,
        Err(error) => {
            eprintln!("Could not retry the missed wake of {actor}: {error}");
            false
        }
    }
}

fn retry(
    tx: &Connection,
    notices: &mut Notices,
    (actor, session): (&Actor, &SessionId),
    stalled_since: i64,
    (active, wait_ms): (i64, i64),
) -> rusqlite::Result<bool> {
    let retried: Option<i64> = tx
        .query_row(
            "SELECT retried_active_ms FROM alarm_wake_retries
              WHERE actor = ?1 AND stalled_since_active_ms = ?2",
            params![actor.to_string(), stalled_since],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(retried) = retried {
        return Ok(active - retried < wait_ms);
    }
    tx.execute(
        "INSERT INTO alarm_wake_retries (actor, stalled_since_active_ms, retried_active_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (actor) DO UPDATE SET stalled_since_active_ms = ?2, retried_active_ms = ?3",
        params![actor.to_string(), stalled_since, active],
    )?;
    notices.name(session.clone());
    Ok(true)
}

/// Whether level 1's message of `episode` stops here. A failure here says no.
pub(super) fn drops_alarm(
    tx: &Connection,
    live: &Live,
    episode: &EpisodeRow,
    now: (i64, i64),
) -> bool {
    let session = route::session_of(tx, &episode.actor).unwrap_or_else(|error| {
        eprintln!(
            "Could not re-check {} before its alarm: {error}",
            episode.id
        );
        None
    });
    match (session, Cause::parse(&episode.cause)) {
        (Some(session), Ok(cause)) => moved_on(
            tx,
            live,
            &session,
            (cause, episode.stalled_since_active),
            now,
        ),
        _ => false,
    }
}

/// Whether the agent moved since `stall` began, by what the taps recorded
/// after this pass took the hub: the pass read every other row in the
/// transaction it writes in, so only the hub can be newer. Asks the detector's
/// own question (`state::progressed`), so a dialog goes only when it was
/// answered, and a hung turn only when it printed or ran a tool. A failure
/// here says no: the alarm opens. A missing sleep hold is no stall: no turn,
/// output or dialog says it was fixed, so a drop could lose it for good.
pub(crate) fn moved_on(
    tx: &Connection,
    live: &Live,
    session: &SessionId,
    stall: (Cause, i64),
    now: (i64, i64),
) -> bool {
    if stall.0 == Cause::HoldMissing {
        return false;
    }
    moved(tx, live, session, stall, now).unwrap_or_else(|error| {
        eprintln!("Could not re-check {session:?} before its alarm: {error}");
        false
    })
}

/// The hub's records go through the code that will write them next pass, and
/// back out: the pass that takes them writes them for good.
fn moved(
    tx: &Connection,
    live: &Live,
    session: &SessionId,
    stall: (Cause, i64),
    now: (i64, i64),
) -> rusqlite::Result<bool> {
    let writes = live.pending(session);
    if writes.is_empty() {
        return Ok(false);
    }
    tx.execute_batch("SAVEPOINT alarm_recheck")?;
    let written = writes
        .iter()
        .try_for_each(|write| activity::apply(tx, session, write, now))
        .and_then(|()| activity::all(tx));
    tx.execute_batch("ROLLBACK TO alarm_recheck; RELEASE alarm_recheck")?;
    let Some(row) = written?.into_iter().find(|row| row.session == *session) else {
        return Ok(false);
    };
    let (cause, stalled_since) = stall;
    let facts = Facts {
        gone: false,
        turn: row.turn,
        dialog: row.dialog,
        background: row.background,
        open_work: Vec::new(),
        children: Vec::new(),
        settled_at: None,
        wake_pending_since: row.wake_pending_since,
        episode: None,
    };
    Ok(state::progressed(
        &facts,
        &Episode {
            cause: Ok(cause),
            stalled_since,
        },
    ))
}
