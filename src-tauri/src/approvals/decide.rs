use std::collections::HashSet;

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use tauri::{AppHandle, Runtime, State};

use super::{announce, check_text, get, to_json, Approval, Decision, Verdict, NOT_OPEN, TEXT_MAX};
use crate::milestones::require;
use crate::mono::content_hash;
use crate::mono_messages::{
    self, now_ms, Body, Delivery, Intent, MonoMessage, Notices, Part, Party, PayloadHash,
    RequestId, Routed, SessionId, Tool, Wanting,
};
use crate::session_store::SessionStore;

pub(crate) const NO_CHAT: &str = "filer_has_no_chat";

pub(crate) const SENDER: &str = "approvals-board";

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

fn deliver(
    tx: &Connection,
    approval: &Approval,
    decision: &Decision,
    now: i64,
) -> Result<Notices, String> {
    let mut notices = Notices::default();
    let Some(giga) = &approval.requested_by.id else {
        return Ok(notices);
    };
    let chat = mono_messages::mono_row(tx, giga)
        .map_err(|e| e.to_string())?
        .and_then(|row| row.chat)
        .ok_or(NO_CHAT)?;
    let body = decision_text(approval, decision);
    require(
        body.chars().count() <= Body::MAX_CHARS,
        "the decision is too long to deliver; shorten the comment",
    )?;
    let message = MonoMessage {
        id: mono_messages::new_message_id(),
        request_id: RequestId::parse(&approval.id).ok_or("the card id is not a request id")?,
        part: Part::FIRST,
        payload_hash: PayloadHash::parse(&content_hash(&body)).expect("sha256 hex"),
        tool: Tool::SendMessage,
        from_mono_id: None,
        from_session_id: SessionId::parse(SENDER).expect("a session id"),
        to: Some(Party::Mono {
            mono_id: giga.clone(),
        }),
        addressed: giga.as_str().into(),
        body: Body::new(&body),
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
    let decision = Decision {
        verdict,
        decided_at: now,
        note: note
            .map(|note| check_text("decision note", &note, TEXT_MAX))
            .transpose()?,
        chosen: chosen.to_vec(),
    };
    let updated = tx
        .execute(
            "UPDATE approvals SET status = ?2, decided_at = ?3, decision_note = ?4,
               chosen_options = ?5
             WHERE id = ?1 AND status = 'open'",
            params![
                id,
                verdict.as_str(),
                now,
                decision.note,
                (!chosen.is_empty()).then(|| to_json(&chosen))
            ],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        return Err(NOT_OPEN.into());
    }
    let notices = deliver(&tx, &open, &decision, now)?;
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
