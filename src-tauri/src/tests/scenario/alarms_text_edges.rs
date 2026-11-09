//! ADE-277: the alarm text on rows that try to make it lie or fail: what an
//! adversary pass on the change found, each shown red first.

use serde_json::json;

use super::alarms_stall::{align, ran_a_turn, turn, MINUTE, N, PASS};
use super::alarms_text::{insert_episode, lead, reminder, replied, Lead, HOSTILE, WAKE};
use crate::alarms::test_taps;

/// Row `id` names `to` as what it replies to, as the tests above write it.
fn link_reply(scene: &Lead, id: &str, to: &str) {
    let conn = scene.world.conn();
    conn.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    conn.execute(
        "UPDATE mono_messages SET reply_to = ?1, hop = 1 WHERE id = ?2",
        [to, id],
    )
    .unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
        .unwrap();
}

// Defect: the linked-row test has no sender check, so a row the
// asker itself wrote reads as the agent's reply: "yes" with no reply at all.
#[test]
fn senders_own_row_is_no_reply() {
    // The asker's own follow-up names the ask.
    let mut scene = lead("int-sender-reply");
    let ask = scene.ask("asked", "Asked work");
    scene.takes_a_turn("t1");
    let word = scene.tell("follow", "Any news?");
    link_reply(&scene, &word, &ask);
    scene.world.advance(WAKE + PASS);
    let said = replied(&scene);
    assert!(!said.starts_with("yes ("), "reply_to by the asker: {said}");

    // The asker's answer is an event of the ask.
    let mut scene = lead("int-sender-event");
    let ask = scene.ask("asked", "Asked work");
    scene.takes_a_turn("t1");
    let word = scene.tell("answer", "The answer is 4");
    scene
        .world
        .conn()
        .execute(
            "INSERT INTO mono_ask_events (ask_id, kind, seq, message_id, at_active_ms, at_wall_ms)
             VALUES (?1, 'answer', 1, ?2, 0, 0)",
            [&ask, &word],
        )
        .unwrap();
    scene.world.advance(WAKE + PASS);
    let said = replied(&scene);
    assert!(!said.starts_with("yes ("), "event by the asker: {said}");
}

// Defect: `created_at >= delivered` also gates a linked row, and
// delivery is stamped when the send resolves. A reply in the turn, by a call
// not bound to the opener's turn id, predates the stamp and reads as "no".
#[test]
fn linked_reply_before_the_delivery_stamp_is_not_no() {
    let mut scene = lead("int-before-stamp");
    let ask = scene.ask("asked", "Asked work");
    let lead_id = scene.lead.clone();
    align(&mut scene.world);
    scene.world.start_turn(&lead_id, "t7");
    turn(&mut scene.world, &lead_id, "t7", true);
    // `session_caller` is bound to turn "t1", so it does not confirm "t7".
    scene.lead_says("reply");
    let reply = scene.message_id("reply");
    link_reply(&scene, &reply, &ask);
    // Addressed to a chat the sender has since left: only the link says it.
    scene
        .world
        .conn()
        .execute(
            "UPDATE mono_messages SET to_mono_id = NULL, to_session_id = 'an-older-chat'
              WHERE id = ?1",
            [&reply],
        )
        .unwrap();
    scene.world.advance(MINUTE);
    scene.world.end_turn(&lead_id, "t7", true);
    turn(&mut scene.world, &lead_id, "t7", false);
    scene.world.advance(N);
    let said = replied(&scene);
    assert_eq!(said, format!("yes ({ask}, by {reply})."));
}

// Defect: the stale-row test is `delivered > updated_wall`. A row
// last written in the same millisecond as the delivery has not seen the turn
// either, and reads as a false "no".
#[test]
fn turn_row_of_the_same_millisecond_is_not_no() {
    let mut scene = lead("int-same-ms-row");
    let ask = scene.ask("asked", "Asked work");
    scene.takes_a_turn("t1");
    scene.world.advance(N);
    let episode = scene.episode();
    let active = scene.world.active_ms();
    let conn = scene.world.conn();
    conn.execute(
        "UPDATE agent_activity SET turn_id = 'older',
           updated_wall_ms = (SELECT delivered_at FROM mono_messages WHERE id = ?1)
         WHERE session_id = ?2",
        [ask.as_str(), scene.lead.as_str()],
    )
    .unwrap();
    let stuck = test_taps::stuck(&conn, &episode, active);
    assert_eq!(
        stuck.replied,
        crate::alarms::test_taps::Replied::Unknown(format!(
            "{ask}, the turn that took it is not recorded yet"
        ))
    );
}

// Defect: every opener of a turn is confirmed at one `now`, so
// two messages tie on `delivered_at` and the id picks "the last". The agent
// answered one and the line says "yes", though the rows cannot say which of
// the two came last and the other has no reply.
#[test]
fn two_messages_delivered_in_one_millisecond_is_not_yes() {
    let mut scene = lead("int-same-ms-pair");
    let first = scene.ask("a1", "Work one");
    let second = scene.ask("a2", "Work two");
    scene.takes_a_turn("t1");
    let delivered: i64 = scene
        .world
        .conn()
        .query_row(
            "SELECT COUNT(DISTINCT delivered_at) FROM mono_messages WHERE id IN (?1, ?2)",
            [&first, &second],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(delivered, 1, "the two tie on delivery");
    // The tie goes to the larger id, so that is the one answered.
    let answered = std::cmp::max(&first, &second);
    scene.lead_says("reply");
    let reply = scene.message_id("reply");
    link_reply(&scene, &reply, answered);
    scene.world.advance(N);
    let said = replied(&scene);
    assert!(!said.starts_with("yes ("), "could not tell: {said}");
}

// Defect: a tool's `status` reaches the Last activity line with
// no `short` (route.rs `last_tool`), and `alarm` does not fold that line, so
// it reads as a line of its own.
#[test]
fn tool_status_cannot_read_as_a_line() {
    let mut scene = lead("int-status");
    align(&mut scene.world);
    ran_a_turn(&mut scene.world, &scene.lead.clone());
    let blocks = json!([{"tool": {"title": "Bash", "status": HOSTILE}}]);
    scene
        .world
        .conn()
        .execute(
            "UPDATE sessions SET blocks_json = ?1 WHERE id = ?2",
            [blocks.to_string().as_str(), scene.lead.as_str()],
        )
        .unwrap();
    scene.world.advance(N);
    let text = scene.alarm();
    let acts = text.lines().filter(|l| l.starts_with("Act:")).count();
    assert_eq!(acts, 1, "{text}");
}

// Defect (the one rule): `minutes_since` subtracts a row's time with no check,
// and nothing between it and the pass catches a panic. In a release build,
// which wraps, the same row reads as a false "0 min old" (requirement 3).
#[test]
fn an_extreme_row_time_neither_panics_nor_reads_zero() {
    let scene = lead("int-overflow");
    let start: String = scene
        .world
        .conn()
        .query_row("SELECT id FROM mono_asks", [], |row| row.get(0))
        .unwrap();
    let actor = format!("session:{}", scene.lead.as_str());
    let key = format!("reminder:{start}:t5");
    reminder(&scene.world, &key, &actor, Some(&start));
    insert_episode(&scene.world, "al_min", &actor, "wake_missed", None);
    let active = scene.world.active_ms();
    let conn = scene.world.conn();
    conn.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    conn.execute("UPDATE mono_outbox SET at_active_ms = ?1", [i64::MIN])
        .unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
        .unwrap();
    conn.execute(
        "UPDATE alarm_episodes SET open_work_json = '[{\"kind\":\"reminder\"}]' WHERE id = 'al_min'",
        [],
    )
    .unwrap();
    let stuck = test_taps::stuck(&conn, "al_min", active);
    assert!(!stuck.item.ends_with(", 0 min old"), "{}", stuck.item);
}

// Defect (the one rule): `route::stall` still returns errors from what it reads
// beside `stuck::read`. An odd wall time in `last_activity` fails `level_text`,
// so no episode opens and nothing escalates.
#[test]
fn an_odd_last_progress_time_still_opens_and_escalates() {
    let mut scene = lead("int-odd-progress");
    align(&mut scene.world);
    ran_a_turn(&mut scene.world, &scene.lead.clone());
    scene
        .world
        .conn()
        .execute(
            "UPDATE agent_activity SET last_progress_wall_ms = 'not a time' WHERE session_id = ?1",
            [scene.lead.as_str()],
        )
        .unwrap();
    scene.world.advance(N);
    let text = scene.alarm();
    assert!(text.contains("No act in 15 min goes to"), "{text}");
    scene.world.advance(15 * MINUTE);
    let levels: i64 = scene
        .world
        .conn()
        .query_row("SELECT MAX(level) FROM alarm_sends", [], |row| row.get(0))
        .unwrap();
    assert_eq!(levels, 2, "the escalation clock still runs");
}

// Defect: with a list it cannot read, or only a kind from a newer
// build, an idle alarm says "open work that no row holds now" while the ask's
// row is open. It could not tell, so it must not say no row holds it.
#[test]
fn an_unread_list_does_not_say_no_row_holds_the_work() {
    let mut scene = lead("int-gone-claim");
    align(&mut scene.world);
    ran_a_turn(&mut scene.world, &scene.lead.clone());
    scene.world.advance(N);
    let episode = scene.episode();
    let active = scene.world.active_ms();
    let conn = scene.world.conn();
    for json in ["not a list", r#"[{"kind":"from_a_newer_build"}]"#] {
        conn.execute(
            "UPDATE alarm_episodes SET open_work_json = ?1 WHERE id = ?2",
            [json, episode.as_str()],
        )
        .unwrap();
        let stuck = test_taps::stuck(&conn, &episode, active);
        assert!(
            !stuck.item.contains("no row holds"),
            "{json}: {}",
            stuck.item
        );
    }
}
