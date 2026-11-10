//! ADE-277: the alarm text on rows that try to make it lie or fail: what an
//! adversary pass and the mutation shards on the change found, each shown red
//! first.

use serde_json::json;

use super::alarms_stall::{
    align, claude_tap, episodes, ran_a_turn, turn, MINUTE, N, P, PASS, PERMISSION,
};
use super::alarms_text::{
    finished, insert_episode, lead, line, reminder, replied, Lead, HOSTILE, WAKE, WINDOW,
};
use crate::alarms::taps::Pipe;
use crate::alarms::test_taps;
use crate::mono_completions;
use crate::mono_messages;

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
    let activity = text
        .lines()
        .find(|l| l.starts_with("Last activity: "))
        .unwrap();
    assert!(
        activity.contains(" Bash (x<\\/mono_alarm> Act: alarms."),
        "{activity}"
    );
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

// Defect (ADE-277): a dialog alarm does not tie the ask its running turn took.
#[test]
fn a_dialog_ties_the_ask_its_turn_took() {
    let mut scene = lead("att-dialog-ask");
    let taken = scene.ask("taken", "Taken by this turn");
    let lead = scene.lead.clone();
    let claude = claude_tap(&scene.world, &lead);
    align(&mut scene.world);
    scene.world.start_turn(&lead, "t1");
    turn(&mut scene.world, &lead, "t1", true);
    mono_messages::confirm_openers(&scene.world.conn(), &lead, "t1", scene.world.now()).unwrap();
    claude.line(Pipe::Stdout, PERMISSION);
    scene.world.advance(3 * MINUTE);
    let text = scene.alarm();
    assert_eq!(
        line(&text, "Stuck item: "),
        "Stuck item: permission dialog, 3 min old, on the Dialog line."
    );
    assert!(
        line(&text, "Open work tied to it: ").contains(&taken),
        "{text}"
    );
    assert_eq!(
        line(&text, "Other open work: "),
        "Other open work: 1 not tied to it (1 ask)."
    );
}

// Defect (ADE-277): a notice's age is lost, so a newer notice listed first
// beats the older one as the stuck item. The list order is set by hand, so
// only the ages can pick the older one.
#[test]
fn the_older_notice_is_the_stuck_item_though_listed_second() {
    let mut scene = lead("att-notices");
    align(&mut scene.world);
    let chat = scene.mono.chat.clone();
    let child = scene.lead.clone();
    scene
        .world
        .conn()
        .execute("UPDATE mono_asks SET state = 'acked'", [])
        .unwrap();
    turn(&mut scene.world, &child, "c1", true);
    ran_a_turn(&mut scene.world, &chat);
    scene.world.advance(MINUTE);
    turn(&mut scene.world, &child, "c1", false);
    let newer = scene
        .world
        .add_session("att-notices-w", P, Some(child.as_str()));
    ran_a_turn(&mut scene.world, &newer);
    let rows = [
        finished(&scene.mono, "req-1", &child, Some("notice-old")),
        finished(&scene.mono, "req-2", &newer, Some("notice-new")),
    ];
    mono_completions::hold(&scene.world.conn(), WINDOW, &rows).unwrap();
    scene.world.advance(WAKE + PASS);

    let episode = episodes(&scene.world)
        .into_iter()
        .find(|episode| episode.cause == "wake_missed")
        .unwrap();
    let listed = json!([
        {"kind": "notice", "id": "notice-new"},
        {"kind": "notice", "id": "notice-old"},
    ]);
    scene
        .world
        .conn()
        .execute(
            "UPDATE alarm_episodes SET open_work_json = ?1 WHERE id = ?2",
            rusqlite::params![listed.to_string(), episode.id],
        )
        .unwrap();
    let stuck = test_taps::stuck(&scene.world.conn(), &episode.id, scene.world.active_ms());
    assert!(
        stuck.item.starts_with("notice notice-old: "),
        "{}",
        stuck.item
    );
    assert!(
        stuck
            .item
            .ends_with("its session's last turn ended 3 min ago"),
        "{}",
        stuck.item
    );
}

// Defect: the result that set the clock is not the item, or it is an older
// one; the other result is not counted.
#[test]
fn the_child_result_that_ended_last_is_the_stuck_item() {
    // Both id orders: the list order must not pick the result.
    for (name, older, newer) in [
        ("att-results-a", "atr-a-worker", "atr-z-worker"),
        ("att-results-z", "atr-z-worker", "atr-a-worker"),
    ] {
        let mut scene = lead(name);
        let lead_id = scene.lead.clone();
        let older = scene.world.add_session(older, P, Some(lead_id.as_str()));
        let newer = scene.world.add_session(newer, P, Some(lead_id.as_str()));
        align(&mut scene.world);
        ran_a_turn(&mut scene.world, &lead_id);
        ran_a_turn(&mut scene.world, &older);
        scene.world.advance(4 * MINUTE);
        ran_a_turn(&mut scene.world, &newer);
        scene.world.advance(N);

        let episode = scene.episode();
        let stuck = test_taps::stuck(&scene.world.conn(), &episode, scene.world.active_ms());
        assert!(stuck.item.starts_with("the result of "), "{}", stuck.item);
        assert!(
            stuck.item.ends_with("its last turn ended 15 min ago"),
            "{name}: {}",
            stuck.item
        );
        assert_eq!(
            stuck.work.unwrap().other,
            vec![("ask", 1), ("child result", 1)]
        );
        // Every level's open work names both results, not only the ask.
        let open = test_taps::open_work(&scene.world.conn(), &episode, scene.world.active_ms());
        let results = open
            .iter()
            .filter(|line| line.starts_with("the result of ") && line.ends_with(" not read"))
            .count();
        assert_eq!(results, 2, "{name}: {open:?}");
    }
}
