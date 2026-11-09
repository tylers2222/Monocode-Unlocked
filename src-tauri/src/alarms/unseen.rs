//! Finds the Mono chats the detector cannot see. `Pass` judges only sessions
//! with an `agent_activity` row, and a row comes from a turn or a tapped child
//! on a build with the detector. So a chat that ran neither since the detector
//! shipped is never judged, whatever waits for it.
//!
//! Each pass, before the rows load, a live Mono chat (a Giga's too) with no row
//! and a wake waiting gets a row: idle, no turn yet, ended now. From there it
//! is any watched chat, so `Pass::wakes` decides, `wake_missed` counts from this
//! pass, and the retry, the limit and the escalation apply unchanged.
//!
//! An open ask alone seeds nothing. Stale asks of old chats would alarm at
//! once after a ship, and a new ask reaches a chat as a message, which seeds.

use std::collections::HashSet;

use rusqlite::{params, Connection};

use crate::ask_gate::Actor;
use crate::ask_scheduler;
use crate::mono_messages::{self, MonoId, SessionId};

/// Gives each unseen chat with a wake its row. A fault here must not stop the
/// pass: every watched agent's alarm depends on it.
pub(super) fn seed(tx: &Connection, active: i64, wall: i64) {
    if let Err(error) = seed_waiting(tx, active, wall) {
        eprintln!("Could not read the Mono chats with no activity row: {error}");
    }
}

fn seed_waiting(tx: &Connection, active: i64, wall: i64) -> rusqlite::Result<()> {
    let chats = mono_messages::mono_chats(tx)?;
    let unseen = unseen(tx, &chats)?;
    if unseen.is_empty() {
        return Ok(());
    }
    let listed = serde_json::to_string(&unseen).unwrap_or_default();
    let holding = holding_work(tx, &listed)?;
    let waiting = ask_scheduler::waiting_recipients(tx)?;
    for (mono, chat) in &chats {
        if unseen.contains(chat.as_str())
            && (holding.contains(chat.as_str())
                || waiting.contains(&Actor::Mono(mono.clone()).to_string())
                || waiting.contains(&Actor::Session(chat.clone()).to_string()))
        {
            write_row(tx, chat, active, wall)?;
        }
    }
    Ok(())
}

/// The chats of `chats` that a pass does not judge and could: live, not paused
/// by the user (a paused queue waits on purpose, as in `Pass::wakes`), no row.
/// A chat `sessions` no longer holds is gone.
fn unseen(tx: &Connection, chats: &[(MonoId, SessionId)]) -> rusqlite::Result<HashSet<String>> {
    let ids: Vec<&str> = chats.iter().map(|(_, chat)| chat.as_str()).collect();
    tx.prepare(
        "SELECT s.id FROM json_each(?1) c JOIN sessions s ON s.id = c.value
          WHERE s.archived = 0 AND s.queue_status IS NOT 'paused'
            AND NOT EXISTS (SELECT 1 FROM agent_activity a WHERE a.session_id = s.id)",
    )?
    .query_map([serde_json::to_string(&ids).unwrap_or_default()], |row| {
        row.get(0)
    })?
    .collect()
}

/// Of the chats in the JSON array `listed`, those a queue item, a message no
/// provider took or a completion result waits for. These read as the same
/// facts do in `Rows::load`, which reads only chats that have a row. A bare
/// completion row seeds nothing: it may be a turn that still runs.
fn holding_work(tx: &Connection, listed: &str) -> rusqlite::Result<HashSet<String>> {
    tx.prepare(&format!(
        "SELECT s.id FROM json_each(?1) c JOIN sessions s ON s.id = c.value,
                json_each(s.queued_messages_json) q
          WHERE s.queued_messages_json <> '[]' AND json_valid(s.queued_messages_json)
            AND json_type(q.value, '$.id') = 'text' AND NOT {alarm_item}
         UNION
         SELECT m.handled_by_session FROM mono_messages m
          WHERE m.handled_by_session IN (SELECT value FROM json_each(?1))
            AND m.delivered_at IS NULL AND m.status IN ('open', 'delivered')
            AND m.tool IN ('send_message', 'reply')
            AND NOT EXISTS (SELECT 1 FROM mono_ask_events e WHERE e.message_id = m.id
                             AND e.kind = 'question' AND e.blocked = 1)
         UNION
         SELECT json_extract(saved_json, '$.origin.monoId') FROM mono_completions
          WHERE json_valid(saved_json) AND json_type(saved_json, '$.origin.monoId') = 'text'
            AND json_type(saved_json, '$.result') = 'object'",
        alarm_item = ask_scheduler::alarm_key_sql("json_extract(q.value, '$.id')"),
    ))?
    .query_map([listed], |row| row.get(0))?
    .collect()
}

fn write_row(tx: &Connection, chat: &SessionId, active: i64, wall: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO agent_activity (session_id, turn_ended_active_ms, updated_wall_ms)
         VALUES (?1, ?2, ?3)",
        params![chat.as_str(), active, wall],
    )?;
    Ok(())
}
