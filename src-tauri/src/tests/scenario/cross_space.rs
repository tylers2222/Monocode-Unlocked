//! ADE-174: across Spaces only a regular Mono messages a regular Mono, with
//! new work. Each cross-Space call runs through the app's own submit path,
//! then the same call runs again with both parties in one Space and gets the
//! answer it had before Spaces. Every checkpoint prints `ok` or `FAIL`, so
//! one run shows every rule a change broke.

use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono};
use super::world::World;
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, SessionId, Submitted};
use crate::mono_roster::MonoRole;

const OTHER_SPACE: &str = "space-2";

fn send(to: &str, intent: &str, reply_to: Option<&MessageId>) -> Value {
    let mut input = json!({
        "message": "Status?", "send_to": to, "wanting_reply": false, "intent": intent,
    });
    if let Some(id) = reply_to {
        input["reply_to"] = id.as_str().into();
    }
    input
}

/// A Giga and Mono A in Space 1, Mono B, and a lead that Mono A started in
/// `/p` with a worker of its own.
struct Cast {
    world: World,
    giga: Mono,
    mono_a: Mono,
    mono_b: Mono,
    lead: SessionId,
    worker: SessionId,
}

impl Cast {
    fn new() -> Self {
        let world = World::new("cross-space");
        world
            .conn()
            .execute(
                "INSERT INTO spaces (id, name, created_at) VALUES (?1, 'Space 2', 0)",
                [OTHER_SPACE],
            )
            .unwrap();
        let giga = world.add_mono("x-giga", MonoRole::Giga);
        let mono_a = world.add_mono_in("x-mono-a", MonoRole::Mono, &["/p"]);
        let mono_b = world.add_mono_in("x-mono-b", MonoRole::Mono, &["/p"]);
        let lead = world.start_lead(&mono_a, "x-lead", "/p");
        let input = json!({"prompt": "Work", "harness": "codex", "model": "codex:default"});
        let lead_caller = session_caller(&lead, "/p");
        world
            .start(&lead_caller, "x-worker", "/p", "start-worker", &input)
            .unwrap();
        Self {
            world,
            giga,
            mono_a,
            mono_b,
            lead,
            worker: SessionId::parse("x-worker").unwrap(),
        }
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

    /// `tool` from `caller` as checkpoint `name`; prints `ok` or `FAIL`
    /// and keeps each failure in `failed`.
    fn check(
        &self,
        failed: &mut Vec<String>,
        name: &str,
        (caller, tool, input): (&Caller, &str, Value),
        want: &str,
    ) -> Submitted {
        let live = [&self.mono_b.chat, &self.lead, &self.worker];
        let request = name.replace(' ', "-");
        let submitted = self.world.tool(caller, &request, tool, input, &live);
        let got = match &submitted {
            Submitted::Accepted { .. } => "accepted",
            Submitted::Failed { error } => error.code,
        };
        if got == want {
            println!("ok   {name}: {got}");
        } else {
            println!("FAIL {name}: {got}, expected {want}");
            failed.push(name.to_string());
        }
        submitted
    }
}

/// Defect: a Giga, a session, an ask or a steer reaches a party in another
/// Space; a regular Mono cannot send new work to one; or a same-Space call
/// gets a different answer than it had before Spaces.
#[test]
fn only_a_regular_mono_messages_across_spaces() {
    let cast = Cast::new();
    let (giga, a, b) = (
        mono_caller(&cast.giga, "t1"),
        mono_caller(&cast.mono_a, "t1"),
        mono_caller(&cast.mono_b, "t1"),
    );
    let lead = session_caller(&cast.lead, "/p");
    let to_b = cast.mono_b.id.as_str();
    let mut failed = Vec::new();

    cast.move_to(OTHER_SPACE);
    let peer = cast.check(
        &mut failed,
        "across regular Mono to regular Mono",
        (&a, "send_message", send(to_b, "new", None)),
        "accepted",
    );
    let Submitted::Accepted { messages, .. } = &peer else {
        panic!("the peer send is the steer's target: {peer:?}");
    };
    let sent = messages[0].message.id.clone();
    // Each call, and its answer with both parties in one Space.
    let calls = || {
        [
            (
                "regular Mono steers regular Mono",
                (&a, "send_message", send(to_b, "steer", Some(&sent))),
                // B's chat has not started the message's turn.
                "target_gone",
            ),
            (
                "Giga to Mono",
                (&giga, "send_message", send(to_b, "new", None)),
                "accepted",
            ),
            (
                "Giga asks Mono",
                (&giga, "ask", json!({"message": "Do this", "send_to": to_b})),
                "accepted",
            ),
            (
                "session to session",
                (
                    &lead,
                    "send_message",
                    send(cast.worker.as_str(), "new", None),
                ),
                "accepted",
            ),
            (
                "Mono to session",
                (&b, "send_message", send(cast.lead.as_str(), "new", None)),
                "accepted",
            ),
        ]
    };
    for (name, call, _) in calls() {
        let name = format!("across {name}");
        cast.check(&mut failed, &name, call, "cross_space_forbidden");
    }
    cast.move_to("space-1");
    for (name, call, same) in calls() {
        cast.check(&mut failed, &format!("same {name}"), call, same);
    }
    assert!(failed.is_empty(), "failed checkpoints: {failed:#?}");
}
