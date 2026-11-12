//! ADE-306: a Mono that works in habit runs is not idle. Each run is its own
//! session with its own row, and its chat takes no turn, so the Mono's facts
//! fold in its runs' turns (`state::Runs`).

use std::collections::HashMap;

use super::activity::Activity;
use super::state::{Runs, Turn};

/// The chat a habit run works for. The front end names each run
/// `app-<chat session id>-habit-<uuid>` (`habitRunSessionId` in
/// `src/features/monos/model/monoHabits.ts`), and no row stores the link.
/// The uuid has no `-habit-`, so the last one ends the chat's id. Only a uuid
/// counts: a session started with `--request-id habit-x` is named
/// `app-<starter>-habit-x` (`appLaunchId`), and is no habit run.
fn chat_of(session: &str) -> Option<&str> {
    let (chat, run) = session.strip_prefix("app-")?.rsplit_once("-habit-")?;
    (!chat.is_empty() && uuid::Uuid::parse_str(run).is_ok()).then_some(chat)
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

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "42b9ff83-0435-47c3-add0-14a41eeb929e";
    const RUN: &str = "70a6a67e-c699-4ae2-a4f0-92c330964737";

    // Defect (PR #647 r1 M1): any `app-<id>-habit-<text>` session counted as
    // a habit run of `<id>`, so a session started with a `habit-` request id
    // held its starter as busy.
    #[test]
    fn only_a_uuid_run_names_its_chat() {
        let run_started = format!("app-app-{CHAT}-habit-{RUN}-{RUN}");
        let rows = [
            (format!("app-{CHAT}-habit-{RUN}"), Some(CHAT)),
            (format!("app-{CHAT}-habit-check"), None),
            (format!("app-{CHAT}-habit-"), None),
            (format!("app--habit-{RUN}"), None),
            (format!("app-x-habit-y-habit-{RUN}"), Some("x-habit-y")),
            (run_started, None),
            (format!("app-{CHAT}-{RUN}"), None),
            (format!("{CHAT}-habit-{RUN}"), None),
            (CHAT.to_string(), None),
        ];
        for (session, chat) in &rows {
            assert_eq!(chat_of(session), *chat, "{session}");
        }
    }
}
