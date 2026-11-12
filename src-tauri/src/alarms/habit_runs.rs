//! ADE-306: a Mono that works in habit runs is not idle. Each run is its own
//! session with its own row, and its chat takes no turn, so the Mono's facts
//! fold in its runs' turns (`state::Runs`).

use std::collections::HashMap;

use super::activity::Activity;
use super::state::{Runs, Turn};

/// The chat a habit run works for. The front end names each run
/// `app-<chat session id>-habit-<uuid>` (`habitRunSessionId` in
/// `src/features/monos/model/monoHabits.ts`), and no row stores the link.
/// The uuid has no `-habit-`, so the last one ends the chat's id.
fn chat_of(session: &str) -> Option<&str> {
    let (chat, run) = session.strip_prefix("app-")?.rsplit_once("-habit-")?;
    (!chat.is_empty() && !run.is_empty()).then_some(chat)
}

/// Each chat's habit runs, folded from the watched rows.
pub(super) fn by_chat(rows: &[Activity]) -> HashMap<String, Runs> {
    let mut runs: HashMap<String, Runs> = HashMap::new();
    for row in rows {
        let Some(chat) = chat_of(row.session.as_str()) else {
            continue;
        };
        let folded = runs.entry(chat.to_string()).or_default();
        let at = match row.turn {
            Turn::Running { started_at, .. } => {
                folded.running = true;
                started_at
            }
            Turn::Idle { ended_at, .. } => ended_at,
        };
        folded.last_at = Some(folded.last_at.map_or(at, |last| last.max(at)));
    }
    runs
}
