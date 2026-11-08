//! `sleep_holds` (alarms design 8): one row per held foreground wait, keyed
//! by the session and the harness's tool call id, so a retried request
//! finds its hold. The row is the log: who was asked, who decided what and
//! when, or that no one answered by the deadline.

use rusqlite::{params, Connection, OptionalExtension};

/// How long the provisioner has to answer, in active time (design 3).
pub(crate) const ANSWER_MS: i64 = super::ANSWER_CUT.as_millis() as i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    Deny,
    /// No answer by the deadline.
    Timeout,
    /// The settings do not hold this agent's waits: allowed at once.
    Skipped,
}

impl Decision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
            Decision::Timeout => "timeout",
            Decision::Skipped => "skipped",
        }
    }

    fn parse(stored: &str) -> Option<Self> {
        match stored {
            "allow" => Some(Decision::Allow),
            "deny" => Some(Decision::Deny),
            "timeout" => Some(Decision::Timeout),
            "skipped" => Some(Decision::Skipped),
            _ => None,
        }
    }
}

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sleep_holds (
           id                  TEXT PRIMARY KEY CHECK (id GLOB 'sh_?*'),
           actor               TEXT NOT NULL,
           session_id          TEXT NOT NULL,
           provisioner         TEXT NOT NULL,
           harness             TEXT NOT NULL CHECK (harness IN ('claude', 'codex', 'omp')),
           tool_use_id         TEXT NOT NULL,
           command             TEXT NOT NULL,
           wait_kind           TEXT NOT NULL,
           wait_seconds        REAL,
           reason              TEXT,
           prompt              TEXT,
           requested_active_ms INTEGER NOT NULL,
           requested_wall_ms   INTEGER NOT NULL,
           deadline_active_ms  INTEGER NOT NULL,
           decision            TEXT CHECK (decision IN ('allow', 'deny', 'timeout', 'skipped')),
           decided_by          TEXT,
           decided_active_ms   INTEGER,
           decided_wall_ms     INTEGER,
           decision_reason     TEXT,
           decided_request_id  TEXT,
           notified_wall_ms    INTEGER,
           withdrawn_wall_ms   INTEGER,
           UNIQUE (session_id, tool_use_id),
           CHECK ((decision IS NULL) = (decided_active_ms IS NULL))
         );
         CREATE INDEX IF NOT EXISTS sleep_holds_open ON sleep_holds (deadline_active_ms)
           WHERE decision IS NULL;",
    )
}

/// The outbox key of the hold's wake to its provisioner. It reaches the
/// front end as a request id, which keeps only `[A-Za-z0-9_-]`.
pub(crate) fn outbox_key(id: &str) -> String {
    format!("hold-{id}")
}

pub(crate) fn new_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("sh_{}", &id[..12])
}

/// A hold as a request writes it.
pub(crate) struct NewHold<'a> {
    pub id: &'a str,
    pub actor: &'a str,
    pub session: &'a str,
    pub provisioner: &'a str,
    pub harness: &'a str,
    pub tool_use_id: &'a str,
    pub command: &'a str,
    pub wait_kind: &'a str,
    pub wait_seconds: Option<f64>,
    pub reason: Option<&'a str>,
    /// The `<mono_sleep_hold>` text, for a provisioner that is an agent.
    pub prompt: Option<&'a str>,
    /// Decided at once: `skipped`, with why.
    pub skipped: Option<&'a str>,
}

/// Writes `hold` once per (session, tool call). Returns the id of the
/// session's hold for that call: the new one, or the one written before.
pub(crate) fn insert(
    tx: &Connection,
    hold: &NewHold,
    (active, wall): (i64, i64),
) -> rusqlite::Result<(String, bool)> {
    let decision = hold.skipped.map(|_| Decision::Skipped.as_str());
    let inserted = tx.execute(
        "INSERT INTO sleep_holds (id, actor, session_id, provisioner, harness, tool_use_id,
           command, wait_kind, wait_seconds, reason, prompt, requested_active_ms,
           requested_wall_ms, deadline_active_ms, decision, decided_active_ms, decided_wall_ms,
           decision_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
           CASE WHEN ?15 IS NULL THEN NULL ELSE ?12 END,
           CASE WHEN ?15 IS NULL THEN NULL ELSE ?13 END, ?16)
         ON CONFLICT (session_id, tool_use_id) DO NOTHING",
        params![
            hold.id,
            hold.actor,
            hold.session,
            hold.provisioner,
            hold.harness,
            hold.tool_use_id,
            hold.command,
            hold.wait_kind,
            hold.wait_seconds,
            hold.reason,
            hold.prompt,
            active,
            wall,
            active + ANSWER_MS,
            decision,
            hold.skipped,
        ],
    )? == 1;
    let id = tx.query_row(
        "SELECT id FROM sleep_holds WHERE session_id = ?1 AND tool_use_id = ?2",
        params![hold.session, hold.tool_use_id],
        |row| row.get(0),
    )?;
    Ok((id, inserted))
}

/// The hold's wake to `recipient`: a stage `hold` outbox row, which the
/// outbox reads with the hold's prompt while it waits for a decision.
pub(crate) fn enqueue(
    tx: &Connection,
    id: &str,
    recipient: &str,
    active: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
         VALUES (?1, NULL, 'hold', ?2, ?3) ON CONFLICT (key) DO NOTHING",
        params![outbox_key(id), recipient, active],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HoldRow {
    pub id: String,
    pub actor: String,
    pub session: String,
    pub provisioner: String,
    pub command: String,
    pub wait_kind: String,
    pub wait_seconds: Option<f64>,
    pub reason: Option<String>,
    pub deadline_active: i64,
    pub decision: Option<Decision>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_request_id: Option<String>,
}

const HOLD_COLUMNS: &str = "id, actor, session_id, provisioner, command, wait_kind,
  wait_seconds, reason, deadline_active_ms, decision, decided_by, decision_reason,
  decided_request_id";

fn hold_row(row: &rusqlite::Row) -> rusqlite::Result<HoldRow> {
    Ok(HoldRow {
        id: row.get(0)?,
        actor: row.get(1)?,
        session: row.get(2)?,
        provisioner: row.get(3)?,
        command: row.get(4)?,
        wait_kind: row.get(5)?,
        wait_seconds: row.get(6)?,
        reason: row.get(7)?,
        deadline_active: row.get(8)?,
        decision: row
            .get::<_, Option<String>>(9)?
            .as_deref()
            .and_then(Decision::parse),
        decided_by: row.get(10)?,
        decision_reason: row.get(11)?,
        decided_request_id: row.get(12)?,
    })
}

pub(crate) fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<HoldRow>> {
    conn.query_row(
        &format!("SELECT {HOLD_COLUMNS} FROM sleep_holds WHERE id = ?1"),
        [id],
        hold_row,
    )
    .optional()
}

/// Decision `decision` of hold `id`, unless the hold is decided already:
/// the first decision wins. Its wake goes if no window took it yet.
pub(crate) fn decide(
    tx: &Connection,
    id: &str,
    decision: Decision,
    by: &str,
    reason: Option<&str>,
    request_id: Option<&str>,
    (active, wall): (i64, i64),
) -> rusqlite::Result<bool> {
    let decided = tx.execute(
        "UPDATE sleep_holds SET decision = ?2, decided_by = ?3, decision_reason = ?4,
           decided_request_id = ?5, decided_active_ms = ?6, decided_wall_ms = ?7
         WHERE id = ?1 AND decision IS NULL",
        params![id, decision.as_str(), by, reason, request_id, active, wall],
    )? == 1;
    if decided {
        drop_wake(tx, id)?;
    }
    Ok(decided)
}

fn drop_wake(tx: &Connection, id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM mono_outbox WHERE key = ?1 AND sent_at IS NULL",
        [outbox_key(id)],
    )?;
    Ok(())
}

/// Every hold past its deadline with no decision is `timeout`, so the log
/// is right even when the client died. Returns how many it closed.
pub(crate) fn expire(tx: &Connection, (active, wall): (i64, i64)) -> rusqlite::Result<usize> {
    let due = tx
        .prepare("SELECT id FROM sleep_holds WHERE decision IS NULL AND deadline_active_ms <= ?1")?
        .query_map([active], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &due {
        time_out(tx, id, (active, wall))?;
    }
    Ok(due.len())
}

/// Hold `id` is `timeout` unless it is decided already. `false` when a
/// decision came first.
pub(crate) fn time_out(
    tx: &Connection,
    id: &str,
    (active, wall): (i64, i64),
) -> rusqlite::Result<bool> {
    let closed = tx.execute(
        "UPDATE sleep_holds SET decision = 'timeout', decided_active_ms = ?2,
           decided_wall_ms = ?3, decision_reason = 'no answer in 2 min'
         WHERE id = ?1 AND decision IS NULL",
        params![id, active, wall],
    )? == 1;
    if closed {
        drop_wake(tx, id)?;
    }
    Ok(closed)
}

/// The hold client of `session` reported in: its hook is installed.
/// `false` when the session has no activity row to mark.
pub(crate) fn ping(conn: &Connection, session: &str, wall: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE agent_activity SET hold_state = 'installed', hold_missing_reason = NULL,
           updated_wall_ms = ?2
         WHERE session_id = ?1",
        params![session, wall],
    )? == 1)
}

/// Tyler's holds waiting for an answer that no banner has shown yet.
pub(crate) fn tyler_unshown(conn: &Connection) -> rusqlite::Result<Vec<HoldRow>> {
    conn.prepare(&format!(
        "SELECT {HOLD_COLUMNS} FROM sleep_holds
          WHERE provisioner = 'tyler' AND decision IS NULL AND notified_wall_ms IS NULL
          ORDER BY requested_active_ms, id"
    ))?
    .query_map([], hold_row)?
    .collect()
}

/// Claims hold `id`'s banner for this pass: `false` when another pass or
/// process claimed it first, so each banner goes out once.
pub(crate) fn claim_shown(conn: &Connection, id: &str, wall: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE sleep_holds SET notified_wall_ms = ?2 WHERE id = ?1 AND notified_wall_ms IS NULL",
        params![id, wall],
    )? == 1)
}

/// Shown holds that are decided or expired and whose banner is still up.
pub(crate) fn tyler_settled(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare(
        "SELECT id FROM sleep_holds
          WHERE notified_wall_ms IS NOT NULL AND decision IS NOT NULL
            AND withdrawn_wall_ms IS NULL
          ORDER BY id",
    )?
    .query_map([], |row| row.get(0))?
    .collect()
}

/// Claims the withdrawal of hold `id`'s banner, as [`claim_shown`].
pub(crate) fn claim_withdrawn(conn: &Connection, id: &str, wall: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE sleep_holds SET withdrawn_wall_ms = ?2 WHERE id = ?1 AND withdrawn_wall_ms IS NULL",
        params![id, wall],
    )? == 1)
}
