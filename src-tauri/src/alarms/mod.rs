//! The stall alarm's detector (alarms design, row S1). Taps on the stdio
//! Rust pipes and on the turn hooks write `agent_activity`; a pass on the
//! active clock derives each agent's state from the rows (`state`) and
//! opens one episode with one level-1 outbox row per stall.

mod activity;
mod provisioner;
mod state;
mod store;
pub(crate) mod taps;

use std::sync::Mutex;

use rusqlite::Connection;
use tauri::{Manager, Runtime};

use crate::mono_messages::SessionId;
use crate::session_store::SessionStore;
use taps::{Harness, Pipe, Tap};

/// The detector runs once per this much active time (design section 3).
pub(crate) const PASS_MS: i64 = 5_000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    activity::ensure_table(conn)?;
    store::ensure_tables(conn)
}

pub(crate) use store::run;

/// The outbox text of level `level` of episode `episode`. A placeholder:
/// row S2 writes the design section 6 text and the act commands.
pub(crate) fn alarm_text(episode: &str, level: i64, actor: &str, cause: &str) -> String {
    format!(
        "<mono_alarm alarm=\"{episode}\" level=\"{level}\" cause=\"{cause}\">\n{actor} is stalled ({cause}). Read its session and help it go on, or tell it to ask with messages.question blocked:true.\n</mono_alarm>"
    )
}

/// The tap for a child of `provider`, when this build reads that harness.
pub(crate) fn tap_for(provider: Option<&str>) -> Option<Tap> {
    provider.and_then(Harness::parse).map(Tap::new)
}

/// A child of `session` by `provider` was spawned. Written for every
/// harness, so a session that moves to one no tap reads is not judged by
/// the last tap's row.
pub(crate) fn spawned<R: Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
    provider: Option<&str>,
) {
    let (Some(session), Some(now)) = (
        SessionId::parse(session),
        crate::active_clock::latest_active_ms(),
    ) else {
        return;
    };
    let harness = provider.and_then(Harness::parse);
    let written = app.state::<SessionStore>().lock_conn().and_then(|conn| {
        activity::spawned(
            &conn,
            &session,
            harness,
            now,
            crate::session_store::now_millis(),
        )
        .map_err(|error| error.to_string())
    });
    if let Err(error) = written {
        eprintln!(
            "Could not record the spawn of {}: {error}",
            session.as_str()
        );
    }
}

/// Line `line` of `session` went through `pipe`; writes the row when due.
/// The tap's lock is held across the write, so the stdout reader and
/// `harness_write` write their rows in the order they read the lines.
pub(crate) fn tap_line<R: Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
    tap: &Mutex<Tap>,
    pipe: Pipe,
    line: &str,
) {
    let (Some(session), Some(now)) = (
        SessionId::parse(session),
        crate::active_clock::latest_active_ms(),
    ) else {
        return;
    };
    let mut tap = tap.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(row) = tap.line(pipe, line, now) else {
        return;
    };
    let written = app.state::<SessionStore>().lock_conn().and_then(|conn| {
        activity::write_tap(&conn, &session, &row, crate::session_store::now_millis())
            .map_err(|error| error.to_string())
    });
    match written {
        Ok(()) => tap.wrote(row, now),
        Err(error) => eprintln!(
            "Could not record the activity of {}: {error}",
            session.as_str()
        ),
    }
}

/// Turn `turn_id` of `session` started or ended, on `conn`.
pub(crate) fn turn(conn: &Connection, session: &str, turn_id: &str, started: bool) {
    let (Some(session), Some(now)) = (
        SessionId::parse(session),
        crate::active_clock::latest_active_ms(),
    ) else {
        return;
    };
    let wall = crate::session_store::now_millis();
    let written = if started {
        activity::turn_started(conn, &session, turn_id, now, wall)
    } else {
        activity::turn_ended(conn, &session, turn_id, now, wall)
    };
    if let Err(error) = written {
        eprintln!("Could not record the turn of {}: {error}", session.as_str());
    }
}

/// What the scenario tests drive: the taps at an explicit active time.
#[cfg(test)]
pub(crate) mod test_taps {
    pub(crate) use super::activity::{turn_ended, turn_started, write_tap};
    pub(crate) use super::store::outbox_key;
}
