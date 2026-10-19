use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as Sql;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

const PROJECT: &str = "/Users/test/orbit";
const OTHER: &str = "/Users/test/other";

fn text(value: &str) -> Sql {
    Sql::Text(value.into())
}

fn giga(conn: &Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
         VALUES (?1, ?2, 'giga', '{}', 0, 0, 0)",
        [id, name],
    )
    .unwrap();
}

/// Inserts a valid open card with `changes` applied and returns its number.
fn insert(conn: &Connection, changes: &[(&'static str, Sql)]) -> rusqlite::Result<i64> {
    let id = Uuid::new_v4().to_string();
    let mut row = vec![
        ("id", text(&id)),
        ("project_path", text(PROJECT)),
        ("heading", text(&id)),
        ("summary", text("Pick where approval cards live.")),
        (
            "tradeoffs",
            text(r#"{"pros":["Local"],"cons":["One machine"]}"#),
        ),
        ("options", text("[]")),
        ("urgency", text("blocking")),
        ("links", text("[]")),
        ("requested_by_name", text("Giga A")),
        ("created_at", Sql::Integer(1)),
        ("status", text("open")),
    ];
    for (column, value) in changes {
        match row.iter_mut().find(|(name, _)| name == column) {
            Some((_, cell)) => *cell = value.clone(),
            None => row.push((column, value.clone())),
        }
    }
    let names: Vec<&str> = row.iter().map(|(name, _)| *name).collect();
    conn.query_row(
        &format!(
            "INSERT INTO approvals ({}) VALUES ({}) RETURNING number",
            names.join(", "),
            vec!["?"; names.len()].join(", ")
        ),
        rusqlite::params_from_iter(row.iter().map(|(_, value)| value)),
        |row| row.get(0),
    )
}

fn closed(status: &'static str, at: i64) -> Vec<(&'static str, Sql)> {
    let column = if status == "withdrawn" {
        "withdrawn_at"
    } else {
        "decided_at"
    };
    vec![("status", text(status)), (column, Sql::Integer(at))]
}

fn snapshot(conn: &Connection) -> Vec<(String, Option<String>, i64)> {
    let objects: Vec<(String, String, Option<String>)> = conn
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    objects
        .into_iter()
        .map(|(kind, name, sql)| {
            let rows = if kind == "table" {
                conn.query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |row| {
                    row.get(0)
                })
                .unwrap()
            } else {
                -1
            };
            (name, sql, rows)
        })
        .collect()
}

fn cards_survive_reopen(path: PathBuf) {
    let (cards, counts, after_filing) = {
        let store = SessionStore::open(path.clone()).unwrap();
        let conn = store.lock_conn().unwrap();
        let first_giga: Option<(String, String)> = conn
            .query_row(
                "SELECT id, name FROM monos WHERE role = 'giga' ORDER BY sort_order, id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap();
        let (id, name) = first_giga.unwrap_or_else(|| {
            giga(&conn, "giga-a", "Giga A");
            ("giga-a".into(), "Giga A".into())
        });
        let filer = [
            ("requested_by", text(&id)),
            ("requested_by_name", text(&name)),
        ];
        insert(&conn, &filer).unwrap();
        insert(&conn, &[filer.to_vec(), closed("approved", 12)].concat()).unwrap();
        (
            list(&conn, PROJECT).unwrap(),
            open_counts(&conn).unwrap(),
            snapshot(&conn),
        )
    };
    assert_eq!(cards.len(), 2);
    assert_eq!(counts.total, 1);
    for open in ["second", "third"] {
        let store = SessionStore::open(path.clone()).unwrap();
        let conn = store.lock_conn().unwrap();
        assert_eq!(
            list(&conn, PROJECT).unwrap(),
            cards,
            "the {open} open keeps every card"
        );
        assert_eq!(open_counts(&conn).unwrap(), counts);
        assert_eq!(
            snapshot(&conn),
            after_filing,
            "the {open} open changes no table, index or row count"
        );
    }
}

// Defects: the table is not created by `migrate`, a reopen drops or
// recreates it, or its CREATE fails on a database that already has it.
#[test]
fn cards_survive_reopen_and_a_second_open_changes_nothing() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "monocode-approvals-{}-{nanos}.db",
        std::process::id()
    ));
    cards_survive_reopen(path);
}

fn app_database_copy() -> PathBuf {
    PathBuf::from(std::env::var("MONOCODE_DB_COPY").unwrap())
}

#[test]
#[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
fn cards_survive_reopen_on_a_copy_of_the_app_database() {
    cards_survive_reopen(app_database_copy());
}

fn real_mono(store: &SessionStore, role: &str) -> (String, String, Value) {
    let (id, name, data): (String, String, String) = store
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT id, name, data_json FROM monos
             WHERE role = ?1 AND json_extract(data_json, '$.sessionId') IS NOT NULL
             ORDER BY sort_order, id LIMIT 1",
            [role],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    (id, name, serde_json::from_str(&data).unwrap())
}

// Defects: the card loses the real Giga's name, or filing the same
// heading again makes a second card.
#[test]
#[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
fn writes_on_a_copy_of_the_app_database() {
    let store = SessionStore::open(app_database_copy()).unwrap();
    let (giga, giga_name, _) = real_mono(&store, "giga");
    let (_, _, mono_data) = real_mono(&store, "mono");
    let conn = store.lock_conn().unwrap();
    let project = mono_data["projects"][0].as_str().unwrap();
    let by = parse_by(&giga).unwrap();
    let heading = format!("Real-state check {}", Uuid::new_v4().simple());
    let filed = file(&conn, project, &by, card(&heading), now_ms()).unwrap();
    let again = Card {
        heading: format!("  {heading} "),
        summary: "Filed again with padding.".into(),
        ..card(&heading)
    };
    let refiled = file(&conn, project, &by, again, now_ms()).unwrap();
    assert_eq!(refiled.id, filed.id);
    let listed = list(&conn, project).unwrap().into_iter();
    let same_heading = listed.filter(|approval| approval.card.heading == heading);
    assert_eq!(same_heading.count(), 1, "the refile kept one card");
    update(&conn, &filed.id, &by, card(&format!("{heading} renamed"))).unwrap();
    let withdrawn = withdraw(&conn, &filed.id, &by, now_ms()).unwrap();
    assert!(matches!(withdrawn.status, Status::Withdrawn { .. }));
    assert_eq!(
        withdrawn.requested_by,
        Filer {
            id: Some(by),
            name: giga_name
        }
    );
}

// Defects: the board mixes closed cards among open ones, puts a newer
// not-blocking card above an older blocking one, sorts closed cards by
// filing time, does not put the newer of two same-time cards first,
// counts closed cards, reuses a deleted card's number, or lists a path
// that is not normalized.
#[test]
fn list_orders_cards_as_the_board_does_and_open_counts_count_open_cards() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let card = |project: &str, urgency: &str, created_at: i64, closing: &[(&'static str, Sql)]| {
        let mut changes = vec![
            ("project_path", text(project)),
            ("urgency", text(urgency)),
            ("created_at", Sql::Integer(created_at)),
        ];
        changes.extend_from_slice(closing);
        insert(&conn, &changes).unwrap()
    };
    let old_blocking = card(PROJECT, "blocking", 1, &[]);
    let rejected = card(PROJECT, "blocking", 0, &closed("rejected", 6));
    let new_not_blocking = card(PROJECT, "not_blocking", 4, &[]);
    let withdrawn = card(PROJECT, "not_blocking", 1, &closed("withdrawn", 7));
    let new_blocking = card(PROJECT, "blocking", 3, &[]);
    let same_time_later = card(PROJECT, "blocking", 3, &[]);
    let approved = card(PROJECT, "blocking", 4, &closed("approved", 5));
    let old_not_blocking = card(PROJECT, "not_blocking", 2, &[]);
    card(OTHER, "not_blocking", 1, &[]);
    let last = card(OTHER, "blocking", 1, &closed("withdrawn", 2));
    let numbers: Vec<i64> = list(&conn, PROJECT)
        .unwrap()
        .into_iter()
        .map(|approval| approval.number)
        .collect();
    assert_eq!(
        numbers,
        [
            same_time_later,
            new_blocking,
            old_blocking,
            new_not_blocking,
            old_not_blocking,
            withdrawn,
            rejected,
            approved
        ]
    );
    assert_eq!(
        open_counts(&conn).unwrap(),
        OpenCounts {
            total: 6,
            by_project: BTreeMap::from([(PROJECT.into(), 5), (OTHER.into(), 1)]),
        }
    );
    conn.execute("DELETE FROM approvals WHERE number = ?1", [last])
        .unwrap();
    assert_eq!(card(OTHER, "blocking", 3, &[]), last + 1);
    let error = list(&conn, "/Users/test/orbit/").unwrap_err();
    assert!(error.starts_with("invalid_input"), "{error}");
}

// Defects: a stored field does not reach the board, a key written by a
// newer build (in tradeoffs, options or links) makes the row unreadable,
// or deleting the filing Giga deletes the card, is refused, or loses the
// Giga's name.
#[test]
fn a_card_reads_back_whole_and_keeps_its_filer_name_when_the_giga_is_deleted() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    giga(&conn, "giga-a", "Giga A");
    let decided_id = "00000000-0000-4000-8000-000000000001";
    let withdrawn_id = "00000000-0000-4000-8000-000000000002";
    let decided = insert(
        &conn,
        &[
            ("id", text(decided_id)),
            ("heading", text("Pick a store")),
            ("tradeoffs", text(r#"{"pros":["Local"],"cons":["One machine"],"risks":[]}"#)),
            ("options", text(r#"[{"label":"SQLite","description":"Local.","recommended":true,"icon":"db"},{"label":"Server"}]"#)),
            ("links", text(r#"[{"kind":"pr","title":"PR #109 caller identity","url":"https://x.dev/109","pinned":true},{"kind":"linear","url":"https://x.dev/ADE-51"}]"#)),
            ("origin", text("MonoCode Fork Orchestrator")),
            ("requested_by", text("giga-a")),
            ("created_at", Sql::Integer(10)),
            ("status", text("changes_requested")),
            ("decided_at", Sql::Integer(12)),
            ("decision_note", text("Add costs")),
            ("chosen_options", text(r#"["Server"]"#)),
        ],
    )
    .unwrap();
    let withdrawn = insert(
        &conn,
        &[
            ("id", text(withdrawn_id)),
            ("heading", text("Bump Playwright")),
            ("urgency", text("not_blocking")),
            ("requested_by", text("giga-a")),
            ("status", text("withdrawn")),
            ("withdrawn_at", Sql::Integer(11)),
        ],
    )
    .unwrap();
    let mut board = json!([
        {
            "id": decided_id, "number": decided, "projectPath": PROJECT,
            "heading": "Pick a store", "summary": "Pick where approval cards live.",
            "tradeoffs": { "pros": ["Local"], "cons": ["One machine"] },
            "options": [
                { "label": "SQLite", "description": "Local.", "recommended": true },
                { "label": "Server", "description": null, "recommended": false }
            ],
            "urgency": "blocking",
            "links": [
                { "kind": "pr", "title": "PR #109 caller identity", "url": "https://x.dev/109" },
                { "kind": "linear", "title": null, "url": "https://x.dev/ADE-51" }
            ],
            "origin": "MonoCode Fork Orchestrator",
            "requestedBy": { "id": "giga-a", "name": "Giga A" },
            "createdAt": 10,
            "status": {
                "state": "decided", "verdict": "changes_requested", "decidedAt": 12,
                "note": "Add costs", "chosen": ["Server"]
            }
        },
        {
            "id": withdrawn_id, "number": withdrawn, "projectPath": PROJECT,
            "heading": "Bump Playwright", "summary": "Pick where approval cards live.",
            "tradeoffs": { "pros": ["Local"], "cons": ["One machine"] },
            "options": [], "urgency": "not_blocking", "links": [], "origin": null,
            "requestedBy": { "id": "giga-a", "name": "Giga A" },
            "createdAt": 1,
            "status": { "state": "withdrawn", "withdrawnAt": 11 }
        }
    ]);
    let read = || serde_json::to_value(list(&conn, PROJECT).unwrap()).unwrap();
    assert_eq!(read(), board);
    assert!(crate::mono_roster::delete(&conn, "giga-a").unwrap());
    board[0]["requestedBy"]["id"] = json!(null);
    board[1]["requestedBy"]["id"] = json!(null);
    assert_eq!(read(), board, "the cards stay, with the Giga's name");
}

// Defect: a second open card takes a heading that is open in the project,
// or a closed card's heading cannot be filed again.
#[test]
fn one_open_card_per_project_and_heading() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let heading = ("heading", text("Ship it"));
    insert(
        &conn,
        &[vec![heading.clone()], closed("rejected", 2)].concat(),
    )
    .unwrap();
    insert(&conn, std::slice::from_ref(&heading)).unwrap();
    insert(&conn, &[heading.clone(), ("project_path", text(OTHER))]).unwrap();
    let error = insert(&conn, &[heading]).unwrap_err().to_string();
    assert!(error.contains("UNIQUE constraint failed"), "{error}");
}

// Defect: a CHECK is missing, so the table holds a row `row_to_approval`
// cannot turn into a `Status`. Each refused row breaks one CHECK only.
#[test]
fn checks_refuse_rows_the_types_cannot_hold() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let options = ("options", text(r#"[{"label":"A"}]"#));
    let chosen = ("chosen_options", text(r#"["A"]"#));
    let approved = || closed("approved", 1);
    insert(&conn, &[]).unwrap();
    insert(&conn, &closed("withdrawn", 1)).unwrap();
    let note = ("decision_note", text("n"));
    insert(
        &conn,
        &[
            approved(),
            vec![note.clone(), options.clone(), chosen.clone()],
        ]
        .concat(),
    )
    .unwrap();
    for (case, changes) in [
        ("an unknown status", vec![("status", text("closed"))]),
        (
            "decided_at on an open card",
            vec![("decided_at", Sql::Integer(1))],
        ),
        (
            "approved with no decided_at",
            vec![("status", text("approved"))],
        ),
        (
            "withdrawn_at on an open card",
            vec![("withdrawn_at", Sql::Integer(1))],
        ),
        (
            "withdrawn with no withdrawn_at",
            vec![("status", text("withdrawn"))],
        ),
        (
            "a note on a withdrawn card",
            [closed("withdrawn", 1), vec![note]].concat(),
        ),
        (
            "chosen options on an open card",
            vec![options.clone(), chosen.clone()],
        ),
        (
            "chosen options with no options",
            [approved(), vec![chosen]].concat(),
        ),
        (
            "an empty chosen list",
            [approved(), vec![options, ("chosen_options", text("[]"))]].concat(),
        ),
        ("options that are not a list", vec![("options", text("{}"))]),
        ("links that are not a list", vec![("links", text("{}"))]),
        ("tradeoffs that are a list", vec![("tradeoffs", text("[]"))]),
        (
            "tradeoffs with no cons",
            vec![("tradeoffs", text(r#"{"pros":[]}"#))],
        ),
        (
            "tradeoffs with no pros",
            vec![("tradeoffs", text(r#"{"cons":[]}"#))],
        ),
        ("an unknown urgency", vec![("urgency", text("urgent"))]),
        ("an untrimmed heading", vec![("heading", text(" Ship it"))]),
        ("an untrimmed origin", vec![("origin", text(" General"))]),
        (
            "a project path ending in /",
            vec![("project_path", text("/Users/test/orbit/"))],
        ),
    ] {
        let error = insert(&conn, &changes).expect_err(case).to_string();
        assert!(error.contains("CHECK constraint failed"), "{case}: {error}");
    }
    let error = insert(&conn, &[("requested_by_name", Sql::Null)])
        .expect_err("a card with no filer name")
        .to_string();
    assert!(
        error.contains("NOT NULL constraint failed: approvals.requested_by_name"),
        "{error}"
    );
}

fn filer(conn: &Connection, id: &str) -> MonoId {
    giga(conn, id, &format!("Giga {id}"));
    MonoId::parse(id).unwrap()
}

fn card(heading: &str) -> Card {
    Card {
        heading: heading.into(),
        summary: "Pick where approval cards live.".into(),
        tradeoffs: Tradeoffs {
            pros: vec!["Local and fast".into()],
            cons: Vec::new(),
        },
        options: Vec::new(),
        urgency: Urgency::Blocking,
        links: Vec::new(),
        origin: None,
    }
}

fn with_options(heading: &str) -> Card {
    let option = |label: &str, recommended| ApprovalOption {
        label: label.into(),
        description: Some(format!("Store cards in {label}.")),
        recommended,
    };
    Card {
        options: vec![option("SQLite", true), option("Server", false)],
        urgency: Urgency::NotBlocking,
        links: vec![Link {
            kind: LinkKind::Pr,
            title: Some("PR #1".into()),
            url: "https://github.com/tylers2222/Monocode-Unlocked/pull/1".into(),
        }],
        origin: Some("MonoCode Fork Orchestrator".into()),
        ..card(heading)
    }
}

fn statuses(conn: &Connection, project: &str) -> Vec<(String, &'static str)> {
    list(conn, project)
        .unwrap()
        .into_iter()
        .map(|approval| {
            let status = match approval.status {
                Status::Open => "open",
                Status::Withdrawn { .. } => "withdrawn",
                Status::Decided(decision) => decision.verdict.as_str(),
            };
            (approval.id, status)
        })
        .collect()
}

// Defects: a second filing makes a duplicate, keeps a field of the first
// filing, rewrites created_at, or reopens a withdrawn card; or
// the card does not keep the filing Giga's name.
#[test]
fn filing_an_open_heading_again_updates_that_card_and_a_closed_one_starts_anew() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let a = filer(&conn, "giga-a");
    let first = file(&conn, PROJECT, &a, card("Ship it"), 1).unwrap();
    assert_eq!(
        first.requested_by,
        Filer {
            id: Some(a.clone()),
            name: "Giga giga-a".into()
        }
    );
    let resend = Card {
        heading: "  Ship it ".into(),
        summary: "Now with numbers.".into(),
        tradeoffs: Tradeoffs {
            pros: vec![" One store ".into()],
            cons: vec!["Slower".into()],
        },
        ..with_options("unused")
    };
    let again = file(&conn, PROJECT, &a, resend, 2).unwrap();
    assert_eq!(again.id, first.id, "the open card is updated");
    assert_eq!(
        again.card,
        Card {
            heading: "Ship it".into(),
            summary: "Now with numbers.".into(),
            tradeoffs: Tradeoffs {
                pros: vec!["One store".into()],
                cons: vec!["Slower".into()],
            },
            ..with_options("unused")
        }
    );
    assert_eq!((again.number, again.created_at), (first.number, 1));
    assert_eq!(list(&conn, PROJECT).unwrap(), vec![again]);
    let elsewhere = file(&conn, OTHER, &a, card("Ship it"), 3).unwrap();
    assert_ne!(elsewhere.id, first.id, "another project gets its own card");

    withdraw(&conn, &first.id, &a, 4).unwrap();
    let refiled = file(&conn, PROJECT, &a, card("Ship it"), 5).unwrap();
    withdraw(&conn, &refiled.id, &a, 6).unwrap();
    let after_withdraw = file(&conn, PROJECT, &a, card("Ship it"), 7).unwrap();
    assert_eq!(
        statuses(&conn, PROJECT),
        [
            (after_withdraw.id, "open"),
            (refiled.id, "withdrawn"),
            (first.id, "withdrawn")
        ]
    );
}

// Defects: another Giga's call changes, withdraws or takes over the card,
// the filer can still change it once it is closed, or a withdrawn card
// has no time.
#[test]
fn another_giga_cannot_update_withdraw_or_take_over_a_card() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let a = filer(&conn, "giga-a");
    let b = filer(&conn, "giga-b");
    let mine = file(&conn, PROJECT, &a, card("Ship it"), 1).unwrap();
    let theirs = Card {
        summary: "Taken over.".into(),
        ..card("Ship it")
    };
    assert_eq!(
        update(&conn, &mine.id, &b, theirs.clone()),
        Err(NOT_YOURS.into())
    );
    assert_eq!(withdraw(&conn, &mine.id, &b, 2), Err(NOT_YOURS.into()));
    assert_eq!(
        file(&conn, PROJECT, &b, theirs, 2),
        Err(HEADING_TAKEN.into())
    );
    assert_eq!(
        list(&conn, PROJECT).unwrap(),
        vec![mine.clone()],
        "A's card is unchanged"
    );

    let other = file(&conn, PROJECT, &b, card("Ship that"), 3).unwrap();
    assert_eq!(
        update(&conn, &other.id, &b, card("Ship it")),
        Err(HEADING_TAKEN.into()),
        "a rename cannot take another open card's heading"
    );
    let renamed = update(&conn, &mine.id, &a, card("Ship it now")).unwrap();
    assert_eq!(renamed.card.heading, "Ship it now");
    assert_eq!(renamed.requested_by.id, Some(a.clone()));
    assert_eq!(
        withdraw(&conn, &mine.id, &a, 4).unwrap().status,
        Status::Withdrawn { withdrawn_at: 4 }
    );
    assert_eq!(withdraw(&conn, &mine.id, &a, 5), Err(NOT_OPEN.into()));
    assert_eq!(
        update(&conn, &mine.id, &a, card("Again")),
        Err(NOT_OPEN.into())
    );
}

// Defects: a check in `Card::into_columns` is missing, so bad input is
// stored or fails as a raw CHECK error instead of `invalid_input`.
#[test]
fn bad_input_is_invalid_input_and_writes_nothing() {
    let store = SessionStore::open_in_memory().unwrap();
    let conn = store.lock_conn().unwrap();
    let a = filer(&conn, "giga-a");
    let mut same_labels = with_options("Same labels");
    same_labels.options[1].label = " SQLite ".into();
    let mut blank_label = with_options("Blank label");
    blank_label.options[0].label = " ".into();
    let mut blank_description = with_options("Blank description");
    blank_description.options[0].description = Some(" ".into());
    let mut http = with_options("Plain http");
    http.links[0].url = "http://example.com".into();
    let mut blank_title = with_options("Blank title");
    blank_title.links[0].title = Some(String::new());
    let mut blank_pro = card("Blank pro");
    blank_pro.tradeoffs.pros.push(" ".into());
    let mut blank_con = card("Blank con");
    blank_con.tradeoffs.cons.push(" ".into());
    let mut no_tradeoffs = card("No tradeoffs");
    no_tradeoffs.tradeoffs.pros.clear();
    let cases = [
        ("blank heading", card("  ")),
        ("long heading", card(&"h".repeat(201))),
        (
            "blank summary",
            Card {
                summary: "\n".into(),
                ..card("Blank summary")
            },
        ),
        ("blank pro", blank_pro),
        ("blank con", blank_con),
        ("no pros or cons", no_tradeoffs),
        (
            "blank origin",
            Card {
                origin: Some(" ".into()),
                ..card("Blank origin")
            },
        ),
        ("same labels", same_labels),
        ("blank label", blank_label),
        ("blank description", blank_description),
        ("http link", http),
        ("blank link title", blank_title),
    ];
    for (case, bad) in cases {
        let error = file(&conn, PROJECT, &a, bad, 1).unwrap_err();
        assert!(error.starts_with("invalid_input"), "{case}: {error}");
    }
    let error = file(&conn, "/Users/test/orbit/", &a, card("Trailing slash"), 1).unwrap_err();
    assert!(error.starts_with("invalid_input"), "{error}");
    assert_eq!(list(&conn, PROJECT).unwrap(), []);
}
