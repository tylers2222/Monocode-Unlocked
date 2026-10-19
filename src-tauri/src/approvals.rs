use std::collections::BTreeMap;

use rusqlite::{Connection, Row};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::milestones::check_project;
use crate::mono_messages::MonoId;
use crate::session_store::SessionStore;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS approvals (
           number            INTEGER PRIMARY KEY AUTOINCREMENT,
           id                TEXT NOT NULL UNIQUE CHECK (length(id) = 36),
           project_path      TEXT NOT NULL
             CHECK (project_path = '/' OR (project_path <> '' AND project_path NOT GLOB '*/')),
           heading           TEXT NOT NULL CHECK (heading = trim(heading) AND length(heading) BETWEEN 1 AND 200),
           summary           TEXT NOT NULL CHECK (summary = trim(summary) AND length(summary) BETWEEN 1 AND 8000),
           tradeoffs         TEXT NOT NULL CHECK (json_valid(tradeoffs)
                               AND json_type(tradeoffs, '$.pros') IS 'array'
                               AND json_type(tradeoffs, '$.cons') IS 'array'),
           options           TEXT NOT NULL CHECK (json_valid(options) AND json_type(options) = 'array'),
           urgency           TEXT NOT NULL CHECK (urgency IN ('blocking', 'not_blocking')),
           links             TEXT NOT NULL CHECK (json_valid(links) AND json_type(links) = 'array'),
           origin            TEXT CHECK (origin = trim(origin) AND length(origin) BETWEEN 1 AND 200),
           requested_by      TEXT REFERENCES monos(id) ON DELETE SET NULL,
           requested_by_name TEXT NOT NULL,
           created_at        INTEGER NOT NULL,
           status            TEXT NOT NULL CHECK (status IN
                               ('open', 'approved', 'rejected', 'changes_requested', 'withdrawn')),
           decided_at        INTEGER,
           withdrawn_at      INTEGER,
           decision_note     TEXT CHECK (decision_note = trim(decision_note)
                               AND length(decision_note) BETWEEN 1 AND 8000),
           chosen_options    TEXT CHECK (json_valid(chosen_options)
                               AND json_type(chosen_options) = 'array'
                               AND json_array_length(chosen_options) > 0),
           CHECK ((decided_at IS NOT NULL) = (status IN ('approved', 'rejected', 'changes_requested'))),
           CHECK ((withdrawn_at IS NOT NULL) = (status = 'withdrawn')),
           CHECK (decided_at IS NOT NULL OR (decision_note IS NULL AND chosen_options IS NULL)),
           CHECK (chosen_options IS NULL OR json_array_length(options) > 0)
         );
         CREATE INDEX IF NOT EXISTS approvals_project_status ON approvals (project_path, status);
         CREATE UNIQUE INDEX IF NOT EXISTS approvals_open_heading
           ON approvals (project_path, heading) WHERE status = 'open';",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Blocking,
    NotBlocking,
}

impl Urgency {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::NotBlocking => "not_blocking",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Blocking, Self::NotBlocking]
            .into_iter()
            .find(|urgency| urgency.as_str() == value)
    }
}

// Stored JSON types ignore keys they do not know, so an older build still
// reads a row that a newer build wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tradeoffs {
    pub pros: Vec<String>,
    pub cons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub label: String,
    pub description: Option<String>,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Pr,
    Doop,
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    pub title: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub heading: String,
    pub summary: String,
    pub tradeoffs: Tradeoffs,
    pub options: Vec<ApprovalOption>,
    pub urgency: Urgency,
    pub links: Vec<Link>,
    pub origin: Option<String>,
}

/// The filing Giga: its id until the Giga is deleted, and its name as filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Filer {
    pub id: Option<MonoId>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approved,
    Rejected,
    ChangesRequested,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::ChangesRequested => "changes_requested",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Approved, Self::Rejected, Self::ChangesRequested]
            .into_iter()
            .find(|verdict| verdict.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub verdict: Verdict,
    pub decided_at: i64,
    pub note: Option<String>,
    pub chosen: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Open,
    Withdrawn {
        #[serde(rename = "withdrawnAt")]
        withdrawn_at: i64,
    },
    Decided(Decision),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    /// Shown as APR-<number>. AUTOINCREMENT never hands out a number twice.
    pub number: i64,
    pub project_path: String,
    #[serde(flatten)]
    pub card: Card,
    pub requested_by: Filer,
    pub created_at: i64,
    pub status: Status,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCounts {
    pub total: u32,
    pub by_project: BTreeMap<String, u32>,
}

fn invalid(column: &str, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("invalid approvals.{column}: {error}").into(),
    )
}

fn json<T: DeserializeOwned>(column: &str, text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|e| invalid(column, e))
}

const COLUMNS: &str = "number, id, project_path, heading, summary, tradeoffs, options, urgency,
  links, origin, requested_by, requested_by_name, created_at, status, decided_at, withdrawn_at,
  decision_note, chosen_options";

fn row_to_approval(row: &Row<'_>) -> rusqlite::Result<Approval> {
    let status: String = row.get("status")?;
    let closed_at = (row.get("decided_at")?, row.get("withdrawn_at")?);
    let status = match (status.as_str(), closed_at) {
        ("open", (None, None)) => Status::Open,
        ("withdrawn", (None, Some(withdrawn_at))) => Status::Withdrawn { withdrawn_at },
        (verdict, (Some(decided_at), None)) => Status::Decided(Decision {
            verdict: Verdict::parse(verdict).ok_or_else(|| invalid("status", verdict))?,
            decided_at,
            note: row.get("decision_note")?,
            chosen: row
                .get::<_, Option<String>>("chosen_options")?
                .map(|chosen| json("chosen_options", &chosen))
                .transpose()?
                .unwrap_or_default(),
        }),
        (status, _) => return Err(invalid("status", status)),
    };
    let urgency: String = row.get("urgency")?;
    let requested_by: Option<String> = row.get("requested_by")?;
    Ok(Approval {
        id: row.get("id")?,
        number: row.get("number")?,
        project_path: row.get("project_path")?,
        card: Card {
            heading: row.get("heading")?,
            summary: row.get("summary")?,
            tradeoffs: json("tradeoffs", &row.get::<_, String>("tradeoffs")?)?,
            options: json("options", &row.get::<_, String>("options")?)?,
            urgency: Urgency::parse(&urgency).ok_or_else(|| invalid("urgency", &urgency))?,
            links: json("links", &row.get::<_, String>("links")?)?,
            origin: row.get("origin")?,
        },
        requested_by: Filer {
            id: requested_by
                .map(|id| MonoId::parse(&id).ok_or_else(|| invalid("requested_by", &id)))
                .transpose()?,
            name: row.get("requested_by_name")?,
        },
        created_at: row.get("created_at")?,
        status,
    })
}

/// Open cards first, blocking before not blocking; then closed cards. Newest
/// first inside each group: open by filing time, closed by closing time.
pub(crate) fn list(conn: &Connection, project: &str) -> Result<Vec<Approval>, String> {
    check_project(project)?;
    conn.prepare(&format!(
        "SELECT {COLUMNS} FROM approvals WHERE project_path = ?1
         ORDER BY status <> 'open', status = 'open' AND urgency <> 'blocking',
           coalesce(decided_at, withdrawn_at, created_at) DESC, number DESC"
    ))
    .and_then(|mut statement| statement.query_map([project], row_to_approval)?.collect())
    .map_err(|e| e.to_string())
}

pub(crate) fn open_counts(conn: &Connection) -> Result<OpenCounts, String> {
    let by_project: BTreeMap<String, u32> = conn
        .prepare(
            "SELECT project_path, COUNT(*) FROM approvals WHERE status = 'open'
             GROUP BY project_path",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .map_err(|e| e.to_string())?;
    Ok(OpenCounts {
        total: by_project.values().sum(),
        by_project,
    })
}

#[tauri::command(async)]
pub fn approvals_list(
    store: State<'_, SessionStore>,
    project_path: String,
) -> Result<Vec<Approval>, String> {
    list(&*store.lock_conn()?, &project_path)
}

#[tauri::command(async)]
pub fn approvals_open_counts(store: State<'_, SessionStore>) -> Result<OpenCounts, String> {
    open_counts(&*store.lock_conn()?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::types::Value as Sql;
    use rusqlite::OptionalExtension;
    use serde_json::json;
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

    #[test]
    #[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
    fn cards_survive_reopen_on_a_copy_of_the_app_database() {
        cards_survive_reopen(PathBuf::from(std::env::var("MONOCODE_DB_COPY").unwrap()));
    }

    // Defects: the board mixes closed cards among open ones, puts a newer
    // not-blocking card above an older blocking one, sorts closed cards by
    // filing time, counts closed cards, or reuses a deleted card's number.
    #[test]
    fn list_orders_cards_as_the_board_does_and_open_counts_count_open_cards() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let card =
            |project: &str, urgency: &str, created_at: i64, closing: &[(&'static str, Sql)]| {
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
                total: 5,
                by_project: BTreeMap::from([(PROJECT.into(), 4), (OTHER.into(), 1)]),
            }
        );
        conn.execute("DELETE FROM approvals WHERE number = ?1", [last])
            .unwrap();
        assert_eq!(card(OTHER, "blocking", 3, &[]), last + 1);
    }

    // Defects: a stored field does not reach the board, a key written by a
    // newer build makes the row unreadable, or deleting the filing Giga
    // deletes the card, is refused, or loses the Giga's name.
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
            ("tradeoffs that are a list", vec![("tradeoffs", text("[]"))]),
            (
                "tradeoffs with no cons",
                vec![("tradeoffs", text(r#"{"pros":[]}"#))],
            ),
            ("an unknown urgency", vec![("urgency", text("urgent"))]),
        ] {
            let error = insert(&conn, &changes).expect_err(case).to_string();
            assert!(error.contains("CHECK constraint failed"), "{case}: {error}");
        }
    }
}
