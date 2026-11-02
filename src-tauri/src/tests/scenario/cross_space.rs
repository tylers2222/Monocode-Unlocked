//! ADE-174: across Spaces only a regular Mono messages a regular Mono, with
//! new work or a reply. Each call runs through the app's own submit or level
//! check path with Mono B, its chat and the worker in Space 2, then again
//! with every party in Space 1, where it gets the answer it had before
//! Spaces. Every checkpoint prints `ok` or `FAIL`, so one run shows every
//! rule a change broke.

use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::mono_gate::Caller;
use crate::mono_messages::{self, ActionChecked, MessageId, RequestId, SessionId, Submitted};
use crate::mono_roster::MonoRole;
use crate::spaces::FIRST_SPACE;

const OTHER_SPACE: &str = "space-2";
const CROSS: &str = "cross_space_forbidden";

fn send(to: &str, intent: &str) -> Value {
    json!({"message": "Status?", "send_to": to, "wanting_reply": false, "intent": intent})
}

fn steer(to: &str, reply_to: &MessageId) -> Value {
    let mut input = send(to, "steer");
    input["reply_to"] = reply_to.as_str().into();
    input
}

/// What a checkpoint calls: a messaging tool, or an app action on a session.
enum Act<'a> {
    Tool(&'static str, Value),
    Action(&'static str, &'a SessionId),
}

/// One checkpoint: its answer across Spaces, and with every party in one.
struct Row<'a> {
    name: &'static str,
    caller: &'a Caller,
    act: Act<'a>,
    across: &'static str,
    same: &'static str,
}

fn row<'a>(name: &'static str, caller: &'a Caller, act: Act<'a>, same: &'static str) -> Row<'a> {
    Row {
        name,
        caller,
        act,
        across: CROSS,
        same,
    }
}

/// A Giga and Mono A in Space 1; Mono B; a lead that Mono A started in `/p`
/// with a worker that holds the lead's ask; a lead that Mono B started,
/// which holds B's ask and has asked it one question; and a session the window lists that MonoCode
/// never saved.
struct Cast {
    world: World,
    mono_b: Mono,
    lead: SessionId,
    worker: SessionId,
    lead_b: SessionId,
    unsaved: SessionId,
    ask_b: MessageId,
    ask_worker: MessageId,
    giga: Caller,
    a: Caller,
    b: Caller,
    lead_c: Caller,
    worker_c: Caller,
    lead_b_c: Caller,
}

impl Cast {
    fn new() -> Self {
        let world = World::new("cross-space");
        let sql = "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 2', 0)";
        world.conn().execute(sql, [OTHER_SPACE]).unwrap();
        let giga = world.add_mono("x-giga", MonoRole::Giga);
        let mono_a = world.add_mono_in("x-mono-a", MonoRole::Mono, &["/p"]);
        let mono_b = world.add_mono_in("x-mono-b", MonoRole::Mono, &["/p"]);
        let lead = world.start_lead(&mono_a, "x-lead", "/p");
        let input = json!({"prompt": "Work", "harness": "codex", "model": "codex:default"});
        let lead_c = session_caller(&lead, "/p");
        let ask_worker = world
            .start(&lead_c, "x-worker", "/p", "start-worker", &input)
            .unwrap()
            .message_id;
        let b = mono_caller(&mono_b, "t1");
        let started = world.start(&b, "x-lead-b", "/p", "start-lead-b", &input);
        let ask_b = started.unwrap().message_id;
        let worker = SessionId::parse("x-worker").unwrap();
        let lead_b = SessionId::parse("x-lead-b").unwrap();
        let cast = Self {
            giga: mono_caller(&giga, "t1"),
            a: mono_caller(&mono_a, "t1"),
            b,
            lead_c,
            worker_c: session_caller(&worker, "/p"),
            lead_b_c: session_caller(&lead_b, "/p"),
            world,
            mono_b,
            lead,
            worker,
            lead_b,
            unsaved: SessionId::parse("x-unsaved").unwrap(),
            ask_b,
            ask_worker,
        };
        let question = json!({"ask_id": cast.ask_b.as_str(), "message": "Which branch?"});
        let asked = cast.call(
            &cast.lead_b_c,
            "question-1",
            Act::Tool("question", question),
        );
        assert_eq!(asked.0, "accepted", "the question the answer rows answer");
        cast
    }

    /// Mono B, its chat and the worker move to `space`.
    fn move_to(&self, space: &str) {
        let conn = self.world.conn();
        for (table, id) in [
            ("monos", self.mono_b.id.as_str()),
            ("sessions", self.mono_b.chat.as_str()),
            ("sessions", self.worker.as_str()),
        ] {
            let sql = format!("UPDATE {table} SET space_id = ?1 WHERE id = ?2");
            assert_eq!(conn.execute(&sql, [space, id]).unwrap(), 1, "{table} {id}");
        }
    }

    /// The code and hint `act` from `caller` gets: `accepted` or `allowed`
    /// when it goes through.
    fn call(&self, caller: &Caller, request: &str, act: Act) -> (String, String) {
        let live = [
            &self.mono_b.chat,
            &self.lead,
            &self.worker,
            &self.lead_b,
            &self.unsaved,
        ];
        let error = match act {
            Act::Tool(tool, input) => match self.world.tool(caller, request, tool, input, &live) {
                Submitted::Accepted { .. } => return ("accepted".into(), String::new()),
                Submitted::Failed { error } => error,
            },
            Act::Action(action, target) => {
                let input = json!({"sessionId": target.as_str(), "prompt": "Go on"});
                let checked = mono_messages::check_action(
                    &self.world.conn(),
                    caller,
                    action,
                    Some(target.as_str()),
                    None,
                    &RequestId::parse(request).unwrap(),
                    &input,
                    self.world.now(),
                );
                match checked.unwrap() {
                    ActionChecked::Allowed { .. } => return ("allowed".into(), String::new()),
                    ActionChecked::Failed { error } => error,
                }
            }
        };
        (error.code.into(), error.hint)
    }

    /// Each row as checkpoint `<round> <name>`; a failure goes to `failed`.
    /// Returns each row's hint.
    fn check(&self, failed: &mut Vec<String>, round: &str, rows: Vec<Row>) -> Vec<String> {
        let mut hints = Vec::new();
        for row in rows {
            let name = format!("{round} {}", row.name);
            let want = if round == "across" {
                row.across
            } else {
                row.same
            };
            // A request id is `[A-Za-z0-9_-]`; names hold spaces, dots and `'`.
            let request: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let (got, hint) = self.call(row.caller, &request, row.act);
            if got == want {
                println!("ok   {name}: {got}");
            } else {
                println!("FAIL {name}: {got}, expected {want}");
                failed.push(name);
            }
            hints.push(hint);
        }
        hints
    }

    /// `messages.send` and `ask` (design 16.5), the steers included.
    fn message_rows<'a>(&'a self, sent: &MessageId) -> Vec<Row<'a>> {
        let b = self.mono_b.id.as_str();
        let mut whole_turn = send(b, "steer");
        whole_turn["scope"] = "turn".into();
        let ask = json!({"message": "Do this", "send_to": b});
        let tool = Act::Tool;
        vec![
            // B's chat has not started the message's turn.
            row(
                "Mono steers Mono",
                &self.a,
                tool("send_message", steer(b, sent)),
                "target_gone",
            ),
            row(
                "Mono steers Mono's whole turn",
                &self.a,
                tool("send_message", whole_turn),
                "steer_scope_not_allowed",
            ),
            row(
                "Giga to Mono",
                &self.giga,
                tool("send_message", send(b, "new")),
                "accepted",
            ),
            row("Giga asks Mono", &self.giga, tool("ask", ask), "accepted"),
            row(
                "session to session",
                &self.lead_c,
                tool("send_message", send(self.worker.as_str(), "new")),
                "accepted",
            ),
            row(
                "Mono to session",
                &self.b,
                tool("send_message", send(self.lead.as_str(), "new")),
                "accepted",
            ),
            row(
                "Mono to unsaved session",
                &self.b,
                tool("send_message", send(self.unsaved.as_str(), "new")),
                "accepted",
            ),
            row(
                "worker to its lead",
                &self.worker_c,
                tool("send_message", send(self.lead.as_str(), "new")),
                "accepted",
            ),
            row(
                "lead to its Mono",
                &self.lead_b_c,
                tool("send_message", send(b, "new")),
                "accepted",
            ),
            Row {
                across: "worker_scope",
                ..row(
                    "worker to Giga",
                    &self.worker_c,
                    tool("send_message", send("x-giga", "new")),
                    "worker_scope",
                )
            },
            Row {
                across: "worker_scope",
                ..row(
                    "worker to made-up id",
                    &self.worker_c,
                    tool("send_message", send("x-made-up", "new")),
                    "worker_scope",
                )
            },
        ]
    }

    /// The calls on B's ask to its lead (R14), and app actions on sessions
    /// (design 6.6). The ack closes the ask, so it comes last.
    fn ask_and_action_rows(&self, round: &str) -> Vec<Row<'_>> {
        let ask_id = self.ask_b.as_str();
        let on_ask = |tool: &'static str, extra: Value| {
            let mut input = json!({"ask_id": ask_id, "message": format!("{round} {tool}")});
            input
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Act::Tool(tool, input)
        };
        vec![
            row(
                "Mono answers its lead",
                &self.b,
                on_ask("answer", json!({"seq": 1})),
                "accepted",
            ),
            row(
                "lead updates its Mono",
                &self.lead_b_c,
                on_ask("update", json!({})),
                "accepted",
            ),
            // B's lead still has question 1 open across Spaces, so the
            // worker, which has none, asks the question.
            row(
                "worker asks its lead a question",
                &self.worker_c,
                Act::Tool(
                    "question",
                    json!({"ask_id": self.ask_worker.as_str(), "message": "Which file?"}),
                ),
                "accepted",
            ),
            row(
                "lead sessions.send to its worker",
                &self.lead_c,
                Act::Action("sessions.send", &self.worker),
                "allowed",
            ),
            row(
                "lead sessions.draft in its worker",
                &self.lead_c,
                Act::Action("sessions.draft", &self.worker),
                "allowed",
            ),
            row(
                "Giga sessions.read of a worker",
                &self.giga,
                Act::Action("sessions.read", &self.worker),
                "allowed",
            ),
            row(
                "Giga sessions.send to Mono's chat",
                &self.giga,
                Act::Action("sessions.send", &self.mono_b.chat),
                "allowed",
            ),
            row(
                "Mono sessions.send to Mono's chat",
                &self.a,
                Act::Action("sessions.send", &self.mono_b.chat),
                "allowed",
            ),
            row(
                "lead acks its Mono",
                &self.lead_b_c,
                Act::Tool("ack", json!({"ask_id": ask_id, "message": "Done"})),
                "accepted",
            ),
        ]
    }

    /// The lead's other actions on its worker (design 6.6, A8).
    fn lifecycle_rows(&self) -> Vec<Row<'_>> {
        [
            "sessions.stop",
            "sessions.queue.clear",
            "sessions.archive",
            "sessions.delete",
        ]
        .into_iter()
        .map(|action| {
            row(
                action,
                &self.lead_c,
                Act::Action(action, &self.worker),
                "allowed",
            )
        })
        .collect()
    }
}

/// Defect: a Giga, a session, an ask, a steer, an app action or a reply
/// reaches a party in another Space; a party with no saved row skips the
/// rule; a worker can tell a real id in another Space from a made-up one;
/// a regular Mono cannot send new work to one; or a same-Space call gets a
/// different answer than it had before Spaces.
#[test]
fn only_a_regular_mono_messages_across_spaces() {
    let mut cast = Cast::new();
    let b = cast.mono_b.id.as_str();
    let mut failed = Vec::new();

    cast.move_to(OTHER_SPACE);
    let peer = cast.check(
        &mut failed,
        "across",
        vec![Row {
            across: "accepted",
            ..row(
                "Mono to Mono",
                &cast.a,
                Act::Tool("send_message", send(b, "new")),
                "accepted",
            )
        }],
    );
    assert!(peer[0].is_empty(), "the peer send is the steer's target");
    let sent: MessageId = cast
        .world
        .conn()
        .query_row(
            "SELECT id FROM mono_messages WHERE request_id = 'across-Mono-to-Mono'",
            [],
            |row| Ok(MessageId::parse(&row.get::<_, String>(0)?).unwrap()),
        )
        .unwrap();
    for (round, space) in [("across", OTHER_SPACE), ("same", FIRST_SPACE)] {
        cast.move_to(space);
        let hints = cast.check(&mut failed, round, cast.message_rows(&sent));
        let (to_giga, made_up) = (&hints[hints.len() - 2], &hints[hints.len() - 1]);
        if to_giga != made_up {
            println!("FAIL {round} worker hints differ: {to_giga:?} vs {made_up:?}");
            failed.push(format!("{round} worker hints"));
        }
        cast.check(&mut failed, round, cast.lifecycle_rows());
        cast.check(&mut failed, round, cast.ask_and_action_rows(round));
    }

    // A reply on a handle that opened in one Space, after B moves (m4).
    let wants = json!({"message": "Ready?", "send_to": b, "wanting_reply": true, "intent": "new"});
    let asked = cast.call(
        &cast.giga,
        "giga-wants-reply",
        Act::Tool("send_message", wants),
    );
    assert_eq!(asked.0, "accepted", "the message B replies to");
    cast.world.start_turn(&cast.mono_b.chat, "tb");
    let replier = mono_caller(&cast.mono_b, "tb");
    let reply = || Act::Tool("reply", json!({"message": "Yes", "wanting_reply": false}));
    for (round, space) in [("across", OTHER_SPACE), ("same", FIRST_SPACE)] {
        cast.move_to(space);
        let rows = vec![row("Mono replies to Giga", &replier, reply(), "accepted")];
        cast.check(&mut failed, round, rows);
    }
    assert!(failed.is_empty(), "failed checkpoints: {failed:#?}");
}
