//! R14: question, answer and update on an open ask. Each travels on a
//! `send_message` row to the other party, as an ack does, and writes one
//! `mono_ask_events` row; the key stays. A question pauses the ask's clock
//! and its answer resumes it; `ask_gate::decide_talk` decides each call.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};

use crate::ask_gate::{self, Actor, Party, Said, Talk, TalkRefused, MAX_QUESTIONS};
use crate::ask_scheduler;
use crate::asks;
use crate::mono_gate::{self, Caller, Refusal};
use crate::mono_messages::{self, Live, MessageId, RequestId, Submitted};

#[derive(Debug)]
pub struct TalkInput {
    talk: Talk,
    ask_id: Option<MessageId>,
    message: String,
}

impl TalkInput {
    /// `tool` is `question`, `answer` or `update`. Only an answer must name
    /// its ask and the `seq` of the question it answers.
    pub fn parse(tool: &str, raw: &Value) -> Result<Self, String> {
        let answer = tool == "answer";
        let fields: &[&str] = if answer {
            &["ask_id", "seq", "message"]
        } else {
            &["ask_id", "message"]
        };
        mono_gate::known_fields(tool, raw, fields)?;
        let ask_id = match raw.get("ask_id") {
            Some(id) => Some(id.as_str().and_then(MessageId::parse).ok_or(ASK_ID)?),
            None if answer => return Err(ASK_ID.into()),
            None => None,
        };
        let talk = match tool {
            "answer" => Talk::Answer {
                seq: raw
                    .get("seq")
                    .and_then(Value::as_i64)
                    .filter(|seq| (1..=MAX_QUESTIONS).contains(seq))
                    .ok_or("seq must be the seq (1 to 3) of the question you answer.")?,
            },
            "update" => Talk::Update,
            _ => Talk::Question,
        };
        Ok(TalkInput {
            talk,
            ask_id,
            message: asks::text(raw, "message"),
        })
    }
}

const ASK_ID: &str = "ask_id must be the ask_id (m_…) of an ask: the one you were sent, or for an answer the question_of of the question.";

/// `question`, `answer` or `update` from `caller` on ask `ask_id`, or on the
/// one open ask sent to the caller when it names none. Decided before the
/// carrier row, as `ack` is, in one `BEGIN IMMEDIATE` with the event and the
/// clock, so two calls on one ask cannot both pass a limit. A repeat (same
/// request id) answers the first outcome.
pub fn talk(
    conn: &Connection,
    caller: &Caller,
    request_id: &RequestId,
    input: Result<TalkInput, String>,
    live: &Live,
    now: i64,
) -> rusqlite::Result<Submitted> {
    let input = match input {
        Ok(input) => input,
        Err(hint) => return Ok(invalid(hint)),
    };
    let talk = input.talk;
    let refused = |reason: TalkRefused| Ok(refusal(talk, reason));
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let me = asks::caller_party(&tx, caller)?.map(Party::into_actor);
    let ask_id = match named_ask(&tx, talk, input.ask_id, me.as_ref())? {
        Ok(ask_id) => ask_id,
        Err(refused) => return Ok(refused),
    };
    let (Some(ask), Some(clock)) = (asks::load(&tx, &ask_id)?, load_clock(&tx, &ask_id)?) else {
        return refused(TalkRefused::NoOpenAsk);
    };
    let prior = mono_messages::get_by_request(&tx, &caller.session_id, request_id)?;
    if asks::another_calls(prior.first(), |id| {
        asks::said_with(&tx, &ask_id, talk.kind(), id)
    })? {
        return Ok(asks::reused());
    }
    let (_, to) = talk.parties(&ask);
    let carrier = match asks::carrier(to.send_to(), &input.message) {
        Ok(carrier) => carrier,
        Err(hint) => return Ok(invalid(hint)),
    };
    // A party with no saved row, or a repeat: the send gate or the first
    // outcome answers it, so nothing is decided.
    let mut seq = None;
    let mut listed = None;
    if let (true, Some(me)) = (prior.is_empty(), &me) {
        seq = Some(match ask_gate::decide_talk(&ask, clock.said, talk, me) {
            Ok(seq) => seq,
            Err(reason) => return refused(reason),
        });
        if let Actor::Session(session) = to {
            listed = Some(mono_messages::with_own_session(&tx, live, session)?);
        }
    }
    let live = Live {
        sessions: listed.as_deref().unwrap_or(live.sessions),
        ..*live
    };
    let hash = mono_gate::action_hash(
        talk.kind(),
        &json!({"carrier": carrier.payload_hash().as_str(), "ask_id": ask_id.as_str(), "seq": talk.answers()}),
    );
    let mut submitted =
        mono_messages::submit_in(&tx, caller, request_id, carrier, hash, &live, now)?;
    if let Submitted::Accepted {
        messages,
        duplicate: false,
        ..
    } = &mut submitted
    {
        // The gate let through a caller with no saved row (a session the
        // window lists): no party to any ask. Dropping `tx` undoes the row.
        let Some(seq) = seq else {
            return refused(talk.not_a_party());
        };
        let id = &messages[0].message.id;
        record(&tx, &ask_id, talk, seq, &clock, id, now)?;
        messages[0].carries = asks::carries(&tx, id)?;
    }
    tx.commit()?;
    Ok(submitted)
}

fn invalid(hint: impl Into<String>) -> Submitted {
    asks::failed(Refusal::InvalidInput.as_str(), hint)
}

fn refusal(talk: Talk, reason: TalkRefused) -> Submitted {
    asks::failed(talk.refused_code(), reason.hint())
}

/// The ask a call names, else the one open ask sent to `me`.
fn named_ask(
    conn: &Connection,
    talk: Talk,
    ask_id: Option<MessageId>,
    me: Option<&Actor>,
) -> rusqlite::Result<Result<MessageId, Submitted>> {
    let no_open_ask = || refusal(talk, TalkRefused::NoOpenAsk);
    let (None, Some(me)) = (&ask_id, me) else {
        return Ok(ask_id.ok_or_else(no_open_ask));
    };
    let open: Vec<String> = conn
        .prepare("SELECT id FROM mono_asks WHERE state = 'open' AND child_actor = ?1 LIMIT 2")?
        .query_map([me.to_string()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match open.as_slice() {
        [one] => Ok(MessageId::parse(one).ok_or(rusqlite::Error::InvalidQuery)?),
        [] => Err(no_open_ask()),
        _ => Err(invalid(
            "You have more than one open ask: name the one you mean with ask_id.",
        )),
    })
}

/// An ask's clock and what it already carries.
struct Clock {
    said: Said,
    due: Option<i64>,
    paused_at: Option<i64>,
    stage: i64,
}

/// The event, at the stored active time, and the clock it moves. A
/// question pauses the clock: no next due, so the scheduler skips the ask.
/// Its answer resumes it where it paused, and the ladder goes on from the
/// moved deadline. An update leaves the clock running.
fn record(
    tx: &Connection,
    ask_id: &MessageId,
    talk: Talk,
    seq: i64,
    clock: &Clock,
    message_id: &MessageId,
    now: i64,
) -> rusqlite::Result<()> {
    let active = asks::active_ms(tx)?;
    match (talk, clock.paused_at) {
        (Talk::Question, _) => tx.execute(
            "UPDATE mono_asks SET paused_at_active_ms = ?2, next_due_active_ms = NULL
              WHERE id = ?1",
            params![ask_id.as_str(), active],
        )?,
        (Talk::Answer { .. }, Some(paused_at)) => {
            let due = ask_gate::resumed_due(clock.due, paused_at, active);
            tx.execute(
                "UPDATE mono_asks SET due_active_ms = ?2, paused_at_active_ms = NULL,
                   next_due_active_ms = ?3 WHERE id = ?1",
                params![
                    ask_id.as_str(),
                    due,
                    due.and_then(|due| ask_scheduler::next_due(due, clock.stage))
                ],
            )?
        }
        (Talk::Answer { .. } | Talk::Update, _) => 0,
    };
    tx.execute(
        "INSERT INTO mono_ask_events (ask_id, kind, seq, message_id, at_active_ms, at_wall_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ask_id.as_str(),
            talk.kind(),
            seq,
            message_id.as_str(),
            active,
            now
        ],
    )?;
    Ok(())
}

fn load_clock(conn: &Connection, id: &MessageId) -> rusqlite::Result<Option<Clock>> {
    conn.query_row(
        "SELECT due_active_ms, paused_at_active_ms, stage,
           (SELECT count(*) FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'question'),
           EXISTS (SELECT 1 FROM mono_ask_events WHERE ask_id = ?1 AND kind = 'update')
         FROM mono_asks WHERE id = ?1",
        [id.as_str()],
        |row| {
            let paused_at: Option<i64> = row.get(1)?;
            Ok(Clock {
                said: Said {
                    questions: row.get(3)?,
                    paused: paused_at.is_some(),
                    updated: row.get(4)?,
                },
                due: row.get(0)?,
                paused_at,
                stage: row.get(2)?,
            })
        },
    )
    .optional()
}
