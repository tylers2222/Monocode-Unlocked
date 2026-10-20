use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::approvals::tests::{app_database_copy, closed, giga, insert, text};
use crate::approvals::{Filer, Status, UNKNOWN};

const CHAT_A: &str = "chat-a";
const CHAT_B: &str = "chat-b";
const PICK: &str = r#"[{"label":"SQLite"},{"label":"Server"}]"#;

fn giga_with_chat(conn: &Connection, id: &str, chat: &str) {
    giga(conn, id, &format!("Giga {id}"));
    conn.execute(
        "UPDATE monos SET data_json = json_object('mascot', 'skull', 'sessionId', ?2) WHERE id = ?1",
        [id, chat],
    )
    .unwrap();
}

fn filed(conn: &Connection, by: &str, heading: &str, options: &str) -> Approval {
    let id = Uuid::new_v4().to_string();
    insert(
        conn,
        &[
            ("id", text(&id)),
            ("heading", text(heading)),
            ("options", text(options)),
            ("requested_by", text(by)),
        ],
    )
    .unwrap();
    get(conn, &id).unwrap()
}

fn chosen(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|label| label.to_string()).collect()
}

fn queued(conn: &Connection, chat: &str) -> Vec<serde_json::Value> {
    mono_messages::unqueued(conn, &SessionId::parse(chat).unwrap())
        .unwrap()
        .into_iter()
        .map(|row| {
            let mut row = serde_json::to_value(row.message).unwrap();
            row.as_object_mut().unwrap().remove("id");
            row
        })
        .collect()
}

fn decision_row(card: &Approval, chat: &str, at: i64, body: &str) -> serde_json::Value {
    let giga = card.requested_by.id.as_ref().unwrap().as_str();
    json!({
        "requestId": card.id, "part": 0, "tool": "send_message",
        "fromMonoId": null, "fromSessionId": "approvals-board",
        "to": { "kind": "mono", "monoId": giga }, "addressed": giga,
        "body": body, "bodyChars": body.chars().count(), "hop": 0, "createdAt": at,
        "status": "delivered", "wantingReply": false, "intent": "new", "replyTo": null,
        "scope": null, "handledBySession": chat, "delivery": "queue", "slot": null
    })
}

fn said(card: &Approval, rest: &str) -> String {
    format!(
        "The user decided approval card APR-{} \"{}\".\ncard_id: {}\n{rest}",
        card.number, card.card.heading, card.id
    )
}

fn naming(chat: &str) -> Notices {
    let mut notices = Notices::default();
    notices.name(SessionId::parse(chat).unwrap());
    notices
}

fn messages(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM mono_messages", [], |row| row.get(0))
        .unwrap()
}

// Defects: a decision reaches the other Giga or no Giga, is steered or
// waits for a reply, leaves out the card id, APR number, verdict, a ticked
// option or the comment, names a chat no window then reconciles, or a
// retry queues it twice.
#[test]
fn each_decision_queues_once_to_the_chat_of_the_giga_that_filed_it() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    giga_with_chat(&conn, "giga-a", CHAT_A);
    giga_with_chat(&conn, "giga-b", CHAT_B);
    let store_card = filed(&conn, "giga-a", "Pick a store", PICK);
    let bump = filed(&conn, "giga-a", "Bump Playwright", "[]");
    let host = filed(&conn, "giga-b", "Pick a host", PICK);
    let ticked = chosen(&["Server", "SQLite"]);

    let (_, told) = decide(&conn, &store_card.id, Verdict::Approved, None, &ticked, 5).unwrap();
    assert_eq!(told, naming(CHAT_A));
    let note = Some("Not this week".to_string());
    let (_, told) = decide(&conn, &bump.id, Verdict::Rejected, note, &[], 6).unwrap();
    assert_eq!(told, naming(CHAT_A));
    let note = Some(" Add costs ".to_string());
    let (_, told) = decide(
        &conn,
        &host.id,
        Verdict::ChangesRequested,
        note,
        &chosen(&["Server"]),
        7,
    )
    .unwrap();
    assert_eq!(told, naming(CHAT_B));
    assert_eq!(
        decide(&conn, &store_card.id, Verdict::Approved, None, &ticked, 8),
        Err(NOT_OPEN.into()),
        "a retry decides nothing"
    );

    let approved = "decision: approved\nchosen_options: [\"Server\",\"SQLite\"]";
    let rejected = "decision: rejected\nchosen_options: []\ncomment: Not this week";
    assert_eq!(
        queued(&conn, CHAT_A),
        [
            decision_row(&store_card, CHAT_A, 5, &said(&store_card, approved)),
            decision_row(&bump, CHAT_A, 6, &said(&bump, rejected)),
        ]
    );
    let changes = "decision: changes_requested\nchosen_options: [\"Server\"]\ncomment: Add costs";
    assert_eq!(
        queued(&conn, CHAT_B),
        [decision_row(&host, CHAT_B, 7, &said(&host, changes))]
    );
    assert_eq!(messages(&conn), 3, "the retry queued nothing");
}

// Defects: a card whose Giga was deleted cannot close, loses its manual
// handling state or writes a message; or a decision that cannot reach
// the Giga (no chat, too long for one message) closes the card anyway.
#[test]
fn decide_delivers_nothing_to_a_deleted_giga_and_refuses_what_it_cannot_deliver() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    giga_with_chat(&conn, "giga-a", CHAT_A);
    giga(&conn, "giga-b", "Giga B");
    let long = filed(&conn, "giga-a", "Long", "[]");
    let chatless = filed(&conn, "giga-b", "No chat", "[]");
    let orphan = filed(&conn, "giga-a", "Orphan", "[]");

    let note = Some("x".repeat(TEXT_MAX));
    let error = decide(&conn, &long.id, Verdict::Rejected, note, &[], 2).unwrap_err();
    assert!(error.starts_with("invalid_input"), "{error}");
    assert_eq!(
        decide(&conn, &chatless.id, Verdict::Rejected, None, &[], 2),
        Err(NO_CHAT.into())
    );
    assert_eq!(get(&conn, &long.id).unwrap(), long);
    assert_eq!(get(&conn, &chatless.id).unwrap(), chatless);

    assert!(crate::mono_roster::delete(&conn, "giga-a").unwrap());
    let (closed, told) = decide(&conn, &orphan.id, Verdict::Approved, None, &[], 3).unwrap();
    assert_eq!(
        closed.requested_by,
        Filer {
            id: None,
            name: "Giga A".into()
        },
        "the card shows the manual handling note"
    );
    assert!(matches!(closed.status, Status::Decided(_)));
    assert_eq!(told, Notices::default());
    assert_eq!(messages(&conn), 0);
}

// Defects: a second decide overwrites the first, a withdrawn card can be
// decided, a chosen option is not one of the card's, or Approve on a
// card with options names none.
#[test]
fn decide_closes_an_open_card_once() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    giga_with_chat(&conn, "giga-a", CHAT_A);
    let id = filed(&conn, "giga-a", "Pick a store", PICK).id;
    for ticked in [vec!["Cloud"], vec!["SQLite", "SQLite"], vec![]] {
        let error = decide(&conn, &id, Verdict::Approved, None, &chosen(&ticked), 2).unwrap_err();
        assert!(error.starts_with("invalid_input"), "{ticked:?}: {error}");
    }
    assert_eq!(get(&conn, &id).unwrap().status, Status::Open);

    let ticked = chosen(&["Server", "SQLite"]);
    let note = Some(" Add costs ".to_string());
    let (decided, _) = decide(&conn, &id, Verdict::ChangesRequested, note, &ticked, 5).unwrap();
    assert_eq!(
        decided.status,
        Status::Decided(Decision {
            verdict: Verdict::ChangesRequested,
            decided_at: 5,
            note: Some("Add costs".into()),
            chosen: ticked,
            not_sent: false,
        })
    );
    assert_eq!(
        decide(&conn, &id, Verdict::Rejected, Some("No".into()), &[], 6),
        Err(NOT_OPEN.into())
    );
    assert_eq!(
        get(&conn, &id).unwrap(),
        decided,
        "the second decide changed nothing"
    );

    let gone = insert(
        &conn,
        &[
            vec![("requested_by", text("giga-a"))],
            closed("withdrawn", 8),
        ]
        .concat(),
    )
    .unwrap();
    let gone: String = conn
        .query_row(
            "SELECT id FROM approvals WHERE number = ?1",
            [gone],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        decide(&conn, &gone, Verdict::Approved, None, &[], 9),
        Err(NOT_OPEN.into())
    );
    assert_eq!(
        decide(&conn, "missing", Verdict::Approved, None, &[], 9),
        Err(UNKNOWN.into())
    );
}

fn clear(conn: &Connection, chat: &str, now: i64) -> Notices {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).unwrap();
    let notices = mono_messages::clear_session(&tx, &SessionId::parse(chat).unwrap(), now);
    tx.commit().unwrap();
    notices.unwrap()
}

fn move_chat(conn: &Connection, giga: &str, chat: &str) -> Notices {
    let update = json!({ "id": giga, "patch": { "sessionId": chat } });
    let update = crate::mono_roster::validate_update(serde_json::from_value(update).unwrap());
    crate::mono_roster::update_many(conn, &[update.unwrap()]).unwrap()
}

fn not_sent(conn: &Connection, card: &Approval) -> bool {
    match get(conn, &card.id).unwrap().status {
        Status::Decided(decision) => decision.not_sent,
        status => panic!("{status:?}"),
    }
}

/// Giga A's card rejected at 5 into chat-a, Giga B's at 6 into chat-b.
fn rejected_by_two_gigas(conn: &Connection) -> (Approval, Approval) {
    giga_with_chat(conn, "giga-a", CHAT_A);
    giga_with_chat(conn, "giga-b", CHAT_B);
    let a = filed(conn, "giga-a", "Pick a store", "[]");
    let b = filed(conn, "giga-b", "Pick a host", "[]");
    decide(conn, &a.id, Verdict::Rejected, None, &[], 5).unwrap();
    decide(conn, &b.id, Verdict::Rejected, None, &[], 6).unwrap();
    (a, b)
}

fn rejected_row(card: &Approval, chat: &str, at: i64) -> serde_json::Value {
    let body = said(card, "decision: rejected\nchosen_options: []");
    decision_row(card, chat, at, &body)
}

// Defects: a decision whose chat is reset, deleted or archived before the
// Giga takes it is lost; it goes to an ended chat or back to the chat it
// was dropped from; or its card does not show that it was not sent.
#[test]
fn a_dropped_decision_follows_the_giga_to_its_next_chat_or_reads_not_sent() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let (a, b) = rejected_by_two_gigas(&conn);

    assert_eq!(clear(&conn, CHAT_A, 7), Notices::default());
    assert!(
        not_sent(&conn, &a),
        "the reset chat took the decision with it"
    );
    assert_eq!(move_chat(&conn, "giga-a", "chat-a2"), naming("chat-a2"));
    assert_eq!(queued(&conn, "chat-a2"), [rejected_row(&a, "chat-a2", 5)]);
    assert!(!not_sent(&conn, &a));

    assert_eq!(move_chat(&conn, "giga-b", "chat-b2"), Notices::default());
    assert_eq!(clear(&conn, CHAT_B, 8), naming("chat-b2"));
    assert_eq!(queued(&conn, "chat-b2"), [rejected_row(&b, "chat-b2", 6)]);

    move_chat(&conn, "giga-b", "chat-b3");
    conn.execute(
        "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
         VALUES ('chat-b3', '/', 'codex', 'm', 'default', 'B', 1, 1)",
        [],
    )
    .unwrap();
    move_chat(&conn, "giga-a", "chat-a3");
    mono_messages::startup(&conn, 9).unwrap();
    assert!(not_sent(&conn, &a), "chat-a3 is not a session either");
    assert!(queued(&conn, "chat-a3").is_empty());
    assert_eq!(queued(&conn, "chat-b3"), [rejected_row(&b, "chat-b3", 6)]);
    assert!(!not_sent(&conn, &b));
    move_chat(&conn, "giga-b", "chat-b3");
    assert!(
        not_sent(&conn, &a),
        "a later roster write keeps it out of gone chat-a3"
    );
}

// Defects: a decision the user removed from the Giga's queue comes back
// when the Giga moves to a new chat, or one an archive dropped stays unsent
// after the chat is unarchived.
#[test]
fn a_removed_decision_stays_removed_and_an_unarchived_chat_gets_its_decision_back() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let (a, b) = rejected_by_two_gigas(&conn);

    let removed: String = conn
        .query_row(
            "SELECT id FROM mono_messages WHERE request_id = ?1",
            [&a.id],
            |row| row.get(0),
        )
        .unwrap();
    let removed = mono_messages::MessageId::parse(&removed).unwrap();
    mono_messages::drop_message(&conn, &removed, 7).unwrap();
    assert_eq!(move_chat(&conn, "giga-a", "chat-a2"), Notices::default());
    assert!(queued(&conn, "chat-a2").is_empty());
    assert!(not_sent(&conn, &a), "the user removed it");

    let archive = |archived| crate::session_store::set_archived(&conn, CHAT_B, archived).unwrap();
    assert_eq!(archive(true), Notices::default());
    assert!(not_sent(&conn, &b));
    assert_eq!(archive(false), naming(CHAT_B));
    assert_eq!(queued(&conn, CHAT_B), [rejected_row(&b, CHAT_B, 6)]);
    assert!(!not_sent(&conn, &b));
}

// Defect: on the app's real tables the decision row breaks a mono_messages
// CHECK, goes to a chat other than the Giga's current one, or moves the
// rows already waiting there.
#[test]
#[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
fn a_decision_queues_to_a_real_giga_on_a_copy_of_the_app_database() {
    let own_copy = std::env::temp_dir().join(format!("monocode-decide-{}.db", Uuid::new_v4()));
    Connection::open(app_database_copy())
        .unwrap()
        .execute("VACUUM INTO ?1", [own_copy.to_str().unwrap()])
        .unwrap();
    let store = SessionStore::open(own_copy).unwrap();
    let conn = store.lock_conn().unwrap();
    let (giga, chat): (String, String) = conn
        .query_row(
            "SELECT id, json_extract(data_json, '$.sessionId') FROM monos
             WHERE role = 'giga' AND json_extract(data_json, '$.sessionId') IS NOT NULL
             ORDER BY sort_order, id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let waiting = queued(&conn, &chat);
    let start = now_ms();
    let seeded = filed(&conn, &giga, "Real-state seed", "[]");
    decide(&conn, &seeded.id, Verdict::Rejected, None, &[], start).unwrap();
    let card = filed(&conn, &giga, "Real-state decision", PICK);
    let note = Some("Add costs".to_string());
    let ticked = chosen(&["SQLite"]);
    let (_, told) = decide(
        &conn,
        &card.id,
        Verdict::ChangesRequested,
        note,
        &ticked,
        start + 1,
    )
    .unwrap();
    assert_eq!(told, naming(&chat));
    let rejected = "decision: rejected\nchosen_options: []";
    let changes = "decision: changes_requested\nchosen_options: [\"SQLite\"]\ncomment: Add costs";
    let mut expected = waiting;
    expected.push(decision_row(
        &seeded,
        &chat,
        start,
        &said(&seeded, rejected),
    ));
    expected.push(decision_row(&card, &chat, start + 1, &said(&card, changes)));
    assert_eq!(queued(&conn, &chat), expected);
    let sent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mono_messages WHERE request_id = ?1",
            [&card.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sent, 1);
}

// Defects: the command binds other argument names than the board sends,
// leaves out the approvals-changed event or the chat's notice, or emits
// either one for a refused decide. Not on Windows: see `tauri` under
// `[target.'cfg(not(windows))'.dev-dependencies]` in Cargo.toml.
#[cfg(not(windows))]
#[test]
fn approvals_decide_emits_the_chat_notice_and_approvals_changed_once_per_decision() {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tauri::{Listener, Manager};

    use crate::approvals::CHANGED;
    use crate::mono_messages::tests::invoke_command;

    let app = tauri::test::mock_builder()
        .manage(SessionStore::open_in_memory().unwrap())
        .invoke_handler(tauri::generate_handler![
            approvals_decide,
            crate::mono_roster::monos_update
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .unwrap();
    let card = {
        let conn = app.state::<SessionStore>().inner().lock_conn().unwrap();
        giga_with_chat(&conn, "giga-a", CHAT_A);
        filed(&conn, "giga-a", "Pick a store", PICK)
    };
    let heard = Arc::new(Mutex::new(Vec::new()));
    for event in ["mono-message-notices", CHANGED] {
        let heard = heard.clone();
        app.listen_any(event, move |emitted| {
            let payload: Value = serde_json::from_str(emitted.payload()).unwrap();
            heard.lock().unwrap().push((event, payload));
        });
    }
    let decide_call = |args: &Value| invoke_command(&webview, "approvals_decide", args);
    let args =
        json!({ "id": card.id, "verdict": "approved", "note": "Ship it", "chosen": ["SQLite"] });

    let decided = decide_call(&args).unwrap();
    assert_eq!(decided["status"]["verdict"], "approved");
    assert_eq!(
        *heard.lock().unwrap(),
        [
            ("mono-message-notices", json!({ "sessions": [CHAT_A] })),
            (CHANGED, json!({ "projectPath": card.project_path })),
        ]
    );
    assert_eq!(decide_call(&args), Err(json!(NOT_OPEN)));
    assert_eq!(
        heard.lock().unwrap().len(),
        2,
        "a refused decide emits nothing"
    );
    clear(
        &app.state::<SessionStore>().inner().lock_conn().unwrap(),
        CHAT_A,
        9,
    );
    let moved = json!({ "updates": [{ "id": "giga-a", "patch": { "sessionId": "chat-a2" } }] });
    invoke_command(&webview, "monos_update", &moved).unwrap();
    assert_eq!(
        heard.lock().unwrap()[2],
        ("mono-message-notices", json!({ "sessions": ["chat-a2"] })),
        "the Giga's new chat is told about the decision it now holds"
    );
    let store = app.state::<SessionStore>();
    crate::session_store::set_archived(&store.lock_conn().unwrap(), "chat-a2", true).unwrap();
    crate::session_store::unarchive(app.handle(), store.inner(), "chat-a2").unwrap();
    assert_eq!(
        heard.lock().unwrap()[3..],
        [("mono-message-notices", json!({ "sessions": ["chat-a2"] }))],
        "an unarchived chat is told about the decision it got back"
    );
}
