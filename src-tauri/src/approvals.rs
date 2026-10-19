// Remove once the approvals_board tool (ADE-51) and delivery (ADE-52) call this store.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;

use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension, Row, ToSql};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::milestones::{check_project, require};
use crate::mono_messages::MonoId;

pub(crate) const UNKNOWN: &str = "unknown_approval";
pub(crate) const NOT_YOURS: &str = "not_your_approval";
pub(crate) const NOT_OPEN: &str = "approval_not_open";
pub(crate) const HEADING_TAKEN: &str = "heading_taken";

const HEADING_MAX: usize = 200;
const TEXT_MAX: usize = 8000;
const LABEL_MAX: usize = 200;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS approvals (
           id             TEXT PRIMARY KEY CHECK (length(id) = 36),
           project_path   TEXT NOT NULL
             CHECK (project_path = '/' OR (project_path <> '' AND project_path NOT GLOB '*/')),
           heading        TEXT NOT NULL CHECK (heading = trim(heading) AND length(heading) BETWEEN 1 AND 200),
           summary        TEXT NOT NULL CHECK (summary = trim(summary) AND length(summary) BETWEEN 1 AND 8000),
           tradeoffs      TEXT NOT NULL CHECK (tradeoffs = trim(tradeoffs) AND length(tradeoffs) BETWEEN 1 AND 8000),
           options        TEXT NOT NULL CHECK (json_valid(options) AND json_type(options) = 'array'),
           urgency        TEXT NOT NULL CHECK (urgency IN ('blocking', 'not_blocking')),
           links          TEXT NOT NULL CHECK (json_valid(links) AND json_type(links) = 'array'),
           origin         TEXT CHECK (origin = trim(origin) AND length(origin) BETWEEN 1 AND 200),
           requested_by   TEXT REFERENCES monos(id) ON DELETE SET NULL,
           created_at     INTEGER NOT NULL,
           status         TEXT NOT NULL CHECK (status IN
                            ('open', 'approved', 'rejected', 'changes_requested', 'withdrawn')),
           decided_at     INTEGER,
           decision_note  TEXT CHECK (decision_note = trim(decision_note)
                            AND length(decision_note) BETWEEN 1 AND 8000),
           chosen_options TEXT CHECK (json_valid(chosen_options)
                            AND json_type(chosen_options) = 'array'
                            AND json_array_length(chosen_options) > 0),
           CHECK ((decided_at IS NOT NULL) = (status IN ('approved', 'rejected', 'changes_requested'))),
           CHECK (decided_at IS NOT NULL OR (decision_note IS NULL AND chosen_options IS NULL)),
           CHECK (chosen_options IS NULL OR json_array_length(options) > 0)
         );
         CREATE INDEX IF NOT EXISTS approvals_project_status ON approvals (project_path, status);
         CREATE UNIQUE INDEX IF NOT EXISTS approvals_open_heading
           ON approvals (project_path, heading) WHERE status = 'open';",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalOption {
    pub label: String,
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
#[serde(deny_unknown_fields)]
pub struct Link {
    pub kind: LinkKind,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub heading: String,
    pub summary: String,
    pub tradeoffs: String,
    pub options: Vec<ApprovalOption>,
    pub urgency: Urgency,
    pub links: Vec<Link>,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    pub decided_at: i64,
    pub note: Option<String>,
    pub chosen: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Open,
    Withdrawn,
    Decided(Decision),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub id: String,
    pub project_path: String,
    pub card: Card,
    pub requested_by: Option<MonoId>,
    pub created_at: i64,
    pub status: Status,
}

fn check_text(field: &str, value: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    let length = value.chars().count();
    require(
        (1..=max).contains(&length),
        &format!("{field} must be 1 to {max} characters"),
    )?;
    Ok(value.to_string())
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("plain data serializes")
}

type Named<'a> = Vec<(&'a str, &'a dyn ToSql)>;

impl Card {
    fn into_columns(self) -> Result<[(&'static str, Sql); 7], String> {
        let options = self
            .options
            .into_iter()
            .map(|option| {
                let label = check_text("option label", &option.label, LABEL_MAX)?;
                Ok(ApprovalOption { label, ..option })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let labels: HashSet<&str> = options.iter().map(|option| option.label.as_str()).collect();
        require(labels.len() == options.len(), "option labels must differ")?;
        let https = self
            .links
            .iter()
            .all(|link| link.url.starts_with("https://"));
        require(https, "links must be https URLs")?;
        let origin = self
            .origin
            .map(|origin| check_text("origin", &origin, LABEL_MAX));
        Ok([
            (
                ":heading",
                check_text("heading", &self.heading, HEADING_MAX)?.into(),
            ),
            (
                ":summary",
                check_text("summary", &self.summary, TEXT_MAX)?.into(),
            ),
            (
                ":tradeoffs",
                check_text("tradeoffs", &self.tradeoffs, TEXT_MAX)?.into(),
            ),
            (":options", to_json(&options).into()),
            (":urgency", self.urgency.as_str().to_string().into()),
            (":links", to_json(&self.links).into()),
            (":origin", origin.transpose()?.into()),
        ])
    }
}

fn named<'a>(columns: &'a [(&'static str, Sql)], more: Named<'a>) -> Named<'a> {
    let card = columns
        .iter()
        .map(|(name, value)| (*name, value as &dyn ToSql));
    more.into_iter().chain(card).collect()
}

fn bad_column(
    index: usize,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
}

fn from_json<T: DeserializeOwned>(index: usize, text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|e| bad_column(index, e))
}

const COLUMNS: &str = "id, project_path, heading, summary, tradeoffs, options, urgency, links,
  origin, requested_by, created_at, status, decided_at, decision_note, chosen_options";

fn row_to_approval(row: &Row<'_>) -> rusqlite::Result<Approval> {
    let status: String = row.get(11)?;
    let status = match (status.as_str(), row.get(12)?) {
        ("open", None) => Status::Open,
        ("withdrawn", None) => Status::Withdrawn,
        (verdict, Some(decided_at)) => Status::Decided(Decision {
            verdict: Verdict::parse(verdict)
                .ok_or_else(|| bad_column(11, "invalid approvals.status"))?,
            decided_at,
            note: row.get(13)?,
            chosen: match row.get::<_, Option<String>>(14)? {
                Some(chosen) => from_json(14, &chosen)?,
                None => Vec::new(),
            },
        }),
        _ => return Err(bad_column(11, "invalid approvals.status")),
    };
    let urgency: String = row.get(6)?;
    let requested_by: Option<String> = row.get(9)?;
    Ok(Approval {
        id: row.get(0)?,
        project_path: row.get(1)?,
        card: Card {
            heading: row.get(2)?,
            summary: row.get(3)?,
            tradeoffs: row.get(4)?,
            options: from_json(5, &row.get::<_, String>(5)?)?,
            urgency: Urgency::parse(&urgency)
                .ok_or_else(|| bad_column(6, "invalid approvals.urgency"))?,
            links: from_json(7, &row.get::<_, String>(7)?)?,
            origin: row.get(8)?,
        },
        requested_by: requested_by
            .map(|id| {
                MonoId::parse(&id).ok_or_else(|| bad_column(9, "invalid approvals.requested_by"))
            })
            .transpose()?,
        created_at: row.get(10)?,
        status,
    })
}

pub(crate) fn get(conn: &Connection, id: &str) -> Result<Approval, String> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM approvals WHERE id = ?1"),
        [id],
        row_to_approval,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| UNKNOWN.to_string())
}

pub(crate) fn list(conn: &Connection, project: &str) -> Result<Vec<Approval>, String> {
    check_project(project)?;
    conn.prepare(&format!(
        "SELECT {COLUMNS} FROM approvals WHERE project_path = ?1
         ORDER BY status <> 'open', created_at DESC, id"
    ))
    .and_then(|mut statement| statement.query_map([project], row_to_approval)?.collect())
    .map_err(|e| e.to_string())
}

pub(crate) fn count_open(conn: &Connection, project: &str) -> Result<u32, String> {
    check_project(project)?;
    conn.query_row(
        "SELECT COUNT(*) FROM approvals WHERE project_path = ?1 AND status = 'open'",
        [project],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Inserts a card, or rewrites the open card `by` filed with the same project
/// and heading. Another Giga's open card with that heading is `heading_taken`.
pub(crate) fn file(
    conn: &Connection,
    project: &str,
    by: &MonoId,
    card: Card,
    now: i64,
) -> Result<Approval, String> {
    check_project(project)?;
    let columns = card.into_columns()?;
    let id = Uuid::new_v4().to_string();
    let filed: Option<String> = conn
        .query_row(
            "INSERT INTO approvals (id, project_path, heading, summary, tradeoffs, options,
               urgency, links, origin, requested_by, created_at, status)
             VALUES (:id, :project, :heading, :summary, :tradeoffs, :options,
               :urgency, :links, :origin, :by, :now, 'open')
             ON CONFLICT (project_path, heading) WHERE status = 'open' DO UPDATE SET
               summary = excluded.summary, tradeoffs = excluded.tradeoffs,
               options = excluded.options, urgency = excluded.urgency,
               links = excluded.links, origin = excluded.origin
             WHERE approvals.requested_by = excluded.requested_by
             RETURNING id",
            &*named(
                &columns,
                vec![
                    (":id", &id),
                    (":project", &project),
                    (":by", &by.as_str()),
                    (":now", &now),
                ],
            ),
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    get(conn, &filed.ok_or(HEADING_TAKEN)?)
}

fn check_filed_by_and_open(conn: &Connection, id: &str, by: &MonoId) -> Result<(), String> {
    let approval = get(conn, id)?;
    if approval.requested_by.as_ref() != Some(by) {
        return Err(NOT_YOURS.into());
    }
    if approval.status != Status::Open {
        return Err(NOT_OPEN.into());
    }
    Ok(())
}

pub(crate) fn update(
    conn: &Connection,
    id: &str,
    by: &MonoId,
    card: Card,
) -> Result<Approval, String> {
    let columns = card.into_columns()?;
    check_filed_by_and_open(conn, id, by)?;
    conn.execute(
        "UPDATE approvals SET heading = :heading, summary = :summary, tradeoffs = :tradeoffs,
           options = :options, urgency = :urgency, links = :links, origin = :origin
         WHERE id = :id",
        &*named(&columns, vec![(":id", &id)]),
    )
    .map_err(|e| match e.sqlite_error() {
        Some(error) if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => {
            HEADING_TAKEN.to_string()
        }
        _ => e.to_string(),
    })?;
    get(conn, id)
}

pub(crate) fn withdraw(conn: &Connection, id: &str, by: &MonoId) -> Result<Approval, String> {
    check_filed_by_and_open(conn, id, by)?;
    conn.execute(
        "UPDATE approvals SET status = 'withdrawn' WHERE id = ?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    get(conn, id)
}

pub(crate) fn decide(
    conn: &Connection,
    id: &str,
    verdict: Verdict,
    note: Option<String>,
    chosen: &[String],
    now: i64,
) -> Result<Approval, String> {
    let card = get(conn, id)?.card;
    let labels: HashSet<&str> = card
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    let distinct: HashSet<&str> = chosen.iter().map(String::as_str).collect();
    require(
        distinct.len() == chosen.len() && distinct.is_subset(&labels),
        "chosen options must be distinct labels of the card's options",
    )?;
    let note = note
        .map(|note| check_text("decision note", &note, TEXT_MAX))
        .transpose()?;
    let decided = conn
        .execute(
            "UPDATE approvals SET status = ?2, decided_at = ?3, decision_note = ?4,
               chosen_options = ?5
             WHERE id = ?1 AND status = 'open'",
            params![
                id,
                verdict.as_str(),
                now,
                note,
                (!chosen.is_empty()).then(|| to_json(&chosen))
            ],
        )
        .map_err(|e| e.to_string())?;
    if decided == 0 {
        return Err(NOT_OPEN.into());
    }
    get(conn, id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::session_store::SessionStore;

    const PROJECT: &str = "/Users/test/orbit";
    const OTHER: &str = "/Users/test/other";

    fn giga(conn: &Connection, id: &str) -> MonoId {
        conn.execute(
            "INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
             VALUES (?1, ?1, 'giga', '{}', 0, 0, 0)",
            [id],
        )
        .unwrap();
        MonoId::parse(id).unwrap()
    }

    fn card(heading: &str) -> Card {
        Card {
            heading: heading.into(),
            summary: "Pick where approval cards live.".into(),
            tradeoffs: "SQLite is local and fast; a server would sync windows.".into(),
            options: Vec::new(),
            urgency: Urgency::Blocking,
            links: Vec::new(),
            origin: None,
        }
    }

    fn with_options(heading: &str) -> Card {
        let option = |label: &str, recommended| ApprovalOption {
            label: label.into(),
            recommended,
        };
        Card {
            options: vec![option("SQLite", true), option("Server", false)],
            urgency: Urgency::NotBlocking,
            links: vec![Link {
                kind: LinkKind::Pr,
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
                    Status::Withdrawn => "withdrawn",
                    Status::Decided(decision) => decision.verdict.as_str(),
                };
                (approval.id, status)
            })
            .collect()
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
        let (cards, after_filing) = {
            let store = SessionStore::open(path.clone()).unwrap();
            let conn = store.lock_conn().unwrap();
            let first_giga: Option<String> = conn
                .query_row(
                    "SELECT id FROM monos WHERE role = 'giga' ORDER BY sort_order, id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            let filer = match first_giga {
                Some(id) => MonoId::parse(&id).unwrap(),
                None => giga(&conn, "giga-a"),
            };
            file(&conn, PROJECT, &filer, card("Ship the board"), 10).unwrap();
            let picked = file(&conn, PROJECT, &filer, with_options("Pick a store"), 11).unwrap();
            let chosen = ["SQLite".to_string()];
            decide(
                &conn,
                &picked.id,
                Verdict::Approved,
                Some("Go".into()),
                &chosen,
                12,
            )
            .unwrap();
            (list(&conn, PROJECT).unwrap(), snapshot(&conn))
        };
        assert_eq!(cards.len(), 2);
        for open in ["second", "third"] {
            let store = SessionStore::open(path.clone()).unwrap();
            let conn = store.lock_conn().unwrap();
            assert_eq!(
                list(&conn, PROJECT).unwrap(),
                cards,
                "the {open} open keeps every card"
            );
            assert_eq!(count_open(&conn, PROJECT).unwrap(), 1);
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

    // Defects: a second filing makes a duplicate, rewrites created_at, or
    // reopens a decided or withdrawn card.
    #[test]
    fn filing_an_open_heading_again_updates_that_card_and_a_closed_one_starts_anew() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let first = file(&conn, PROJECT, &a, card("Ship it"), 1).unwrap();
        let resend = Card {
            heading: "  Ship it ".into(),
            summary: "Now with numbers.".into(),
            ..with_options("unused")
        };
        let again = file(&conn, PROJECT, &a, resend, 2).unwrap();
        assert_eq!(again.id, first.id, "the open card is updated");
        assert_eq!(
            again.card,
            Card {
                heading: "Ship it".into(),
                summary: "Now with numbers.".into(),
                ..with_options("unused")
            }
        );
        assert_eq!(again.created_at, 1);
        assert_eq!(list(&conn, PROJECT).unwrap(), vec![again]);
        let elsewhere = file(&conn, OTHER, &a, card("Ship it"), 3).unwrap();
        assert_ne!(elsewhere.id, first.id, "another project gets its own card");

        decide(&conn, &first.id, Verdict::Approved, None, &[], 4).unwrap();
        let after_decide = file(&conn, PROJECT, &a, card("Ship it"), 5).unwrap();
        withdraw(&conn, &after_decide.id, &a).unwrap();
        let after_withdraw = file(&conn, PROJECT, &a, card("Ship it"), 6).unwrap();
        assert_eq!(
            statuses(&conn, PROJECT),
            [
                (after_withdraw.id, "open"),
                (after_decide.id, "withdrawn"),
                (first.id, "approved")
            ]
        );
    }

    // Defects: another Giga's call changes, withdraws or takes over the card,
    // or the filer can still change it once it is closed.
    #[test]
    fn another_giga_cannot_update_withdraw_or_take_over_a_card() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let b = giga(&conn, "giga-b");
        let mine = file(&conn, PROJECT, &a, card("Ship it"), 1).unwrap();
        let theirs = Card {
            summary: "Taken over.".into(),
            ..card("Ship it")
        };
        assert_eq!(
            update(&conn, &mine.id, &b, theirs.clone()),
            Err(NOT_YOURS.into())
        );
        assert_eq!(withdraw(&conn, &mine.id, &b), Err(NOT_YOURS.into()));
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
        assert_eq!(renamed.requested_by, Some(a.clone()));
        assert_eq!(
            withdraw(&conn, &mine.id, &a).unwrap().status,
            Status::Withdrawn
        );
        assert_eq!(withdraw(&conn, &mine.id, &a), Err(NOT_OPEN.into()));
        assert_eq!(
            update(&conn, &mine.id, &a, card("Again")),
            Err(NOT_OPEN.into())
        );
    }

    // Defects: a second decide overwrites the first, a withdrawn card can be
    // decided, or a chosen option is not one of the card's.
    #[test]
    fn decide_closes_an_open_card_once() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let id = file(&conn, PROJECT, &a, with_options("Pick a store"), 1)
            .unwrap()
            .id;
        for chosen in [vec!["Cloud"], vec!["SQLite", "SQLite"]] {
            let chosen: Vec<String> = chosen.into_iter().map(String::from).collect();
            let error = decide(&conn, &id, Verdict::Approved, None, &chosen, 2).unwrap_err();
            assert!(error.starts_with("invalid_input"), "{chosen:?}: {error}");
        }
        assert_eq!(get(&conn, &id).unwrap().status, Status::Open);

        let chosen = ["Server".to_string(), "SQLite".to_string()];
        let decided = decide(
            &conn,
            &id,
            Verdict::ChangesRequested,
            Some(" Add costs ".into()),
            &chosen,
            5,
        )
        .unwrap();
        assert_eq!(
            decided.status,
            Status::Decided(Decision {
                verdict: Verdict::ChangesRequested,
                decided_at: 5,
                note: Some("Add costs".into()),
                chosen: vec!["Server".into(), "SQLite".into()],
            })
        );
        assert_eq!(
            decided.requested_by,
            Some(a.clone()),
            "delivery reads the filer from decide"
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

        let gone = file(&conn, PROJECT, &a, card("Old"), 7).unwrap().id;
        withdraw(&conn, &gone, &a).unwrap();
        assert_eq!(
            decide(&conn, &gone, Verdict::Approved, None, &[], 8),
            Err(NOT_OPEN.into())
        );
        assert_eq!(
            decide(&conn, "missing", Verdict::Approved, None, &[], 8),
            Err(UNKNOWN.into())
        );
    }

    // Defects: the board's list mixes open cards among closed ones, or the
    // badge counts closed cards or another project's.
    #[test]
    fn list_puts_open_cards_first_and_count_open_counts_them() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let old_open = file(&conn, PROJECT, &a, card("Old open"), 1).unwrap().id;
        let decided = file(&conn, PROJECT, &a, card("Decided"), 2).unwrap().id;
        decide(&conn, &decided, Verdict::Rejected, None, &[], 3).unwrap();
        let new_open = file(&conn, PROJECT, &a, card("New open"), 4).unwrap().id;
        let withdrawn = file(&conn, PROJECT, &a, card("Withdrawn"), 5).unwrap().id;
        withdraw(&conn, &withdrawn, &a).unwrap();
        file(&conn, OTHER, &a, card("Elsewhere"), 6).unwrap();
        assert_eq!(
            statuses(&conn, PROJECT),
            [
                (new_open, "open"),
                (old_open, "open"),
                (withdrawn, "withdrawn"),
                (decided, "rejected")
            ]
        );
        assert_eq!(count_open(&conn, PROJECT).unwrap(), 2);
        assert_eq!(count_open(&conn, OTHER).unwrap(), 1);
        assert_eq!(count_open(&conn, "/Users/test/empty").unwrap(), 0);
    }

    // Defect: deleting the filing Giga deletes its cards, fails, or leaves an
    // id that delivery would follow.
    #[test]
    fn a_deleted_giga_leaves_its_cards_with_no_filer() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let mine = file(&conn, PROJECT, &a, card("Ship it"), 1).unwrap();
        assert!(crate::mono_roster::delete(&conn, a.as_str()).unwrap());
        let orphan = get(&conn, &mine.id).unwrap();
        assert_eq!((orphan.requested_by, orphan.card), (None, mine.card));
        let decided = decide(&conn, &mine.id, Verdict::Approved, None, &[], 2).unwrap();
        assert_eq!(decided.requested_by, None);
    }

    // Defect: a CHECK is missing, so the table holds a row `row_to_approval`
    // cannot turn into a `Status`.
    #[test]
    fn checks_refuse_rows_the_types_cannot_hold() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let options = r#"[{"label":"A","recommended":false}]"#;
        let insert = |status: &str,
                      decided_at: Option<i64>,
                      note: Option<&str>,
                      options: &str,
                      chosen: Option<&str>,
                      urgency: &str| {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO approvals (id, project_path, heading, summary, tradeoffs, options,
                   urgency, links, created_at, status, decided_at, decision_note, chosen_options)
                 VALUES (?1, '/p', ?1, 's', 't', ?2, ?3, '[]', 1, ?4, ?5, ?6, ?7)",
                params![id, options, urgency, status, decided_at, note, chosen],
            )
        };
        insert("open", None, None, "[]", None, "blocking").unwrap();
        insert(
            "approved",
            Some(1),
            Some("n"),
            options,
            Some(r#"["A"]"#),
            "not_blocking",
        )
        .unwrap();
        for (case, refused) in [
            (
                "decided_at on an open card",
                insert("open", Some(1), None, "[]", None, "blocking"),
            ),
            (
                "a note on a withdrawn card",
                insert("withdrawn", None, Some("n"), "[]", None, "blocking"),
            ),
            (
                "chosen options on an open card",
                insert("open", None, None, options, Some(r#"["A"]"#), "blocking"),
            ),
            (
                "approved with no decided_at",
                insert("approved", None, None, "[]", None, "blocking"),
            ),
            (
                "chosen options with no options",
                insert(
                    "approved",
                    Some(1),
                    None,
                    "[]",
                    Some(r#"["A"]"#),
                    "blocking",
                ),
            ),
            (
                "an empty chosen list",
                insert("approved", Some(1), None, options, Some("[]"), "blocking"),
            ),
            (
                "options that are not a list",
                insert("open", None, None, "{}", None, "blocking"),
            ),
            (
                "an unknown urgency",
                insert("open", None, None, "[]", None, "urgent"),
            ),
            (
                "an unknown status",
                insert("closed", Some(1), None, "[]", None, "blocking"),
            ),
        ] {
            let error = refused.expect_err(case).to_string();
            assert!(error.contains("CHECK constraint failed"), "{case}: {error}");
        }
    }

    // Defects: a check in `Card::into_columns` is missing, so bad input is
    // stored or fails as a raw CHECK error instead of `invalid_input`.
    #[test]
    fn bad_input_is_invalid_input_and_writes_nothing() {
        let store = SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        let a = giga(&conn, "giga-a");
        let mut same_labels = with_options("Same labels");
        same_labels.options[1].label = " SQLite ".into();
        let mut blank_label = with_options("Blank label");
        blank_label.options[0].label = " ".into();
        let mut http = with_options("Plain http");
        http.links[0].url = "http://example.com".into();
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
            (
                "blank tradeoffs",
                Card {
                    tradeoffs: String::new(),
                    ..card("Blank tradeoffs")
                },
            ),
            (
                "blank origin",
                Card {
                    origin: Some(" ".into()),
                    ..card("Blank origin")
                },
            ),
            ("same labels", same_labels),
            ("blank label", blank_label),
            ("http link", http),
        ];
        for (case, bad) in cases {
            let error = file(&conn, PROJECT, &a, bad, 1).unwrap_err();
            assert!(error.starts_with("invalid_input"), "{case}: {error}");
        }
        let error = file(&conn, "/Users/test/orbit/", &a, card("Trailing slash"), 1).unwrap_err();
        assert!(error.starts_with("invalid_input"), "{error}");
        assert_eq!(list(&conn, PROJECT).unwrap(), []);
    }
}
