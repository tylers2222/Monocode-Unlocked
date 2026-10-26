//! S1 (R7): a start is an ask. A Giga asks a Mono with no clock, the Mono
//! starts two leads with a clock, each lead starts a worker, and every ask
//! is acked from the bottom up, with no tracker configured.

use rusqlite::OptionalExtension;
use serde_json::{json, Value};

use super::agents::{mono_caller, session_caller, Mono, Started};
use super::asks::{ack_went_to, written};
use super::world::World;
use crate::asks::Carries;
use crate::mono_gate::Caller;
use crate::mono_messages::{MessageId, SessionId, Submitted, ToolError};
use crate::mono_roster::MonoRole;

const P: &str = "/s1";
const MINUTE: i64 = 60_000;

/// What the app keeps for a configured tracker, in its data folder.
const TRACKER_FILES: [&str; 4] = [
    "linear-token",
    "jira-config.json",
    "gitlab-config.json",
    "azure-devops-config.json",
];

fn start_input(due: Option<u32>) -> Value {
    let mut input = json!({"prompt": "Do your part", "harness": "codex", "model": "codex:default"});
    if let Some(due) = due {
        input["due_minutes"] = json!(due);
    }
    input
}

/// Ask `id` as stored: parent, child, its deadline in active minutes from
/// `at`, and its state. `None` when no ask has the id.
fn ask(world: &World, id: &MessageId, at: i64) -> Option<(String, String, Option<i64>, String)> {
    world
        .conn()
        .query_row(
            "SELECT parent_actor, child_actor, due_active_ms, state FROM mono_asks WHERE id = ?1",
            [id.as_str()],
            |row| {
                let due: Option<i64> = row.get(2)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    due.map(|due| (due - at) / MINUTE),
                    row.get(3)?,
                ))
            },
        )
        .optional()
        .unwrap()
}

fn saved(world: &World, session: &str) -> bool {
    world
        .conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
            [session],
            |row| row.get(0),
        )
        .unwrap()
}

/// One ask of the S1 tree: its id, its parent and child actors, its
/// deadline in active minutes after the asks, and the caller that acks it.
struct Asked {
    id: MessageId,
    parent: String,
    child: String,
    due: Option<i64>,
    acker: Caller,
}

/// Where an ack to `parent` goes: its Mono or its session.
fn to(parent: &str) -> (Option<&str>, Option<&str>) {
    match parent.split_once(':').unwrap() {
        ("mono", id) => (Some(id), None),
        (_, id) => (None, Some(id)),
    }
}

fn the_live() -> Vec<SessionId> {
    ["s1-lead-a", "s1-lead-b", "s1-worker-a", "s1-worker-b"]
        .map(|id| SessionId::parse(id).unwrap())
        .to_vec()
}

/// `by` starts session `id` with `due` minutes; the same call again is a
/// repeat under the same request id.
fn start(world: &World, by: &Caller, id: &str, due: Option<u32>) -> Result<Started, ToolError> {
    let key = format!("start-{id}-{}", due.unwrap_or(0));
    world.start(by, id, P, &key, &start_input(due))
}

/// The ask a started session reads on its first prompt, and its due_in in
/// minutes. Its id is the start row the launch wrote after.
fn received(started: &Started) -> (MessageId, Option<i64>) {
    let ask = started
        .received
        .as_ref()
        .expect("the first prompt carries the start's ask");
    assert_eq!(ask.ask_id, started.message_id, "the ask is the start row");
    (ask.ask_id.clone(), ask.due_in_ms.map(|due| due / MINUTE))
}

/// Review r1 N2: an ask under the request id of `by`'s earlier start `key` is
/// another call.
fn ask_under_start_key(world: &World, by: &Caller, key: &str) {
    let input = json!({"message": "Again", "send_to": "s1-lead-a"});
    let live = the_live();
    let reused = world.tool(by, key, "ask", input, &live.iter().collect::<Vec<_>>());
    assert!(
        matches!(&reused, Submitted::Failed { error } if error.code == "request_id_reused"),
        "{reused:?}"
    );
}

/// The S1 tree: the Giga asks the Mono with no clock, the Mono starts two
/// leads, each lead a worker. Each ask's id is the one its child was
/// handed. Its asks bottom up, and the Giga's start.
fn grow(world: &mut World, giga: &Caller, mono: &Mono) -> ([Asked; 5], Started) {
    let live = the_live();
    written(world.tool(
        giga,
        "s1-project",
        "ask",
        json!({"message": "Run the project", "send_to": "s1-mono", "due_minutes": 30}),
        &live.iter().collect::<Vec<_>>(),
    ));
    let project = world
        .start_turn(&mono.chat, "m1")
        .into_iter()
        .find(|row| matches!(row.carries, Some(Carries::Ask { .. })))
        .expect("the Mono's turn takes the Giga's ask")
        .message
        .id;
    let mono_turn = mono_caller(mono, "m1");
    let giga_start = start(world, giga, "s1-giga-lead", None).unwrap();
    let lead_a = start(world, &mono_turn, "s1-lead-a", Some(30)).unwrap();
    let again = start(world, &mono_turn, "s1-lead-a", Some(30));
    assert_eq!(
        again.as_ref(),
        Ok(&lead_a),
        "a repeat answers its first row"
    );
    ask_under_start_key(world, &mono_turn, "start-s1-lead-a-30");
    let lead_b = start(world, &mono_turn, "s1-lead-b", None).unwrap();
    // Plan section 2: due_minutes is 10 to 480, refused before the launch.
    for due in [9, 481] {
        let refused = start(world, &mono_turn, "s1-lead-x", Some(due)).unwrap_err();
        assert_eq!(refused.code, "invalid_input", "due_minutes {due}");
        assert!(!saved(world, "s1-lead-x"), "{due} launched nothing");
    }
    // Review r1 N3: a deadline on a start that asks nothing is refused.
    let mut draft = start_input(Some(30));
    draft["draft"] = json!(true);
    for (by, id, input) in [
        (giga, "s1-giga-x", start_input(Some(30))),
        (&mono_turn, "s1-draft-x", draft),
    ] {
        let refused = world.start(by, id, P, id, &input).unwrap_err();
        assert_eq!(refused.code, "invalid_input", "{id}");
        assert!(!saved(world, id), "{id} launched nothing");
    }
    let lead = |at: usize| session_caller(&live[at], P);
    let worker_a = start(world, &lead(0), "s1-worker-a", Some(20)).unwrap();
    let worker_b = start(world, &lead(1), "s1-worker-b", None).unwrap();
    // The level gate answers before the range check.
    let spawned = start(world, &lead(2), "s1-worker-x", Some(5)).unwrap_err();
    assert_eq!(spawned.code, "worker_cannot_spawn");
    let asks = [
        (
            received(&worker_a),
            "session:s1-lead-a",
            "session:s1-worker-a",
            Some(20),
            lead(2),
        ),
        (
            received(&worker_b),
            "session:s1-lead-b",
            "session:s1-worker-b",
            Some(60),
            lead(3),
        ),
        (
            received(&lead_a),
            "mono:s1-mono",
            "session:s1-lead-a",
            Some(30),
            lead(0),
        ),
        (
            received(&lead_b),
            "mono:s1-mono",
            "session:s1-lead-b",
            Some(60),
            lead(1),
        ),
        (
            (project, None),
            "mono:s1-giga",
            "mono:s1-mono",
            None,
            mono_turn.clone(),
        ),
    ];
    let asks = asks.map(|((id, due_in), parent, child, due, acker)| {
        assert_eq!(due_in, due, "{child} reads its due_in");
        Asked {
            id,
            parent: parent.into(),
            child: child.into(),
            due,
            acker,
        }
    });
    (asks, giga_start)
}

/// Defect: a start writes no ask, or its first prompt does not carry the
/// ask's id (review r1 M1), so a lead or worker has nothing to ack; a
/// repeated start writes its ask again (or reopens an acked one); a Giga's
/// ask or start gets a clock, or an ask from level 2 down has none; a start
/// with a `due_minutes` out of range, or on a start that asks nothing,
/// launches anyway; or the flow needs a tracker to work.
#[test]
fn s1_a_start_is_an_ask_and_every_ask_is_acked_bottom_up() {
    let mut world = World::new("s1-asks");
    // Active time before the asks, so a deadline that forgets it shows.
    world.advance(30_000);
    let giga = mono_caller(&world.add_mono_in("s1-giga", MonoRole::Giga, &[P]), "g1");
    let mono = world.add_mono_in("s1-mono", MonoRole::Mono, &[P]);
    let at: i64 = world
        .conn()
        .query_row("SELECT active_ms FROM mono_clock WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(at > 0, "the clock ran");
    let (asks, giga_start) = grow(&mut world, &giga, &mono);
    // The asks are rows: a kill loses none.
    world.down(MINUTE);
    for asked in &asks {
        assert_eq!(
            ask(&world, &asked.id, at),
            Some((
                asked.parent.clone(),
                asked.child.clone(),
                asked.due,
                "open".into()
            )),
            "S1.1 No clock on Giga to Mono; a clock on every ask from level 2 down"
        );
    }
    assert_eq!(giga_start.received, None, "a Giga's start is no ask");
    assert_eq!(ask(&world, &giga_start.message_id, at), None);

    // Every ask is acked from the bottom up, with the id its child was
    // handed; each ack reaches only its parent.
    let live = the_live();
    let live: Vec<&SessionId> = live.iter().collect();
    for asked in &asks {
        let input = json!({"ask_id": asked.id.as_str(), "message": "Done"});
        let key = format!("ack-{}", asked.id.as_str());
        written(world.tool(&asked.acker, &key, "ack", input, &live));
        let went = ack_went_to(&world, &asked.id);
        assert_eq!((went.0.as_deref(), went.1.as_deref()), to(&asked.parent));
        assert_eq!(ask(&world, &asked.id, at).unwrap().3, "acked");
    }
    // A repeat after the ack still writes nothing: the ask stays acked.
    let lead_a = &asks[2].id;
    let again = start(&world, &asks[4].acker, "s1-lead-a", Some(30));
    assert_eq!(again.map(|started| started.message_id), Ok(lead_a.clone()));
    assert_eq!(
        ask(&world, lead_a, at).unwrap().3,
        "acked",
        "a repeat reopens nothing"
    );
    for asked in &asks {
        let to_child = world.conn().query_row(
            "SELECT count(*) FROM mono_asks WHERE child_actor = ?1",
            [&asked.child],
            |row| row.get::<_, i64>(0),
        );
        assert_eq!(to_child.unwrap(), 1, "one ask to {}", asked.child);
    }

    let folder = world.path().parent().unwrap();
    for file in TRACKER_FILES {
        assert!(
            !folder.join(file).exists(),
            "S1.5 No tracker is used anywhere, and everything still works: {file}"
        );
    }
}
