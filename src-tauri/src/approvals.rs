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
mod tests;
