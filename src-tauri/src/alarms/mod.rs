//! The stall alarm (alarms design, rows S1 and S2). Taps on the stdio Rust
//! pipes and on the turn hooks record into the store's hub (`live`); a pass
//! on the active clock writes those records to `agent_activity`, derives
//! each agent's state from the rows (`state`), opens one episode per stall
//! and routes it up the chain of provisioners (`route`). Recipients act on
//! it with the `alarms.*` app actions (`act`).

pub(crate) mod act;
mod activity;
pub(crate) mod deliver;
mod live;
mod provisioner;
mod rights;
mod route;
mod state;
mod store;
pub(crate) mod taps;
mod text;
mod tree;

use rusqlite::Connection;
use tauri::{Manager, Runtime};

use crate::session_store::SessionStore;

pub(crate) use activity::end_all_running;
pub(crate) use live::{ChildTap, Edge, Live};
pub(crate) use store::{run, Sources};

/// The detector runs once per this much active time (design section 3).
pub(crate) const PASS_MS: i64 = 5_000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    activity::ensure_table(conn)?;
    store::ensure_tables(conn)?;
    store::ensure_indexes(conn)?;
    route::ensure_tables(conn)
}

pub(crate) use route::TylerAlarm;

/// The active and wall time an act is stamped with: this process's clock,
/// else the one the clock's process last stored.
pub(crate) fn now(live: &Live, conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    let active = match live.now() {
        Some(active) => active,
        None => conn.query_row(
            "SELECT COALESCE((SELECT active_ms FROM mono_clock WHERE id = 1), 0)",
            [],
            |row| row.get(0),
        )?,
    };
    Ok((active, crate::session_store::now_millis()))
}

/// Shows Tyler, once each, the levels that reached him and were not shown.
/// The store lock is let go while `show` runs. A level is marked shown
/// even when its banner failed, so a denied OS permission does not retry
/// every pass.
pub(crate) fn notify_tyler(
    store: &SessionStore,
    (active, wall): (i64, i64),
    mut show: impl FnMut(&TylerAlarm) -> Result<(), String>,
) -> Result<(), String> {
    let pending =
        route::tyler_pending(&*store.lock_conn()?, active).map_err(|error| error.to_string())?;
    for alarm in &pending {
        if let Err(error) = show(alarm) {
            eprintln!("Could not show the alarm {}: {error}", alarm.episode);
        }
        route::tyler_told(&*store.lock_conn()?, &alarm.episode, alarm.level, wall)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Starts the thread that puts Tyler's alarms up as OS notifications.
pub(crate) fn init(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(
            u64::try_from(PASS_MS).unwrap_or(5_000),
        ));
        let store = app.state::<SessionStore>();
        let Some(active) = store.alarms.now() else {
            continue;
        };
        let now = (active, crate::session_store::now_millis());
        let shown = notify_tyler(&store, now, |alarm| {
            tauri::async_runtime::block_on(crate::notifications::show_alarm(
                &app,
                &alarm.episode,
                &alarm.title,
                &alarm.body,
            ))
        });
        if let Err(error) = shown {
            eprintln!("Could not show Tyler's alarms: {error}");
        }
    });
}

/// The session a click on alarm `episode`'s banner opens: the stalled
/// agent's own (Tyler acts on it directly).
pub(crate) fn alarm_session<R: Runtime>(
    app: &tauri::AppHandle<R>,
    episode: &str,
) -> Option<String> {
    let store = app.state::<SessionStore>();
    let conn = store.lock_conn().ok()?;
    let episode = route::episode(&conn, episode).ok()??;
    route::session_of(&conn, &episode.actor)
        .ok()?
        .map(|session| session.as_str().to_string())
}

/// What the tests read of the rows.
#[cfg(test)]
pub(crate) mod test_taps {
    pub(crate) use super::store::outbox_key;

    /// `session`'s row as `(turn_state, turn_id, last_progress, tool_running)`.
    pub(crate) fn row(
        conn: &rusqlite::Connection,
        session: &str,
    ) -> Option<(String, Option<String>, Option<i64>, bool)> {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT turn_state, turn_id, last_progress_active_ms, tool_running
               FROM agent_activity WHERE session_id = ?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .unwrap()
    }
}
