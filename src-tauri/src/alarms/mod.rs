//! The stall alarm's detector (alarms design, row S1). Taps on the stdio
//! Rust pipes and on the turn hooks record into the store's hub (`live`);
//! a pass on the active clock writes those records to `agent_activity`,
//! derives each agent's state from the rows (`state`) and opens one
//! episode with one level-1 outbox row per stall.

mod activity;
mod live;
mod provisioner;
mod state;
mod store;
pub(crate) mod taps;

use rusqlite::Connection;

pub(crate) use activity::end_all_running;
pub(crate) use live::{ChildTap, Edge, Live};
pub(crate) use store::{run, Sources};

/// The detector runs once per this much active time (design section 3).
pub(crate) const PASS_MS: i64 = 5_000;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    activity::ensure_table(conn)?;
    store::ensure_tables(conn)?;
    store::ensure_indexes(conn)
}

/// The outbox text of level `level` of episode `episode`. A placeholder:
/// row S2 writes the design section 6 text and the act commands.
pub(crate) fn alarm_text(episode: &str, level: i64, actor: &str, cause: &str) -> String {
    format!(
        "<mono_alarm alarm=\"{episode}\" level=\"{level}\" cause=\"{cause}\">\n{actor} is stalled ({cause}). Read its session and help it go on, or tell it to ask with messages.question blocked:true.\n</mono_alarm>"
    )
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
