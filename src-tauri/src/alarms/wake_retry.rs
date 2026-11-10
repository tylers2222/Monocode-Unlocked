//! ADE-276: what runs between a stall coming due and its alarm. A missed wake
//! is mechanical, so the app wakes the agent itself once before it tells
//! anyone, and no alarm opens for an agent the hub already shows moving.
//!
//! The retry names the agent's chat, as the turn end and the reminders do:
//! `mono-message-notices` makes the window that owns the chat reconcile it.
//! That queues only what no queue item or slot holds, and its pump opens the
//! turn, so a retry cannot deliver an item twice or run a finished one. It
//! writes no message or prompt, and no model call goes to the provisioner.
//!
//! Neither step can hide a stall. Neither writes an episode, so the next pass
//! judges the agent again; a retry that fails, or that starts no turn in its
//! wait, ends in the alarm the detector would have opened, with the same
//! stalled-since. A drop keeps no state and needs a new hub record, and the
//! pass that takes a record that moved the agent ends that stall.
//!
//! Both read a turn as progress, and a turn end restarts wake_missed, so an
//! agent that keeps taking turns while its item stays stuck would never alarm.
//! The ceiling bounds that: an idle-type stall that began `CEILING_MS` ago
//! opens with no retry or re-check, from its original start.

use rusqlite::{params, Connection, OptionalExtension};

use super::activity;
use super::live::Live;
pub(crate) use super::state::Cause;
use super::state::{self, Episode, Facts};
use super::store::MINUTE_MS;
use crate::ask_gate::Actor;
use crate::mono_messages::{Notices, SessionId};

#[cfg(test)]
pub(crate) use super::store::pass_before_record;

/// How long an idle-type stall can wait for its item to move while the agent
/// keeps taking turns: past this, no retry or re-check holds its alarm back.
pub(super) const CEILING_MS: i64 = 30 * MINUTE_MS;

/// The original start of an idle-type stall that began a ceiling ago or more.
/// A turn end restarts wake_missed, but while the same wake waits (first
/// `seen` then), the stall began where its run of retries began.
pub(super) fn past_ceiling(
    tx: &Connection,
    actor: &Actor,
    (cause, stalled_since): (Cause, i64),
    (seen, active): (i64, i64),
) -> Option<i64> {
    let since = match cause {
        Cause::WakeMissed => match run_start(tx, actor, (stalled_since, seen)) {
            Ok(start) => start,
            Err(error) => {
                eprintln!("Could not read the retries of {actor}: {error}");
                stalled_since
            }
        },
        Cause::IdleOpenWork => stalled_since,
        Cause::Dialog | Cause::Hung | Cause::HoldMissing => return None,
    };
    (active - since >= CEILING_MS).then_some(since)
}

/// Where `actor`'s run of wake_missed stalls began. A run goes on while the
/// wake first `seen` at or before its start still waits; a wake that cleared
/// and came back is seen after the run began, and starts a new one.
fn run_start(
    tx: &Connection,
    actor: &Actor,
    (stalled_since, seen): (i64, i64),
) -> rusqlite::Result<i64> {
    let first: Option<i64> = tx
        .query_row(
            "SELECT first_stalled_since_active_ms FROM alarm_wake_retries WHERE actor = ?1",
            [actor.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match first {
        Some(first) if seen <= first => first.min(stalled_since),
        _ => stalled_since,
    })
}

/// Whether the turn that closes episode `id` as progress left its item where
/// it was. An alarm the ceiling opened counts from its run's start, so every
/// turn since reads as progress; its item moved only when the wake that waited
/// from then was taken. A failure here says no: the episode closes as before.
pub(super) fn still_stuck(
    tx: &Connection,
    id: &str,
    episode: &Episode,
    wake_pending_since: Option<i64>,
) -> bool {
    if !matches!(episode.cause, Ok(Cause::WakeMissed))
        || wake_pending_since.is_none_or(|seen| seen > episode.stalled_since)
    {
        return false;
    }
    let opened = tx.query_row(
        "SELECT opened_active_ms FROM alarm_episodes WHERE id = ?1",
        [id],
        |row| row.get::<_, i64>(0),
    );
    match opened {
        Ok(opened) => opened - episode.stalled_since >= CEILING_MS,
        Err(error) => {
            eprintln!("Could not read alarm {id}'s open: {error}");
            false
        }
    }
}

/// Whether the agent of an idle-type alarm took a turn after its stall
/// began: then it is alive, and only its item has not moved.
pub(super) fn turned_since(
    conn: &Connection,
    session: &SessionId,
    stalled_since: i64,
) -> rusqlite::Result<bool> {
    let turned: Option<Option<bool>> = conn
        .query_row(
            "SELECT turn_ended_active_ms > ?2 OR turn_started_active_ms > ?2
               FROM agent_activity WHERE session_id = ?1",
            params![session.as_str(), stalled_since],
            |row| row.get(0),
        )
        .optional()?;
    Ok(turned.flatten().unwrap_or(false))
}

/// One row per agent: its last retry, the stall it was for, and where that
/// stall's run began (`run_start`). A stall is its stalled-since, which moves
/// with a new turn end or a new wake, so a new stall replaces the row and gets
/// its own retry.
pub(crate) fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alarm_wake_retries (
           actor                   TEXT PRIMARY KEY,
           stalled_since_active_ms INTEGER NOT NULL,
           retried_active_ms       INTEGER NOT NULL,
           first_stalled_since_active_ms INTEGER NOT NULL
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
    stall: (i64, i64),
    (active, wait_ms): (i64, i64),
) -> bool {
    match retry(tx, notices, (actor, session), stall, (active, wait_ms)) {
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
    (stalled_since, seen): (i64, i64),
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
    let first = run_start(tx, actor, (stalled_since, seen))?;
    tx.execute(
        "INSERT INTO alarm_wake_retries
           (actor, stalled_since_active_ms, retried_active_ms, first_stalled_since_active_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (actor) DO UPDATE SET stalled_since_active_ms = ?2,
           retried_active_ms = ?3, first_stalled_since_active_ms = ?4",
        params![actor.to_string(), stalled_since, active, first],
    )?;
    notices.name(session.clone());
    Ok(true)
}

/// Whether the agent moved since `stall` began, by what the taps recorded
/// after this pass took the hub: the pass read every other row in the
/// transaction it writes in, so only the hub can be newer. Asks the detector's
/// own question (`state::progressed`), so a dialog goes only when it was
/// answered, and a hung turn only when it printed or ran a tool. A failure
/// here says no: the alarm opens. Only the known stall causes can drop, so a
/// cause added later always sends. A missing sleep hold is no stall: no turn,
/// output or dialog says it was fixed, and it is told once, so a drop would
/// lose it for good.
pub(crate) fn moved_on(
    tx: &Connection,
    live: &Live,
    session: &SessionId,
    stall: (Cause, i64),
    now: (i64, i64),
) -> bool {
    if !matches!(
        stall.0,
        Cause::IdleOpenWork | Cause::Dialog | Cause::Hung | Cause::WakeMissed
    ) {
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
