//! S2 (ADE-55): the approvals board, one scenario per group of ADE-14 done
//! items. Callers go through the gate the web app runs first
//! (`check_action`, as `mono_app_action_check`), then the store function its
//! `handleApprovals` reaches, as the calling Giga. The user decides through
//! `decide`, the function `approvals_decide` calls. Delivery is read the way
//! the window takes it: `start_turn` and `end_turn`.

use serde_json::{json, Value};

use super::agents::Mono;
use super::world::World;
use crate::approvals::decide::decide;
use crate::approvals::{self, Approval, Card, Status, Verdict};
use crate::mono_messages::{self, ActionChecked, CallerArg, RequestId, SessionId, Targeted};
use crate::mono_roster::{self, MonoRole};

/// `from_session_id` of every decision row.
const BOARD: &str = "approvals-board";

/// A Mono's chat as the web app sends `caller`.
fn chat(mono: &Mono) -> Value {
    json!({"kind": "mono_chat", "monoId": mono.id.as_str(), "sessionId": mono.chat.as_str(), "turnId": "t1"})
}

/// A Giga's habit run as the web app sends `caller`.
fn habit(mono: &Mono) -> Value {
    json!({"kind": "habit_run", "monoId": mono.id.as_str(), "sessionId": mono.chat.as_str(), "turnId": "t1"})
}

/// A started session (a lead or a worker) as the web app sends `caller`.
fn session(id: &SessionId) -> Value {
    json!({"kind": "session", "sessionId": id.as_str(), "turnId": "t1", "access": "started", "cwd": "/p"})
}

/// A card as `file_approval` takes it, `project` included.
fn card(project: &str, heading: &str, summary: &str, options: &[&str]) -> Value {
    json!({
        "project": project,
        "heading": heading,
        "summary": summary,
        "tradeoffs": {"pros": ["Unblocks the Mono"], "cons": ["Costs a day"]},
        "options": options.iter().map(|label| json!({"label": label, "description": null, "recommended": false})).collect::<Vec<_>>(),
        "urgency": "blocking",
        "links": [{"kind": "pr", "title": null, "url": "https://github.com/tylers2222/Monocode-Unlocked/pull/1"}],
        "origin": "s2-mono",
    })
}

fn parse_card(input: &Value) -> Card {
    let mut fields = input.as_object().unwrap().clone();
    fields.remove("project");
    fields.remove("id");
    serde_json::from_value(Value::Object(fields)).unwrap()
}

impl World {
    /// An `approvals.*` call: the gate, then, when it lets the call through,
    /// the store write as the caller's Giga. `Err` is the code the agent gets.
    fn approvals(
        &self,
        caller: Value,
        request_id: &str,
        action: &str,
        input: Value,
    ) -> Result<Approval, String> {
        let caller = serde_json::from_value::<CallerArg>(caller)
            .unwrap()
            .parse()
            .unwrap();
        let checked = mono_messages::check_action(
            &self.conn(),
            &caller,
            action,
            None,
            None,
            &RequestId::parse(request_id).unwrap(),
            &input,
            self.now(),
        )
        .unwrap();
        if let ActionChecked::Failed { error } = checked {
            return Err(error.code.to_string());
        }
        let by = caller.mono_id().expect("the gate lets only a Giga through");
        let id = || input["id"].as_str().unwrap();
        let conn = self.conn();
        match action {
            "approvals.file" => approvals::file(
                &conn,
                input["project"].as_str().unwrap(),
                by,
                parse_card(&input),
                self.now(),
            ),
            "approvals.update" => approvals::update(&conn, id(), by, parse_card(&input)),
            "approvals.withdraw" => approvals::withdraw(&conn, id(), by, self.now()),
            other => panic!("not an approvals action: {other}"),
        }
    }

    /// The user decides card `id` on the board.
    fn decide(
        &self,
        id: &str,
        verdict: Verdict,
        note: &str,
        chosen: &[&str],
    ) -> Result<Approval, String> {
        let chosen: Vec<String> = chosen.iter().map(ToString::to_string).collect();
        decide(
            &self.conn(),
            id,
            verdict,
            Some(note.to_string()),
            &chosen,
            self.now(),
        )
        .map(|(decided, _)| decided)
    }

    fn card(&self, id: &str) -> Approval {
        approvals::get(&self.conn(), id).unwrap()
    }

    fn count(&self, sql: &str, params: &[&str]) -> i64 {
        self.conn()
            .query_row(sql, rusqlite::params_from_iter(params), |row| row.get(0))
            .unwrap()
    }

    /// The badge reads `open_counts`: its total is every open card in the
    /// database, and `project` has `open` of them.
    fn badge_matches(&self, project: &str, open: u32, at: &str) {
        let counts = approvals::open_counts(&self.conn()).unwrap();
        let stored: i64 = self
            .conn()
            .query_row(
                "SELECT count(*) FROM approvals WHERE status = 'open'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(i64::from(counts.total), stored, "{at}: badge total");
        assert_eq!(
            counts.by_project.get(project).copied().unwrap_or(0),
            open,
            "{at}: open cards of {project}"
        );
    }
}

fn decided(card: &Approval) -> (Verdict, Option<&str>, Vec<&str>) {
    match &card.status {
        Status::Decided(decision) => (
            decision.verdict,
            decision.note.as_deref(),
            decision.chosen.iter().map(String::as_str).collect(),
        ),
        other => panic!("{} is not decided: {other:?}", card.card.heading),
    }
}

fn bodies(rows: &[Targeted]) -> Vec<&str> {
    rows.iter().map(|row| row.message.body.text()).collect()
}

/// The decision text the filing Giga reads.
fn said(card: &Approval, verdict: &str, chosen: &str, note: &str) -> String {
    format!(
        "The user decided approval card APR-{} \"{}\".\ncard_id: {}\ndecision: {verdict}\nchosen_options: {chosen}\ncomment: {note}",
        card.number, card.card.heading, card.id
    )
}

/// One decision row as stored: who it is for, which chat holds it, how it
/// is delivered, its status and the turn it went into.
#[derive(Debug, PartialEq, Eq)]
struct Sent {
    to: String,
    chat: String,
    delivery: String,
    status: String,
    turn: Option<String>,
}

fn sent(world: &World, card: &Approval) -> Vec<Sent> {
    world
        .conn()
        .prepare(
            "SELECT to_mono_id, handled_by_session, delivery, status, turn_id FROM mono_messages
             WHERE from_session_id = ?1 AND request_id = ?2",
        )
        .unwrap()
        .query_map([BOARD, card.id.as_str()], |row| {
            Ok(Sent {
                to: row.get(0)?,
                chat: row.get(1)?,
                delivery: row.get(2)?,
                status: row.get(3)?,
                turn: row.get(4)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn queued_for(giga: &Mono) -> Sent {
    Sent {
        to: giga.id.as_str().into(),
        chat: giga.chat.as_str().into(),
        delivery: "queue".into(),
        status: "delivered".into(),
        turn: None,
    }
}

/// The cards the decide scenario files: three of A's, B's with options.
struct Four {
    ship: Approval,
    cache: Approval,
    rename: Approval,
    database: Approval,
}

fn file_four(world: &World, project: &str, a: &Mono, b: &Mono) -> Four {
    let file = |giga: &Mono, n: u32, heading: &str, options: &[&str]| {
        world
            .approvals(
                chat(giga),
                &format!("{}-file-{n}", giga.id.as_str()),
                "approvals.file",
                card(project, heading, "Needs the user", options),
            )
            .unwrap()
    };
    Four {
        ship: file(a, 1, "Ship the importer", &[]),
        cache: file(a, 2, "Drop the cache", &[]),
        rename: file(a, 3, "Rename the CLI", &[]),
        database: file(b, 1, "Pick a database", &["SQLite", "Postgres", "Server"]),
    }
}

/// The user decides each card, a fake second apart, and fails twice to
/// approve B's card without a valid tick first.
fn decide_four(world: &mut World, four: &Four) {
    let mut decide = |card: &Approval, verdict: Verdict, note: &str, chosen: &[&str]| {
        world.advance(1_000);
        world.decide(&card.id, verdict, note, chosen).unwrap()
    };
    let approve = decide(&four.ship, Verdict::Approved, "Ship it Friday", &[]);
    let reject = decide(&four.cache, Verdict::Rejected, "Keep the cache", &[]);
    let changes = decide(&four.rename, Verdict::ChangesRequested, "Name it mc", &[]);
    assert_eq!(
        world.decide(&four.database.id, Verdict::Approved, "Need a pick", &[]),
        Err("invalid_input: approving a card with options needs at least one chosen option".into()),
        "approving a card with options and no tick is refused"
    );
    assert_eq!(
        world.decide(
            &four.database.id,
            Verdict::Approved,
            "Not an option",
            &["MySQL"]
        ),
        Err("invalid_input: chosen options must be distinct labels of the card's options".into()),
        "a tick that is not one of the card's options is refused"
    );
    let pick = world
        .decide(
            &four.database.id,
            Verdict::Approved,
            "Both, SQLite first",
            &["SQLite", "Server"],
        )
        .unwrap();
    assert_eq!(
        decided(&approve),
        (Verdict::Approved, Some("Ship it Friday"), vec![])
    );
    assert_eq!(
        decided(&reject),
        (Verdict::Rejected, Some("Keep the cache"), vec![])
    );
    assert_eq!(
        decided(&changes),
        (Verdict::ChangesRequested, Some("Name it mc"), vec![])
    );
    assert_eq!(
        decided(&pick),
        (
            Verdict::Approved,
            Some("Both, SQLite first"),
            vec!["SQLite", "Server"]
        )
    );
    let Status::Decided(decision) = &pick.status else {
        unreachable!()
    };
    assert_eq!(
        decision.decided_at,
        world.now(),
        "decided at the fake clock's time"
    );
}

/// Each decision row waits in its filer's chat, queued; nobody else has one.
fn each_decision_waits_for_its_filer(
    world: &World,
    four: &Four,
    a: &Mono,
    b: &Mono,
    others: &[&Mono],
) {
    for card in [&four.ship, &four.cache, &four.rename] {
        assert_eq!(
            sent(world, card),
            vec![queued_for(a)],
            "{} queued to A only",
            card.card.heading
        );
    }
    assert_eq!(
        sent(world, &four.database),
        vec![queued_for(b)],
        "the pick queued to B only"
    );
    for other in others {
        assert_eq!(
            world.count(
                "SELECT count(*) FROM mono_messages WHERE to_mono_id = ?1 OR handled_by_session = ?2",
                &[other.id.as_str(), other.chat.as_str()]
            ),
            0,
            "{} gets nothing",
            other.id.as_str()
        );
    }
}

/// A's running turn ends and its queued decisions open its next turn; B
/// takes its pick; nobody takes anything twice or anything that is not theirs.
fn each_giga_takes_its_decisions_once(
    world: &mut World,
    four: &Four,
    a: &Mono,
    b: &Mono,
    others: &[&Mono],
) {
    world.end_turn(&a.chat, "a-t1", true);
    let to_a = world.start_turn(&a.chat, "a-t2");
    assert_eq!(
        bodies(&to_a),
        vec![
            said(&four.ship, "approved", "[]", "Ship it Friday").as_str(),
            said(&four.cache, "rejected", "[]", "Keep the cache").as_str(),
            said(&four.rename, "changes_requested", "[]", "Name it mc").as_str(),
        ],
        "A's next turn carries its three decisions with their comments, in order"
    );
    for card in [&four.ship, &four.cache, &four.rename] {
        assert_eq!(
            sent(world, card)[0].turn.as_deref(),
            Some("a-t2"),
            "into A's next turn, not a-t1"
        );
    }
    let to_b = world.start_turn(&b.chat, "b-t1");
    assert_eq!(
        bodies(&to_b),
        vec![said(
            &four.database,
            "approved",
            "[\"SQLite\",\"Server\"]",
            "Both, SQLite first"
        )
        .as_str()],
        "B gets the ticked options"
    );
    for other in others {
        assert!(
            world.start_turn(&other.chat, "o-t1").is_empty(),
            "{} has nothing to take",
            other.id.as_str()
        );
    }
    world.end_turn(&a.chat, "a-t2", true);
    world.end_turn(&b.chat, "b-t1", true);
    assert!(
        world.start_turn(&a.chat, "a-t3").is_empty(),
        "A gets each decision once"
    );
    assert!(
        world.start_turn(&b.chat, "b-t2").is_empty(),
        "B gets its decision once"
    );
}

/// Defects: a decision reaches another Giga or a Mono, or no Giga; it is
/// steered into the Giga's running turn instead of queued; it arrives
/// twice; its text leaves out the verdict, the comment or a ticked option;
/// or a decided card can be decided again.
#[test]
fn s2_each_decision_reaches_only_the_giga_that_filed_it_queued() {
    let mut world = World::new("s2-deliver");
    let project = "/scenario/s2/deliver";
    let a = world.add_mono("s2-giga-a", MonoRole::Giga);
    let b = world.add_mono("s2-giga-b", MonoRole::Giga);
    let c = world.add_mono("s2-giga-c", MonoRole::Giga);
    let mono = world.add_mono("s2-mono", MonoRole::Mono);
    let four = file_four(&world, project, &a, &b);
    world.badge_matches(project, 4, "four cards filed");
    assert!(
        world.start_turn(&a.chat, "a-t1").is_empty(),
        "A is in a turn with nothing waiting"
    );
    decide_four(&mut world, &four);
    world.badge_matches(project, 0, "every card decided");
    each_decision_waits_for_its_filer(&world, &four, &a, &b, &[&c, &mono]);
    each_giga_takes_its_decisions_once(&mut world, &four, &a, &b, &[&c, &mono]);
    assert_eq!(
        world.decide(&four.ship.id, Verdict::Rejected, "Changed my mind", &[]),
        Err("approval_not_open".into()),
        "a decided card is not decided again"
    );
    assert_eq!(
        world.count(
            "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1",
            &[BOARD]
        ),
        4,
        "one decision row per decided card"
    );
}

/// A Mono, a lead and a worker each call every approvals action: each call
/// is refused and on record, and no card is written.
fn only_gigas_pass_the_gate(world: &World, project: &str, host: &Value) {
    let mono = world.add_mono("s2-mono", MonoRole::Mono);
    let lead = world.start_lead(&mono, "s2-lead", "/p");
    let worker = world.add_session("s2-worker", "/p", Some("s2-lead"));
    for (who, caller, from) in [
        ("Mono", chat(&mono), &mono.chat),
        ("lead", session(&lead), &lead),
        ("worker", session(&worker), &worker),
    ] {
        for (n, action) in ["approvals.file", "approvals.update", "approvals.withdraw"]
            .iter()
            .enumerate()
        {
            let mut input = host.clone();
            input["id"] = json!("00000000-0000-4000-8000-000000000000");
            assert_eq!(
                world.approvals(caller.clone(), &format!("{who}-{n}"), action, input),
                Err("not_a_giga".into()),
                "a {who} calling {action} is refused"
            );
        }
        assert_eq!(
            world.count(
                "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1
                   AND status = 'rejected' AND gate_result = 'not_a_giga'",
                &[from.as_str()]
            ),
            3,
            "each refusal of the {who} is on record"
        );
    }
    assert_eq!(
        world.count(
            "SELECT count(*) FROM approvals WHERE project_path = ?1",
            &[project]
        ),
        0,
        "no refused call wrote a card"
    );
}

/// A files a card, then files the same heading again from a habit run.
fn filing_again_updates_the_open_card(
    world: &World,
    project: &str,
    a: &Mono,
    host: &Value,
) -> Approval {
    let filed = world
        .approvals(chat(a), "a-1", "approvals.file", host.clone())
        .unwrap();
    assert_eq!(
        filed.requested_by.id.as_ref(),
        Some(&a.id),
        "filed as the calling Giga"
    );
    assert_eq!(filed.status, Status::Open);
    assert_eq!(
        filed.created_at,
        world.now(),
        "filed at the fake clock's time"
    );
    world.badge_matches(project, 1, "one card filed");
    let mut again = host.clone();
    again["summary"] = json!("Fly, Railway or Render");
    again["options"] = json!([{"label": "Fly"}, {"label": "Railway"}, {"label": "Render"}]);
    let refiled = world
        .approvals(habit(a), "a-2", "approvals.file", again)
        .unwrap();
    assert_eq!(
        (refiled.id.as_str(), refiled.number),
        (filed.id.as_str(), filed.number),
        "the same heading updates the open card"
    );
    assert_eq!(refiled.card.summary, "Fly, Railway or Render");
    assert_eq!(refiled.card.options.len(), 3);
    world.badge_matches(project, 1, "filing again made no second card");
    refiled
}

/// B cannot file over, update or withdraw A's open card.
fn another_giga_cannot_change_it(world: &World, b: &Mono, host: &Value, filed: &Approval) {
    let mut taken = host.clone();
    taken["summary"] = json!("B wants this heading");
    assert_eq!(
        world.approvals(chat(b), "b-1", "approvals.file", taken),
        Err("heading_taken".into()),
        "another Giga cannot file over A's open card"
    );
    let mut update = host.clone();
    update["id"] = json!(filed.id);
    update["summary"] = json!("B rewrites A's card");
    assert_eq!(
        world.approvals(chat(b), "b-2", "approvals.update", update),
        Err("not_your_approval".into()),
        "another Giga cannot update it"
    );
    assert_eq!(
        world.approvals(
            chat(b),
            "b-3",
            "approvals.withdraw",
            json!({"id": filed.id})
        ),
        Err("not_your_approval".into()),
        "another Giga cannot withdraw it"
    );
    assert_eq!(
        &world.card(&filed.id),
        filed,
        "B's calls left A's card as it was"
    );
}

/// A updates its card, withdraws it, and then can change it no more.
fn the_filer_updates_and_withdraws_it(
    world: &World,
    project: &str,
    a: &Mono,
    host: &Value,
    filed: &Approval,
) {
    let mut update = host.clone();
    update["id"] = json!(filed.id);
    update["heading"] = json!("Choose a host for the API");
    update["urgency"] = json!("not_blocking");
    let updated = world
        .approvals(chat(a), "a-3", "approvals.update", update.clone())
        .unwrap();
    assert_eq!(updated.id, filed.id);
    assert_eq!(
        updated.card.heading, "Choose a host for the API",
        "the filer updates its card"
    );
    assert_eq!(updated.card.urgency, approvals::Urgency::NotBlocking);
    let withdrawn = world
        .approvals(
            chat(a),
            "a-4",
            "approvals.withdraw",
            json!({"id": filed.id}),
        )
        .unwrap();
    assert_eq!(
        withdrawn.status,
        Status::Withdrawn {
            withdrawn_at: world.now()
        },
        "the filer withdraws its card"
    );
    world.badge_matches(project, 0, "the withdrawn card is not open");
    assert_eq!(
        world.approvals(
            chat(a),
            "a-5",
            "approvals.withdraw",
            json!({"id": filed.id})
        ),
        Err("approval_not_open".into()),
        "a withdrawn card is not withdrawn again"
    );
    update["summary"] = json!("Too late");
    assert_eq!(
        world.approvals(chat(a), "a-6", "approvals.update", update),
        Err("approval_not_open".into()),
        "a withdrawn card is not updated"
    );
    assert_eq!(
        world.decide(&filed.id, Verdict::Approved, "Fly", &["Fly"]),
        Err("approval_not_open".into()),
        "a withdrawn card is not decided"
    );
}

/// Defects: a Mono, a lead or a worker files, changes or withdraws a card,
/// or its refusal writes a card; filing the same heading again makes a
/// second card; another Giga takes, changes or withdraws a Giga's card; or
/// a closed card can be changed.
#[test]
fn s2_only_a_giga_files_and_only_its_filer_changes_a_card() {
    let world = World::new("s2-file");
    let project = "/scenario/s2/file";
    let a = world.add_mono("s2-giga-a", MonoRole::Giga);
    let b = world.add_mono("s2-giga-b", MonoRole::Giga);
    let host = card(
        project,
        "Choose a host",
        "Fly or Railway",
        &["Fly", "Railway"],
    );
    only_gigas_pass_the_gate(&world, project, &host);
    let filed = filing_again_updates_the_open_card(&world, project, &a, &host);
    another_giga_cannot_change_it(&world, &b, &host, &filed);
    the_filer_updates_and_withdraws_it(&world, project, &a, &host, &filed);
    let mut fresh = host;
    fresh["heading"] = json!("Choose a host for the API");
    let next = world
        .approvals(chat(&b), "b-4", "approvals.file", fresh)
        .unwrap();
    assert_ne!(
        next.id, filed.id,
        "the heading is free once the card is closed"
    );
    assert!(next.number > filed.number);
    assert_eq!(next.requested_by.id.as_ref(), Some(&b.id));
    world.badge_matches(project, 1, "B's new card is open");
}

/// A's three cards (one approved, one withdrawn) and B's, across a kill:
/// every card survives and the queued decision reaches A once.
fn cards_and_a_queued_decision_survive_a_kill(
    world: &mut World,
    project: &str,
    a: &Mono,
    b: &Mono,
) -> Approval {
    let file = |giga: &Mono, n: u32, heading: &str| {
        world
            .approvals(
                chat(giga),
                &format!("{}-{n}", giga.id.as_str()),
                "approvals.file",
                card(project, heading, "Needs the user", &["Yes", "No"]),
            )
            .unwrap()
    };
    file(a, 1, "Keep open");
    let approved = file(a, 2, "Approve before the kill");
    let withdrawn = file(a, 3, "Withdraw before the kill");
    let orphan = file(b, 1, "Filed by a removed Giga");
    world.advance(5_000);
    world
        .decide(&approved.id, Verdict::Approved, "Go", &["Yes"])
        .unwrap();
    world
        .approvals(
            chat(a),
            "s2-giga-a-4",
            "approvals.withdraw",
            json!({"id": withdrawn.id}),
        )
        .unwrap();
    world.badge_matches(project, 2, "two open before the kill");
    let before = approvals::list(&world.conn(), project).unwrap();
    let counts = approvals::open_counts(&world.conn()).unwrap();
    world.down(10 * 60_000);
    assert_eq!(
        approvals::list(&world.conn(), project).unwrap(),
        before,
        "every card and status survives the kill"
    );
    assert_eq!(
        approvals::open_counts(&world.conn()).unwrap(),
        counts,
        "the badge reads the same after the restart"
    );
    world.badge_matches(project, 2, "after the restart");
    assert_eq!(
        sent(world, &approved),
        vec![queued_for(a)],
        "the decision still waits for A"
    );
    let to_a = world.start_turn(&a.chat, "a-t1");
    assert_eq!(
        bodies(&to_a),
        vec![said(&approved, "approved", "[\"Yes\"]", "Go").as_str()],
        "A takes the decision after the restart"
    );
    world.end_turn(&a.chat, "a-t1", true);
    world.down(60_000);
    assert!(
        world.start_turn(&a.chat, "a-t2").is_empty(),
        "a second restart does not send it again"
    );
    orphan
}

/// Defects: a kill loses a card, a status, a decision note or a queued
/// decision, or delivers it twice; the badge counts other than the open
/// cards; deleting the filing Giga deletes its card or forgets who filed
/// it; or deciding that card sends a message to anyone, even to a new Giga
/// that takes the removed Giga's name.
#[test]
fn s2_cards_survive_a_restart_and_a_removed_giga_gets_nothing() {
    let mut world = World::new("s2-restart");
    let project = "/scenario/s2/restart";
    let a = world.add_mono("s2-giga-a", MonoRole::Giga);
    let b = world.add_mono("s2-giga-b", MonoRole::Giga);
    let orphan = cards_and_a_queued_decision_survive_a_kill(&mut world, project, &a, &b);

    mono_roster::delete(&world.conn(), b.id.as_str()).unwrap();
    let removed = world.card(&orphan.id);
    assert_eq!(
        removed.requested_by.id, None,
        "the card no longer names a Giga"
    );
    assert_eq!(
        removed.requested_by.name, "s2-giga-b",
        "the card keeps the name it was filed under"
    );
    assert_eq!(removed.status, Status::Open, "the card stays open");
    world.badge_matches(project, 2, "the removed Giga's card still counts");
    let namesake = world.add_mono("s2-giga-b2", MonoRole::Giga);
    let rename: mono_roster::MonoUpdate =
        serde_json::from_value(json!({"id": "s2-giga-b2", "patch": {"name": "s2-giga-b"}}))
            .unwrap();
    mono_roster::update_many(
        &world.conn(),
        &[mono_roster::validate_update(rename).unwrap()],
    )
    .unwrap();
    assert_eq!(
        world.count("SELECT count(*) FROM monos WHERE name = ?1", &["s2-giga-b"]),
        1,
        "another Giga now goes by the removed Giga's name"
    );
    let board_rows = world.count(
        "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1",
        &[BOARD],
    );
    world.advance(1_000);
    let closed = world
        .decide(&orphan.id, Verdict::Rejected, "Handled by hand", &[])
        .unwrap();
    assert_eq!(
        decided(&closed),
        (Verdict::Rejected, Some("Handled by hand"), vec![])
    );
    assert!(
        sent(&world, &orphan).is_empty(),
        "deciding it sends nothing"
    );
    assert_eq!(
        world.count(
            "SELECT count(*) FROM mono_messages WHERE from_session_id = ?1",
            &[BOARD]
        ),
        board_rows,
        "no decision row was written"
    );
    assert!(
        world.start_turn(&b.chat, "b-t1").is_empty(),
        "the removed Giga's chat gets nothing"
    );
    world.down(60_000);
    assert!(
        world.start_turn(&namesake.chat, "b2-t1").is_empty(),
        "a new Giga with the removed Giga's name gets nothing"
    );
    world.badge_matches(project, 1, "only the kept card is open");

    let after = approvals::list(&world.conn(), project).unwrap();
    world.down(60_000);
    assert_eq!(
        approvals::list(&world.conn(), project).unwrap(),
        after,
        "the removed Giga's decided card survives a restart"
    );
    world.badge_matches(project, 1, "after the last restart");
}

/// Names the instance database that `s2_user_acts_on_an_instance_database`
/// acts on; `scripts/scenarios/app.sh approvals` sets it with the app
/// stopped.
const USER_DB: &str = "MONOCODE_SCENARIO_USER_DB";

/// The user's actions on a real-app instance, through the functions the
/// board's and the roster's commands call: `MONOCODE_SCENARIO_USER_STEPS`
/// holds `{"decide": heading, "verdict", "note", "chosen"}` and
/// `{"remove_mono": id}` steps, made at `MONOCODE_SCENARIO_USER_NOW`. One
/// JSON line per step on stdout. The app has no other way in for the user
/// but the window.
#[test]
#[ignore = "run by scripts/scenarios/app.sh approvals"]
fn s2_user_acts_on_an_instance_database() {
    let path = std::fs::canonicalize(std::env::var(USER_DB).unwrap()).unwrap();
    assert!(
        path.starts_with("/tmp") || path.starts_with("/private/tmp"),
        "{USER_DB} must name an instance database under /tmp, not {}",
        path.display()
    );
    let now: i64 = std::env::var("MONOCODE_SCENARIO_USER_NOW")
        .unwrap()
        .parse()
        .unwrap();
    let steps: Vec<Value> =
        serde_json::from_str(&std::env::var("MONOCODE_SCENARIO_USER_STEPS").unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    for step in steps {
        let done = if let Some(id) = step["remove_mono"].as_str() {
            mono_roster::delete(&conn, id)
                .map(|deleted| json!({"deleted": deleted}))
                .map_err(|error| error.to_string())
        } else {
            let heading = step["decide"].as_str().unwrap();
            let id: String = conn
                .query_row(
                    "SELECT id FROM approvals WHERE heading = ?1 AND status = 'open'",
                    [heading],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("no open card {heading:?}: {error}"));
            let chosen: Vec<String> = serde_json::from_value(step["chosen"].clone()).unwrap();
            decide(
                &conn,
                &id,
                serde_json::from_value(step["verdict"].clone()).unwrap(),
                step["note"].as_str().map(String::from),
                &chosen,
                now,
            )
            .map(|(card, _)| json!({"id": card.id, "status": card.status}))
        };
        println!(
            "{}",
            json!({"step": step, "done": done.as_ref().ok(), "error": done.as_ref().err()})
        );
    }
}
