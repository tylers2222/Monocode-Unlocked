//! R14, worlds S2 and S4: a question keeps the key and pauses the ask's
//! clock, its answer resumes it where it paused, one update keeps the key
//! with the clock running, and the limits hold.

use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller};
use super::asks::written;
use super::world::World;
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

/// A refusal the caller reads: its exact code, and a hint naming the fix.
fn refused(submitted: Submitted, code: &str, fix: &str) {
    match submitted {
        Submitted::Failed { error } => {
            assert_eq!(error.code, code, "{}", error.hint);
            assert!(error.hint.contains(fix), "{code}: {}", error.hint);
        }
        other => panic!("not refused: {other:?}"),
    }
}

fn repeated(submitted: &Submitted) -> bool {
    matches!(
        submitted,
        Submitted::Accepted {
            duplicate: true,
            ..
        }
    )
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
    refused(a.question(world, 2), "question_refused", "after it comes");
    refused(
        a.answer(world, &a.w, "w-a", 1),
        "answer_refused",
        "Only the one who asked",
    );
    refused(
        a.answer(world, &a.l, "l-a2", 2),
        "answer_refused",
        "Answer it with seq 1.",
    );
    // An answer names its ask: the lead's own open ask is not guessed.
    let unnamed = json!({"seq": 1, "message": "Use main"});
    let got = a.call(world, &a.l, "l-a-unnamed", "answer", unnamed);
    refused(got, "invalid_input", "ask_id must be");
    for (key, seq) in [
        ("l-a0", json!(0)),
        ("l-a-neg", json!(-1)),
        ("l-a-none", Value::Null),
    ] {
        let mut input = json!({"ask_id": ask.as_str(), "message": "Use main"});
        if !seq.is_null() {
            input["seq"] = seq;
        }
        let got = a.call(world, &a.l, key, "answer", input);
        refused(got, "invalid_input", "seq must be the seq (1 to 3)");
    }

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
    // The Mono's window lists no session: the idle lead it started still
    // gets the answer.
    let got = said(world, world.tool(m, "m-a1", "answer", down, &[]));
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
    refused(
        a.answer(world, &a.l, "l-a1-again", 1),
        "answer_refused",
        "No question on this ask waits",
    );
    // The seq is part of the call: the same request id replays its answer,
    // and with another seq it is another call.
    assert!(repeated(&a.answer(world, &a.l, "l-a1", 1)));
    refused(
        a.answer(world, &a.l, "l-a1", 2),
        "request_id_reused",
        "Use a new request id.",
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
        "update_refused",
        "Ack it when the work is done.",
    );
    // A request id the question used is that call's, not this update's,
    // even before its message is read.
    let reused = a.call(world, &a.w, "w-q1", "update", json!({"message": ""}));
    refused(reused, "request_id_reused", "Use a new request id.");
    for seq in 2..=3 {
        said(world, a.question(world, seq));
        said(world, a.answer(world, &a.l, &format!("l-a{seq}"), seq));
    }
    refused(
        a.question(world, 4),
        "question_refused",
        "already had its 3 questions",
    );
}

/// A reminder queued before a question is withdrawn while it waits, and
/// its rung runs again from the moved deadline.
fn a_queued_reminder_waits_out_the_question(world: &mut World, a: &Asked, docs: &MessageId) {
    let (_, Some(docs_due), None, Some(docs_t5)) = clock(world, docs) else {
        panic!("the second ask runs its clock");
    };
    world.advance(docs_t5 - world.active_ms());
    assert_eq!(reminders(world, docs).len(), 1, "t5 is queued");
    let question = json!({"ask_id": docs.as_str(), "message": "Which docs?"});
    said(world, a.call(world, &a.w, "w-dq1", "question", question));
    assert!(
        reminders(world, docs).is_empty(),
        "no reminder while paused"
    );
    assert_eq!(
        reminders(world, &a.ask).len(),
        1,
        "another ask's reminder stays"
    );
    let paused_at = at(world, docs, "question", 1);
    world.advance(HOUR);
    let answer = json!({"ask_id": docs.as_str(), "seq": 1, "message": "All"});
    said(world, a.call(world, &a.l, "l-da1", "answer", answer));
    let resumed = docs_due + at(world, docs, "answer", 1) - paused_at;
    assert_eq!(
        clock(world, docs),
        ("open".into(), Some(resumed), None, Some(resumed - T5_LEAD)),
        "t5 falls due again from the moved deadline"
    );
    // It paused in its t5 window, so the rung falls due again at once.
    world.advance(MINUTE);
    let keys: Vec<String> = reminders(world, docs)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(keys, [format!("reminder:{}:t5", docs.as_str())]);
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
/// Also: a refusal loses its code or its hint; an answer guesses its ask,
/// or its request id ignores the seq; an idle lead no window lists misses
/// the Mono's answer; a call with no open ask, or two, is misread; another
/// call's request id is taken as this one's; a repeat that named no ask is
/// refused once a second ask is open; or a reminder queued before a
/// question goes out while it waits, or never runs again after the answer.
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
    // No ask was sent to the Mono, so a question naming none has no ask.
    let none = world.tool(&m, "m-q", "question", json!({"message": "Why?"}), &[]);
    refused(none, "question_refused", "Name it with ask_id.");
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

    // A second open ask to the worker: a repeat of the first question
    // still replays its outcome, and a new question must name its ask.
    let input = json!({"message": "Fix the docs", "send_to": a.worker.as_str(), "due_minutes": 30});
    let docs = written(world.tool(&a.l, "ask-2", "ask", input, &[&a.lead, &a.worker]));
    assert!(repeated(&a.question(&world, 1)), "w-q1 replays");
    let unnamed = a.call(
        &world,
        &a.w,
        "w-q-two",
        "question",
        json!({"message": "Which?"}),
    );
    refused(
        unnamed,
        "invalid_input",
        "name the one you mean with ask_id",
    );
    a_queued_reminder_waits_out_the_question(&mut world, &a, &docs);
}
