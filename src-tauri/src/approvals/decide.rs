use std::collections::HashSet;

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use tauri::{AppHandle, Runtime, State};

use super::{announce, get, to_json, Approval, Decision, Status, Verdict, NOT_OPEN};
use crate::milestones::require;
use crate::mono::content_hash;
use crate::mono_messages::{
    self, now_ms, Body, Delivery, Intent, MonoMessage, Notices, Part, Party, PayloadHash,
    RequestId, Routed, SessionId, Tool, Wanting,
};
use crate::session_store::SessionStore;

const NO_CHAT: &str = "filer_has_no_chat";

const TOO_LONG: &str = "decision_too_long";

macro_rules! sender {
    () => {
        "approvals-board"
    };
}

const SENDER: &str = sender!();

/// `removed` of a card: null unless its decision row is dropped, true when the user removed it.
pub(super) const UNDELIVERED: &str = concat!(
    "(SELECT notified_at IS NOT NULL FROM mono_messages WHERE from_session_id = '",
    sender!(),
    "' AND request_id = approvals.id AND status = 'dropped') AS removed"
);

fn decision_text(approval: &Approval, decision: &Decision) -> String {
    let mut text = format!(
        "The user decided approval card APR-{} \"{}\".\ncard_id: {}\ndecision: {}\nchosen_options: {}",
        approval.number,
        approval.card.heading,
        approval.id,
        decision.verdict.as_str(),
        to_json(&decision.chosen),
    );
    if let Some(note) = &decision.note {
        text.push_str("\ncomment: ");
        text.push_str(note);
    }
    text
}

fn deliver(tx: &Connection, approval: &Approval, body: &str, now: i64) -> Result<Notices, String> {
    let mut notices = Notices::default();
    let Some(giga) = &approval.requested_by.id else {
        return Ok(notices);
    };
    let chat = mono_messages::mono_row(tx, giga)
        .map_err(|e| e.to_string())?
        .and_then(|row| row.chat)
        .ok_or(NO_CHAT)?;
    let message = MonoMessage {
        id: mono_messages::new_message_id(),
        request_id: RequestId::parse(&approval.id).ok_or("the card id is not a request id")?,
        part: Part::FIRST,
        payload_hash: PayloadHash::parse(&content_hash(body)).expect("sha256 hex"),
        tool: Tool::SendMessage,
        from_mono_id: None,
        from_session_id: SessionId::parse(SENDER).expect("a session id"),
        to: Some(Party::Mono {
            mono_id: giga.clone(),
        }),
        addressed: giga.as_str().into(),
        body: Body::new(body),
        hop: 0,
        created_at: now,
        status: mono_messages::Status::Delivered {
            wanting_reply: Wanting,
            intent: Intent::New,
            routed: Routed {
                handled_by_session: chat.clone(),
                delivery: Delivery::Queue,
            },
            slot: None,
        },
    };
    mono_messages::insert(tx, &message).map_err(|e| e.to_string())?;
    notices.name(chat);
    Ok(notices)
}

/// Moves each decision row that a chat reset, delete or archive, or a chat
/// gone at startup, dropped before its Giga took it to the Giga's current
/// chat. A row the user removed from the queue (`notified_at`) stays dropped,
/// and so does one while that chat ended or, after startup, is still the
/// chat it was dropped from. A dropped row's card is `undelivered`.
pub(crate) fn requeue(conn: &Connection, now: i64) -> rusqlite::Result<Notices> {
    let chats = conn
        .prepare(
            "UPDATE mono_messages SET status = 'delivered', handled_by_session = chat.id,
               delivery = 'queue', turn_id = NULL, slot_window = NULL, handle = NULL,
               closed_at = NULL, updated_at = ?2
             FROM (SELECT id AS mono, json_extract(data_json, '$.sessionId') AS id FROM monos) AS chat
             WHERE chat.mono = mono_messages.to_mono_id AND chat.id IS NOT NULL
               AND from_session_id = ?1 AND status = 'dropped' AND delivered_at IS NULL
               AND notified_at IS NULL
               AND chat.id IS NOT handled_by_session
               AND chat.id NOT IN (SELECT session_id FROM mono_session_ends)
             RETURNING handled_by_session",
        )?
        .query_map(params![SENDER, now], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut notices = Notices::default();
    notices.approvals = !chats.is_empty();
    chats
        .iter()
        .filter_map(|chat| SessionId::parse(chat))
        .for_each(|chat| notices.name(chat));
    Ok(notices)
}

/// The board shows each decision's delivery, so dropping one changes it.
pub(crate) fn dropped(
    tx: &Connection,
    ids: &[mono_messages::MessageId],
) -> rusqlite::Result<Notices> {
    let mut is_decision =
        tx.prepare("SELECT 1 FROM mono_messages WHERE id = ?1 AND from_session_id = ?2")?;
    let mut notices = Notices::default();
    for id in ids {
        notices.approvals |= is_decision.exists(params![id.as_str(), SENDER])?;
    }
    Ok(notices)
}

pub(crate) fn decide(
    conn: &Connection,
    id: &str,
    verdict: Verdict,
    note: Option<String>,
    chosen: &[String],
    now: i64,
) -> Result<(Approval, Notices), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let open = get(&tx, id)?;
    if open.status != Status::Open {
        return Err(NOT_OPEN.into());
    }
    let labels: HashSet<&str> = open
        .card
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    let distinct: HashSet<&str> = chosen.iter().map(String::as_str).collect();
    require(
        distinct.len() == chosen.len() && distinct.is_subset(&labels),
        "chosen options must be distinct labels of the card's options",
    )?;
    require(
        verdict != Verdict::Approved || labels.is_empty() || !chosen.is_empty(),
        "approving a card with options needs at least one chosen option",
    )?;
    let note = note.map(|note| note.trim().to_owned());
    require(
        note.as_deref() != Some(""),
        "decision note must not be blank",
    )?;
    let decision = Decision {
        verdict,
        decided_at: now,
        note,
        chosen: chosen.to_vec(),
        undelivered: None,
    };
    let body = decision_text(&open, &decision);
    let chars = body.chars().count();
    if chars > Body::MAX_CHARS {
        return Err(format!("{TOO_LONG}:{chars}:{}", Body::MAX_CHARS));
    }
    tx.execute(
        "UPDATE approvals SET status = ?2, decided_at = ?3, decision_note = ?4,
           chosen_options = ?5
         WHERE id = ?1",
        params![
            id,
            verdict.as_str(),
            now,
            decision.note,
            (!chosen.is_empty()).then(|| to_json(&chosen))
        ],
    )
    .map_err(|e| e.to_string())?;
    let notices = deliver(&tx, &open, &body, now)?;
    let decided = get(&tx, id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok((decided, notices))
}

#[tauri::command(async)]
pub fn approvals_decide<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, SessionStore>,
    id: String,
    verdict: Verdict,
    note: Option<String>,
    chosen: Vec<String>,
) -> Result<Approval, String> {
    let (decided, notices) = decide(&*store.lock_conn()?, &id, verdict, note, &chosen, now_ms())?;
    notices.emit(&app);
    announce(&app, Ok(decided))
}

#[cfg(test)]
mod tests;
