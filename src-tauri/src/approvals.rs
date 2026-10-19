use std::collections::{BTreeMap, HashSet};

use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension, Row, ToSql};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use crate::milestones::{check_project, require};
use crate::mono_messages::{now_ms, MonoId};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub(crate) const UNKNOWN: &str = "unknown_approval";
pub(crate) const NOT_YOURS: &str = "not_your_approval";
pub(crate) const NOT_OPEN: &str = "approval_not_open";
pub(crate) const HEADING_TAKEN: &str = "heading_taken";

const HEADING_MAX: usize = 200;
const TEXT_MAX: usize = 8000;
const LABEL_MAX: usize = 200;

fn check_text(field: &str, value: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    let length = value.chars().count();
    require(
        (1..=max).contains(&length),
        &format!("{field} must be 1 to {max} characters"),
    )?;
    Ok(value.to_string())
}

fn check_optional(field: &str, value: Option<String>) -> Result<Option<String>, String> {
    value
        .map(|value| check_text(field, &value, LABEL_MAX))
        .transpose()
}

fn check_each(field: &str, values: &[String]) -> Result<Vec<String>, String> {
    values
        .iter()
        .map(|value| check_text(field, value, TEXT_MAX))
        .collect()
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
                Ok(ApprovalOption {
                    label: check_text("option label", &option.label, LABEL_MAX)?,
                    description: check_optional("option description", option.description)?,
                    recommended: option.recommended,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let labels: HashSet<&str> = options.iter().map(|option| option.label.as_str()).collect();
        require(labels.len() == options.len(), "option labels must differ")?;
        let links = self
            .links
            .into_iter()
            .map(|link| {
                require(link.url.starts_with("https://"), "links must be https URLs")?;
                Ok(Link {
                    title: check_optional("link title", link.title)?,
                    ..link
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let tradeoffs = Tradeoffs {
            pros: check_each("pro", &self.tradeoffs.pros)?,
            cons: check_each("con", &self.tradeoffs.cons)?,
        };
        require(
            !(tradeoffs.pros.is_empty() && tradeoffs.cons.is_empty()),
            "tradeoffs need at least one pro or con",
        )?;
        Ok([
            (
                ":heading",
                check_text("heading", &self.heading, HEADING_MAX)?.into(),
            ),
            (
                ":summary",
                check_text("summary", &self.summary, TEXT_MAX)?.into(),
            ),
            (":tradeoffs", to_json(&tradeoffs).into()),
            (":options", to_json(&options).into()),
            (":urgency", self.urgency.as_str().to_string().into()),
            (":links", to_json(&links).into()),
            (":origin", check_optional("origin", self.origin)?.into()),
        ])
    }
}

fn named<'a>(columns: &'a [(&'static str, Sql)], more: Named<'a>) -> Named<'a> {
    let card = columns
        .iter()
        .map(|(name, value)| (*name, value as &dyn ToSql));
    more.into_iter().chain(card).collect()
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

/// Inserts a card, or rewrites the open card `by` filed with the same project
/// and heading. Another Giga's open card with that heading is `heading_taken`.
/// The card keeps a copy of the Giga's name for when the Giga is deleted.
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
               urgency, links, origin, requested_by, requested_by_name, created_at, status)
             VALUES (:id, :project, :heading, :summary, :tradeoffs, :options,
               :urgency, :links, :origin, :by, (SELECT name FROM monos WHERE id = :by),
               :now, 'open')
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
    if approval.requested_by.id.as_ref() != Some(by) {
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

pub(crate) fn withdraw(
    conn: &Connection,
    id: &str,
    by: &MonoId,
    now: i64,
) -> Result<Approval, String> {
    check_filed_by_and_open(conn, id, by)?;
    conn.execute(
        "UPDATE approvals SET status = 'withdrawn', withdrawn_at = ?2 WHERE id = ?1",
        params![id, now],
    )
    .map_err(|e| e.to_string())?;
    get(conn, id)
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

/// `by` is the calling Giga, which the web app takes from the caller and
/// never from the agent's input.
fn parse_by(by: &str) -> Result<MonoId, String> {
    MonoId::parse(by).ok_or_else(|| "invalid_input: by is not a mono id".into())
}

#[tauri::command(async)]
pub fn approvals_file(
    store: State<'_, SessionStore>,
    project_path: String,
    by: String,
    card: Card,
) -> Result<Approval, String> {
    file(
        &*store.lock_conn()?,
        &project_path,
        &parse_by(&by)?,
        card,
        now_ms(),
    )
}

#[tauri::command(async)]
pub fn approvals_update(
    store: State<'_, SessionStore>,
    id: String,
    by: String,
    card: Card,
) -> Result<Approval, String> {
    update(&*store.lock_conn()?, &id, &parse_by(&by)?, card)
}

#[tauri::command(async)]
pub fn approvals_withdraw(
    store: State<'_, SessionStore>,
    id: String,
    by: String,
) -> Result<Approval, String> {
    withdraw(&*store.lock_conn()?, &id, &parse_by(&by)?, now_ms())
}

#[cfg(test)]
mod tests {
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

    /// `MONOCODE_DB_COPY`: a fresh copy of the app's monocode.db.
    fn app_database_copy() -> PathBuf {
        PathBuf::from(std::env::var("MONOCODE_DB_COPY").unwrap())
    }

    #[test]
    #[ignore = "needs MONOCODE_DB_COPY, a fresh copy of the app's monocode.db"]
    fn cards_survive_reopen_on_a_copy_of_the_app_database() {
        cards_survive_reopen(app_database_copy());
    }

    /// The first Mono with a chat of `role` in the copy: id, name, data.
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

    // ADE-51 real-state proof on the app's own Giga and a real project.
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
}
