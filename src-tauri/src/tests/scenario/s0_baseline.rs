//! S0: today's Giga -> Mono request and reply, across kills and restarts.
//! It proves the harness, not Change R.

use serde_json::json;

use super::agents::Mono;
use super::world::World;
use crate::mono_messages::{MessageId, Submitted, Targeted, Tool};
use crate::mono_roster::MonoRole;

/// The stored columns a scenario reads back, as the database holds them.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    status: String,
    handled_by: Option<String>,
    turn: Option<String>,
    delivered_at: Option<i64>,
    closed_at: Option<i64>,
}

fn row(world: &World, id: &MessageId) -> Row {
    world
        .conn()
        .query_row(
            "SELECT status, handled_by_session, turn_id, delivered_at, closed_at
               FROM mono_messages WHERE id = ?1",
            [id.as_str()],
            |row| {
                Ok(Row {
                    status: row.get(0)?,
                    handled_by: row.get(1)?,
                    turn: row.get(2)?,
                    delivered_at: row.get(3)?,
                    closed_at: row.get(4)?,
                })
            },
        )
        .unwrap()
}

/// The ids of the rows a call wrote; panics on a refusal or a duplicate.
fn written(submitted: Submitted) -> Vec<MessageId> {
    match submitted {
        Submitted::Accepted {
            messages,
            duplicate: false,
            ..
        } => messages.into_iter().map(|row| row.message.id).collect(),
        other => panic!("not a new accept: {other:?}"),
    }
}

fn ids(rows: &[Targeted]) -> Vec<&MessageId> {
    rows.iter().map(|row| &row.message.id).collect()
}

/// Defect: a request or reply is lost by a kill, delivered twice after a
/// restart, or a repeated call after a restart writes a second row.
#[test]
fn s0_a_giga_request_and_its_reply_survive_kills_and_arrive_once() {
    let mut world = World::new("s0");
    let giga = world.add_mono("s0-giga", MonoRole::Giga);
    let mono = world.add_mono("s0-mono", MonoRole::Mono);
    let ask = ask_survives_a_kill(&mut world, &giga, &mono);
    let reply = mono_answers(&mut world, &giga, &mono, &ask);
    reply_arrives_once(&mut world, &giga, &reply);
    let rows: i64 = world
        .conn()
        .query_row(
            "SELECT count(*) FROM mono_messages WHERE from_mono_id IN ('s0-giga', 's0-mono')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2, "one ask and one reply");
}

/// The ask as it waits for the Mono's turn.
fn waiting(mono: &Mono) -> Row {
    Row {
        status: "open".into(),
        handled_by: Some(mono.chat.as_str().into()),
        turn: None,
        delivered_at: None,
        closed_at: None,
    }
}

/// The Giga asks while the Mono is idle, and the app dies before the
/// Mono's window takes the message. After the restart the ask waits, and
/// the Giga's tool call made again with its request id answers the first
/// result and writes no second row.
fn ask_survives_a_kill(world: &mut World, giga: &Mono, mono: &Mono) -> MessageId {
    let input = json!({
        "message": "Is CI green?", "send_to": mono.id.as_str(),
        "wanting_reply": true, "intent": "new",
    });
    world.advance(1_000);
    let [ask] = written(world.call(giga, "g1", "ask", Tool::SendMessage, input.clone(), mono))
        .try_into()
        .unwrap();
    world.down(30_000);
    assert_eq!(
        row(world, &ask),
        waiting(mono),
        "the ask waits after a kill"
    );
    let again = world.call(giga, "g2", "ask", Tool::SendMessage, input, mono);
    assert!(
        matches!(&again, Submitted::Accepted { duplicate: true, messages, .. }
            if messages.len() == 1 && messages[0].message.id == ask),
        "{again:?}"
    );
    ask
}

/// The Mono's window hands the ask to a turn and the app dies before the
/// provider takes it, so after a restart the ask waits again. The next turn
/// takes it and the Mono answers; the reply call confirms the turn's opener,
/// so the ask is delivered and answered at the reply's time.
fn mono_answers(world: &mut World, giga: &Mono, mono: &Mono, ask: &MessageId) -> MessageId {
    world.advance(1_000);
    assert_eq!(ids(&world.start_turn(&mono.chat, "m1")), [ask]);
    world.down(30_000);
    assert_eq!(row(world, ask), waiting(mono), "an untaken ask waits again");
    world.advance(1_000);
    assert_eq!(ids(&world.start_turn(&mono.chat, "m2")), [ask]);
    world.advance(5_000);
    let answer = json!({"message": "Yes, CI is green."});
    let [reply] = written(world.call(mono, "m2", "answer", Tool::Reply, answer, giga))
        .try_into()
        .unwrap();
    let replied_at = world.now();
    world.end_turn(&mono.chat, "m2", true);
    assert_eq!(
        row(world, ask),
        Row {
            status: "answered".into(),
            turn: Some("m2".into()),
            delivered_at: Some(replied_at),
            closed_at: Some(replied_at),
            ..waiting(mono)
        }
    );
    reply
}

/// The Giga's window hands the reply to a turn and the app dies before the
/// provider takes it, so after a restart the reply waits again. The next
/// turn takes it, and after another kill no turn gets it again.
fn reply_arrives_once(world: &mut World, giga: &Mono, reply: &MessageId) {
    world.advance(1_000);
    assert_eq!(ids(&world.start_turn(&giga.chat, "g3")), [reply]);
    world.down(60_000);
    let reply_waiting = Row {
        status: "delivered".into(),
        handled_by: Some(giga.chat.as_str().into()),
        turn: None,
        delivered_at: None,
        closed_at: None,
    };
    assert_eq!(
        row(world, reply),
        reply_waiting,
        "an untaken reply waits again"
    );
    world.advance(1_000);
    assert_eq!(ids(&world.start_turn(&giga.chat, "g4")), [reply]);
    let taken_at = world.now();
    world.end_turn(&giga.chat, "g4", true);
    world.down(60_000);
    assert_eq!(
        row(world, reply),
        Row {
            turn: Some("g4".into()),
            delivered_at: Some(taken_at),
            ..reply_waiting
        },
        "the reply was delivered once"
    );
    assert!(
        world.start_turn(&giga.chat, "g5").is_empty(),
        "no turn after a restart gets the reply again"
    );
}
