//! R14: question, answer and update on an open ask. Each travels on a
//! `send_message` row to the other party, as an ack does, and writes one
//! `mono_ask_events` row; the key stays. A question pauses the ask's clock
//! and its answer resumes it; `ask_gate::decide_talk` decides each call.

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::ask_gate::{self, Actor, AskState, Said, Talk, TalkRefused, MAX_QUESTIONS};
use crate::ask_scheduler;
use crate::asks::{self, Carries, OnAsk, Refused};
use crate::mono_gate::{self, Refusal};
use crate::mono_messages::{MessageId, MonoMessage};

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
            message: raw["message"].as_str().unwrap_or_default().into(),
        })
    }
}

const ASK_ID: &str = "ask_id must be the ask_id (m_…) of an ask: the one you were sent, or for an answer the question_of of the question.";

fn refusal(talk: Talk, reason: TalkRefused) -> Refused {
    Refused(talk.refused_code(), reason.hint())
}

/// `question`, `answer` or `update` from the caller on ask `ask_id`, or on
/// the one open ask sent to the caller when it names none. `asks::on_ask`
/// runs it as it runs `ack`.
impl OnAsk for TalkInput {
    type Decided = (i64, Clock);

    fn kind(&self) -> &'static str {
        self.talk.kind()
    }

    fn message(&self) -> &str {
        &self.message
    }

    fn seq(&self) -> Option<i64> {
        self.talk.answers()
    }

    /// The ask the call names, else the ask a repeat's first row is on (the
    /// one its event names, or for a refused row the one its hash names),
    /// else the one open ask sent to `me`. A repeat is settled before the
    /// guess, so a second ask that came since cannot turn it into a refusal.
    fn ask_id(
        &self,
        conn: &Connection,
        prior: Option<&MonoMessage>,
        me: Option<&Actor>,
    ) -> rusqlite::Result<Result<MessageId, Refused>> {
        if let Some(ask_id) = &self.ask_id {
            return Ok(Ok(ask_id.clone()));
        }
        if let Some(row) = prior {
            if let Some(
                Carries::Ack { ask_id }
                | Carries::Question { ask_id, .. }
                | Carries::Answer { ask_id, .. }
                | Carries::Update { ask_id },
            ) = asks::carries(conn, &row.id)?
            {
                return Ok(Ok(ask_id));
            }
            if let Some(settled) = asks::refused_on(conn, self, row, me)? {
                return Ok(settled);
            }
        }
        let Some(me) = me else {
            return Ok(Err(self.no_open_ask()));
        };
        let open: Vec<String> = conn
            .prepare("SELECT id FROM mono_asks WHERE state = 'open' AND child_actor = ?1 LIMIT 2")?
            .query_map([me.to_string()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(match open.as_slice() {
            [one] => Ok(MessageId::parse(one).ok_or(rusqlite::Error::InvalidQuery)?),
            [] => Err(self.no_open_ask()),
            _ => Err(Refused(
                Refusal::InvalidInput.as_str(),
                "You have more than one open ask: name the one you mean with ask_id.".into(),
            )),
        })
    }

    fn to<'a>(&self, ask: &'a AskState) -> &'a Actor {
        self.talk.parties(ask).1
    }

    fn no_open_ask(&self) -> Refused {
        refusal(self.talk, TalkRefused::NoOpenAsk)
    }

    fn not_a_party(&self) -> Refused {
        refusal(self.talk, self.talk.not_a_party())
    }

    fn decide(
        &self,
        conn: &Connection,
        ask_id: &MessageId,
        ask: &AskState,
        me: &Actor,
    ) -> rusqlite::Result<Result<(i64, Clock), Refused>> {
        let clock = load_clock(conn, ask_id)?;
        Ok(ask_gate::decide_talk(ask, clock.said, self.talk, me)
            .map(|seq| (seq, clock))
            .map_err(|reason| refusal(self.talk, reason)))
    }

    /// A question pauses the clock: no next due, so the scheduler skips the
    /// ask. Its answer resumes it where it paused, and the ladder goes on
    /// from the moved deadline. An update leaves the clock running.
    fn write(
        self,
        tx: &Connection,
        ask_id: &MessageId,
        _: &AskState,
        _: &Actor,
        (seq, clock): (i64, Clock),
        active: i64,
    ) -> rusqlite::Result<Result<i64, Refused>> {
        match (self.talk, clock.paused_at) {
            (Talk::Question, _) => {
                // A reminder already queued would still go out while paused:
                // withdraw it and step the ladder back by the rows withdrawn,
                // so the answer re-runs that rung from the moved deadline. A
                // claimed row is mid-send and goes out, as one sent just
                // before the question would.
                let withdrawn = tx.execute(
                    "DELETE FROM mono_outbox
                      WHERE ask_id = ?1 AND sent_at IS NULL AND claimed_by IS NULL",
                    [ask_id.as_str()],
                )?;
                tx.execute(
                    "UPDATE mono_asks SET paused_at_active_ms = ?2, next_due_active_ms = NULL,
                       stage = stage - ?3 WHERE id = ?1",
                    params![
                        ask_id.as_str(),
                        active,
                        i64::try_from(withdrawn).unwrap_or(i64::MAX)
                    ],
                )?
            }
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
        Ok(Ok(seq))
    }
}

/// An ask's clock and what it already carries.
pub(crate) struct Clock {
    said: Said,
    due: Option<i64>,
    paused_at: Option<i64>,
    stage: i64,
}

fn load_clock(conn: &Connection, id: &MessageId) -> rusqlite::Result<Clock> {
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
}
