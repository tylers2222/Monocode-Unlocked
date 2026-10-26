//! R14, worlds S2 and S4: a question keeps the key and pauses the ask's
//! clock, its answer resumes it where it paused, one update keeps the key
//! with the clock running, and the limits hold.

use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller};
use super::asks::written;
use super::world::World;
use crate::ask_gate::{Talk, TalkRefused};
use crate::asks::Carries;
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, MonoId, SessionId, Submitted};
use crate::mono_roster::MonoRole;

const P: &str = "/p";
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const T5_LEAD: i64 = 5 * MINUTE;

type Said = (Option<Carries>, Option<String>, Option<String>);
type Clock = (String, Option<i64>, Option<i64>, Option<i64>);

/// What the row a call wrote carries, and the Mono or session it went to.
fn said(world: &World, submitted: Submitted) -> Said {
    let Submitted::Accepted {
        mut messages,
        duplicate: false,
        ..
    } = submitted
    else {
        panic!("not a new accept: {submitted:?}");
    };
    let row = messages.remove(0);
    let (mono, session) = world
        .conn()
        .query_row(
            "SELECT to_mono_id, to_session_id FROM mono_messages WHERE id = ?1",
            [row.message.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    (row.carries, mono, session)
}

fn refused(submitted: Submitted, talk: Talk, reason: TalkRefused) {
    match submitted {
        Submitted::Failed { error } => {
            assert_eq!(
                (error.code, error.hint),
                (talk.refused_code(), reason.hint())
            );
        }
        other => panic!("not refused: {other:?}"),
    }
}

/// `(state, due, paused_at, next_due)` of ask `id`.
fn clock(world: &World, id: &MessageId) -> Clock {
    world
        .conn()
        .query_row(
            "SELECT state, due_active_ms, paused_at_active_ms, next_due_active_ms
               FROM mono_asks WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

/// The active time event `kind` `seq` of ask `id` was written at.
fn at(world: &World, id: &MessageId, kind: &str, seq: i64) -> i64 {
    world
        .conn()
        .query_row(
            "SELECT at_active_ms FROM mono_ask_events
              WHERE ask_id = ?1 AND kind = ?2 AND seq = ?3",
            rusqlite::params![id.as_str(), kind, seq],
            |row| row.get(0),
        )
        .unwrap()
}

fn reminders(world: &World, id: &MessageId) -> Vec<(String, i64)> {
    world
        .conn()
        .prepare("SELECT key, at_active_ms FROM mono_outbox WHERE ask_id = ?1 ORDER BY key")
        .unwrap()
        .query_map([id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The lead's ask on its worker, and the calls on it. `mono` started the
/// lead.
struct Asked {
    mono: MonoId,
    lead: SessionId,
    worker: SessionId,
    l: Caller,
    w: Caller,
    ask: MessageId,
}

impl Asked {
    fn call(
        &self,
        world: &World,
        caller: &Caller,
        key: &str,
        tool: &str,
        input: Value,
    ) -> Submitted {
        world.tool(caller, key, tool, input, &[&self.lead, &self.worker])
    }

    fn question(&self, world: &World, seq: i64) -> Submitted {
        let input = json!({"message": format!("Question {seq}")});
        self.call(world, &self.w, &format!("w-q{seq}"), "question", input)
    }

    fn answer(&self, world: &World, caller: &Caller, key: &str, seq: i64) -> Submitted {
        let input = json!({"ask_id": self.ask.as_str(), "seq": seq, "message": "Use main"});
        self.call(world, caller, key, "answer", input)
    }

    fn update(&self, world: &World, key: &str) -> Submitted {
        let input = json!({"message": "Still working"});
        self.call(world, &self.w, key, "update", input)
    }

    fn to_lead(&self, carries: Carries) -> Said {
        (Some(carries), None, Some(self.lead.as_str().into()))
    }
}

/// S4.1: the worker's question goes to the lead, keeps the key and pauses
/// the clock; a second one, or an answer from the worker or with another
/// seq, is refused. The lead cannot answer: its own question goes one hop
/// up, on its own ask, to its Mono. Returns when the clock paused.
fn questions_pause_and_go_up(world: &World, a: &Asked, due: i64, start_ask: &MessageId) -> i64 {
    let ask = &a.ask;
    let question = Carries::Question {
        ask_id: ask.clone(),
        seq: 1,
    };
    assert_eq!(said(world, a.question(world, 1)), a.to_lead(question));
    let paused_at = at(world, ask, "question", 1);
    assert_eq!(
        clock(world, ask),
        ("open".into(), Some(due), Some(paused_at), None)
    );
    let waits = TalkRefused::QuestionWaits { seq: 1 };
    refused(a.question(world, 2), Talk::Question, waits);
    let first = Talk::Answer { seq: 1 };
    refused(
        a.answer(world, &a.w, "w-a", 1),
        first,
        TalkRefused::NotYourAsk,
    );
    let other = TalkRefused::AnswerOtherSeq { seq: 1 };
    refused(
        a.answer(world, &a.l, "l-a2", 2),
        Talk::Answer { seq: 2 },
        other,
    );

    let input = json!({"message": "Which branch?"});
    let up = a.call(world, &a.l, "l-q1", "question", input);
    let up_carries = Carries::Question {
        ask_id: start_ask.clone(),
        seq: 1,
    };
    let mono = a.mono.as_str().to_string();
    assert_eq!(said(world, up), (Some(up_carries), Some(mono), None));
    paused_at
}

/// S4.4: the Mono's answer comes down to the lead, and the lead's to the
/// worker; each clock resumes where it paused, so the worker's deadline
/// moves by the active time its question waited. Returns the new deadline.
fn answers_come_down(
    world: &World,
    a: &Asked,
    (due, paused_at): (i64, i64),
    (m, start_ask): (&Caller, &MessageId),
) -> i64 {
    let ask = &a.ask;
    let down = json!({"ask_id": start_ask.as_str(), "seq": 1, "message": "main"});
    let down_carries = Carries::Answer {
        ask_id: start_ask.clone(),
        seq: 1,
    };
    let got = said(world, a.call(world, m, "m-a1", "answer", down));
    assert_eq!(got, a.to_lead(down_carries));
    assert_eq!(clock(world, start_ask).2, None, "the lead's clock resumes");
    let answer = Carries::Answer {
        ask_id: ask.clone(),
        seq: 1,
    };
    let to_worker = (Some(answer), None, Some(a.worker.as_str().into()));
    assert_eq!(said(world, a.answer(world, &a.l, "l-a1", 1)), to_worker);
    let resumed_at = at(world, ask, "answer", 1);
    assert!(resumed_at - paused_at >= 3 * HOUR);
    let resumed = due + resumed_at - paused_at;
    assert_eq!(
        clock(world, ask),
        ("open".into(), Some(resumed), None, Some(resumed - T5_LEAD)),
        "the clock resumes where it paused: the same active time is left"
    );
    let none_waits = TalkRefused::NoQuestionWaits;
    refused(
        a.answer(world, &a.l, "l-a1-again", 1),
        Talk::Answer { seq: 1 },
        none_waits,
    );
    resumed
}

/// S4.5: one update keeps the key and leaves the clock running; a 2nd is
/// refused. S2.4: questions 2 and 3, each answered; a 4th is refused.
fn one_update_and_three_questions(world: &World, a: &Asked, due: i64) {
    let update = Carries::Update {
        ask_id: a.ask.clone(),
    };
    assert_eq!(said(world, a.update(world, "w-u1")), a.to_lead(update));
    assert_eq!(
        clock(world, &a.ask),
        ("open".into(), Some(due), None, Some(due - T5_LEAD))
    );
    refused(
        a.update(world, "w-u2"),
        Talk::Update,
        TalkRefused::UpdateSent,
    );
    for seq in 2..=3 {
        said(world, a.question(world, seq));
        said(world, a.answer(world, &a.l, &format!("l-a{seq}"), seq));
    }
    refused(
        a.question(world, 4),
        Talk::Question,
        TalkRefused::NoQuestionsLeft,
    );
}

/// S4 "question() keeps the key and pauses the clock", "the clock jumps 3
/// hours while it waits: no reminder", "the answer comes down and the clock
/// resumes where it paused", "one update() keeps the key and the clock
/// runs", and S2 "a 4th question(); a 2nd update()". A question the lead
/// cannot answer goes up as the lead's own question on its own ask. Defect:
/// a question spends the key or leaves the clock running, so a reminder
/// comes while it waits; an answer resumes the clock from the answer's time
/// instead of where it paused, or answers a question that does not wait;
/// a 4th question or a 2nd update gets through; or the wrong party answers.
#[test]
fn s4_a_question_pauses_the_clock_and_its_answer_resumes_it() {
    let mut world = World::new("s4-questions");
    let mono = world.add_mono_in("s4-mono", MonoRole::Mono, &[P]);
    let lead = world.start_lead(&mono, "s4-lead", P);
    let worker = world.add_session("s4-worker", P, Some("s4-lead"));
    world.advance(MINUTE);
    let l = session_caller(&lead, P);
    let input = json!({"message": "Fix the build", "send_to": worker.as_str(), "due_minutes": 30});
    let ask = written(world.tool(&l, "ask", "ask", input, &[&lead, &worker]));
    let start_ask: String = world
        .conn()
        .query_row(
            "SELECT id FROM mono_asks WHERE child_actor = 'session:s4-lead'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let start_ask = MessageId::parse(&start_ask).unwrap();
    let a = Asked {
        mono: mono.id.clone(),
        w: session_caller(&worker, P),
        l,
        lead,
        worker,
        ask,
    };
    let (_, Some(due), None, Some(next_due)) = clock(&world, &a.ask) else {
        panic!("a new ask runs its clock");
    };
    assert_eq!(next_due, due - T5_LEAD);
    world.advance(2 * MINUTE);
    let paused_at = questions_pause_and_go_up(&world, &a, due, &start_ask);

    // S4.3: three hours of active time while both wait: no reminder.
    world.advance(3 * HOUR);
    assert!(
        reminders(&world, &a.ask).is_empty(),
        "no reminder while paused"
    );
    assert!(
        reminders(&world, &start_ask).is_empty(),
        "no reminder while paused"
    );
    assert_eq!(clock(&world, &a.ask).3, None);

    let m = mono_caller(&mono, "m1");
    let resumed = answers_come_down(&world, &a, (due, paused_at), (&m, &start_ask));
    one_update_and_three_questions(&world, &a, resumed);

    // The resumed ladder: the reminder falls due at its moved time, and
    // not a millisecond before.
    let (_, Some(due), None, Some(t5)) = clock(&world, &a.ask) else {
        panic!("the clock runs after the answers");
    };
    assert_eq!(t5, due - T5_LEAD);
    world.advance(t5 - 1 - world.active_ms());
    assert!(reminders(&world, &a.ask).is_empty(), "not a ms before");
    world.advance(1);
    let key = format!("reminder:{}:t5", a.ask.as_str());
    assert_eq!(reminders(&world, &a.ask), [(key, t5)]);
}
