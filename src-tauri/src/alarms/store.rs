//! Design sections 2, 8 and 9: the episode and send log, the settings, and
//! the detector pass that turns each agent's rows into `Facts`, runs
//! `next_state` and writes what it says in one transaction.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::activity::{self, Activity};
use super::live::{Live, Taken, Write};
use super::provisioner;
use super::state::{
    self, Cause, CloseReason, Episode, Facts, OpenWork, Thresholds, Transition, Wake,
};
use super::tree::{self, Edge, Edges};
use crate::ask_gate::Actor;
use crate::mono_messages::{self, MonoId, Notices, SessionId};

pub(super) const MINUTE_MS: i64 = 60_000;

/// The per-agent stall threshold's bounds (section 9).
pub(crate) const STALL_MINUTES: std::ops::RangeInclusive<i64> = 5..=240;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alarm_episodes (
           id                      TEXT PRIMARY KEY CHECK (id GLOB 'al_?*'),
           actor                   TEXT NOT NULL CHECK (actor GLOB 'mono:?*' OR actor GLOB 'session:?*'),
           mono_id                 TEXT,
           provisioner             TEXT NOT NULL,
           cause                   TEXT NOT NULL CHECK (cause IN
                                     ('idle_open_work', 'dialog', 'hung', 'wake_missed', 'hold_missing')),
           open_work_json          TEXT NOT NULL,
           last_activity           TEXT,
           dialog_text             TEXT,
           stalled_since_active_ms INTEGER NOT NULL,
           stalled_since_wall_ms   INTEGER NOT NULL,
           opened_active_ms        INTEGER NOT NULL,
           opened_wall_ms          INTEGER NOT NULL,
           level                   INTEGER NOT NULL DEFAULT 1 CHECK (level >= 1),
           next_escalate_active_ms INTEGER,
           acted_by                TEXT,
           acted_how               TEXT,
           acted_active_ms         INTEGER,
           closed_active_ms        INTEGER,
           closed_wall_ms          INTEGER,
           close_reason            TEXT CHECK (close_reason IN
                                     ('progress', 'work_cleared', 'agent_gone', 'act_no_effect')),
           CHECK ((closed_active_ms IS NULL) = (close_reason IS NULL))
         );
         CREATE UNIQUE INDEX IF NOT EXISTS alarm_episodes_open
           ON alarm_episodes (actor) WHERE closed_active_ms IS NULL;
         CREATE INDEX IF NOT EXISTS alarm_episodes_stalled
           ON alarm_episodes (stalled_since_wall_ms);
         CREATE INDEX IF NOT EXISTS alarm_episodes_actor
           ON alarm_episodes (actor, closed_active_ms);
         CREATE TABLE IF NOT EXISTS alarm_sends (
           episode_id        TEXT NOT NULL REFERENCES alarm_episodes(id),
           level             INTEGER NOT NULL CHECK (level >= 1),
           recipient         TEXT NOT NULL,
           outbox_key        TEXT UNIQUE,
           created_active_ms INTEGER NOT NULL,
           sent_wall_ms      INTEGER,
           PRIMARY KEY (episode_id, level),
           CHECK ((recipient = 'tyler') = (outbox_key IS NULL))
         );
         CREATE TABLE IF NOT EXISTS alarm_settings (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS agent_alarm_settings (
           actor         TEXT PRIMARY KEY,
           stall_minutes INTEGER NOT NULL CHECK (stall_minutes BETWEEN 5 AND 240)
         );",
    )
}

/// The indexes the pass's queries on other modules' tables use, so a pass
/// reads the few rows that wait, not every sent or answered one. After
/// those tables, and after a rebuild of any of them.
pub(crate) fn ensure_indexes(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS mono_downtime_active ON mono_downtime (active_ms);
         CREATE INDEX IF NOT EXISTS mono_outbox_unsent
           ON mono_outbox (recipient) WHERE sent_at IS NULL;
         CREATE INDEX IF NOT EXISTS mono_messages_untaken
           ON mono_messages (handled_by_session)
           WHERE delivered_at IS NULL AND status IN ('open', 'delivered');
         CREATE INDEX IF NOT EXISTS mono_messages_started
           ON mono_messages (to_session_id, from_mono_id)
           WHERE tool = 'start_session' AND status <> 'rejected';",
    )
}

/// Section 9's global settings. A row in `alarm_settings` overrides the
/// default; a value that does not parse keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub stall_minutes: i64,
    pub dialog_minutes: i64,
    pub wake_minutes: i64,
    pub escalate_minutes: i64,
    pub sleep_hold: bool,
    /// Off until row U ships an answer surface for Tyler.
    pub tyler_sleep_hold: bool,
    pub root_giga: Option<String>,
    /// A single foreground sleep under this many seconds runs unasked
    /// (section 12, Tyler 2026-11-08).
    pub short_sleep_skip_s: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stall_minutes: 15,
            dialog_minutes: 3,
            wake_minutes: 2,
            escalate_minutes: 15,
            sleep_hold: true,
            tyler_sleep_hold: false,
            root_giga: None,
            short_sleep_skip_s: 10,
        }
    }
}

fn minutes(value: &str, valid: std::ops::RangeInclusive<i64>) -> Option<i64> {
    value.parse().ok().filter(|minutes| valid.contains(minutes))
}

fn switch(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub(crate) fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let rows = conn
            .prepare("SELECT key, value FROM alarm_settings")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut settings = Settings::default();
        for (key, value) in rows {
            let value = value.as_str();
            match key.as_str() {
                "stall_minutes" => {
                    settings.stall_minutes =
                        minutes(value, STALL_MINUTES).unwrap_or(settings.stall_minutes);
                }
                "dialog_minutes" => {
                    settings.dialog_minutes =
                        minutes(value, 1..=240).unwrap_or(settings.dialog_minutes);
                }
                "wake_minutes" => {
                    settings.wake_minutes =
                        minutes(value, 1..=240).unwrap_or(settings.wake_minutes);
                }
                "escalate_minutes" => {
                    settings.escalate_minutes =
                        minutes(value, 1..=240).unwrap_or(settings.escalate_minutes);
                }
                "sleep_hold" => settings.sleep_hold = switch(value).unwrap_or(settings.sleep_hold),
                "tyler_sleep_hold" => {
                    settings.tyler_sleep_hold = switch(value).unwrap_or(settings.tyler_sleep_hold);
                }
                "root_giga" => settings.root_giga = Some(value.to_string()),
                "short_sleep_skip_s" => {
                    settings.short_sleep_skip_s = value
                        .parse()
                        .ok()
                        .filter(|s: &i64| *s >= 0)
                        .unwrap_or(settings.short_sleep_skip_s);
                }
                _ => {}
            }
        }
        Ok(settings)
    }

    fn thresholds(&self, stall_minutes: Option<i64>) -> Thresholds {
        Thresholds {
            stall_ms: stall_minutes.unwrap_or(self.stall_minutes) * MINUTE_MS,
            dialog_ms: self.dialog_minutes * MINUTE_MS,
            wake_ms: self.wake_minutes * MINUTE_MS,
        }
    }
}

/// What one pass reads: one query per fact type (lead decision M3/F3), not
/// one per agent.
struct Rows {
    activity: Vec<Activity>,
    /// Each Mono's current chat session, and the Mono.
    chats: HashMap<String, MonoId>,
    /// Each watched session that has a row, and whether it is archived.
    archived: HashMap<String, bool>,
    /// The open asks: (id, parent actor, child actor).
    asks: Vec<(String, String, String)>,
    /// Whom each agent started or asked.
    edges: Edges,
    /// The watched sessions whose queue the user paused: nothing waits for
    /// their provider (review F1).
    paused: HashSet<String>,
    /// The sessions a message waits for that no provider took.
    untaken: HashSet<String>,
    /// The recipients an outbox reminder waits for (not the alarms' own rows).
    waiting: HashSet<String>,
    /// Mono completion results not yet in a chat, by chat session.
    notices: HashMap<String, Vec<String>>,
    /// The items each watched session's saved queue holds, but the alarms'.
    queued: HashMap<String, Vec<String>>,
    episodes: HashMap<String, (String, Episode)>,
    stall_minutes: HashMap<String, i64>,
}

pub(crate) fn agent_stall_minutes(
    conn: &Connection,
    actor: &Actor,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT stall_minutes FROM agent_alarm_settings WHERE actor = ?1",
        [actor.to_string()],
        |row| row.get(0),
    )
    .optional()
}

fn strings(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare(sql)?
        .query_map(params, |row| row.get(0))?
        .collect()
}

fn pairs(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare(sql)?
        .query_map(params, |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn grouped(rows: Vec<(String, String)>) -> HashMap<String, Vec<String>> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in rows {
        groups.entry(key).or_default().push(value);
    }
    groups
}

/// The parent -> child edges: open asks, and the leads and Monos that
/// started a watched session (one with no row runs no turn here).
fn edges(conn: &Connection, asks: &[(String, String, String)]) -> rusqlite::Result<Edges> {
    let started = pairs(
        conn,
        "SELECT 'session:' || s.lead_session_id, 'session:' || s.id
           FROM agent_activity a JOIN sessions s ON s.id = a.session_id
          WHERE s.lead_session_id IS NOT NULL AND s.archived = 0
         UNION
         SELECT 'mono:' || m.from_mono_id, 'session:' || m.to_session_id
           FROM agent_activity a JOIN mono_messages m ON m.to_session_id = a.session_id
          WHERE m.tool = 'start_session' AND m.status <> 'rejected'
            AND m.from_mono_id IS NOT NULL",
        [],
    )?;
    let asked = asks
        .iter()
        .map(|(_, parent, child)| (parent.clone(), child.clone()));
    let mut edges = Edges::new();
    for ((parent, child), started) in started
        .into_iter()
        .map(|edge| (edge, true))
        .chain(asked.map(|edge| (edge, false)))
    {
        let children = edges.entry(parent).or_default();
        let edge = Edge { child, started };
        if !children.contains(&edge) {
            children.push(edge);
        }
    }
    Ok(edges)
}

/// Each Mono's completion results still owed, by the id its episode lists.
/// The window writes a row at the hand-off, with only the request, and adds the
/// result when the turn ends (`SavedMonoCompletion`, ADE-270). A bare row is owed
/// too once its session stopped: the watcher lives only in a window's memory, so
/// nothing here can read it, and a live window can hold a row whose watcher is
/// gone (a refused Continue). A live watcher writes the result as the turn ends,
/// well inside the wake threshold. A session with no turn yet, or that still
/// holds the request in its queue, has not stopped this request (ADE-271).
fn owed_results(conn: &Connection) -> rusqlite::Result<HashMap<String, Vec<String>>> {
    Ok(grouped(pairs(
        conn,
        "SELECT json_extract(c.saved_json, '$.origin.monoId'),
                CASE json_type(c.saved_json, '$.notice') WHEN 'text'
                  THEN json_extract(c.saved_json, '$.notice') ELSE c.request_id END
           FROM mono_completions c
           LEFT JOIN agent_activity a
             ON a.session_id = json_extract(c.saved_json, '$.request.sessionId')
          WHERE json_valid(c.saved_json) AND json_type(c.saved_json, '$.origin.monoId') = 'text'
            AND (json_type(c.saved_json, '$.result') = 'object'
              OR (a.turn_state = 'idle' AND a.turn_started_active_ms IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM sessions s, json_each(s.queued_messages_json) q
                     WHERE s.id = a.session_id AND json_valid(s.queued_messages_json)
                       AND json_extract(q.value, '$.appRequestId') = c.request_id)))",
        [],
    )?))
}

impl Rows {
    fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let chats = mono_messages::mono_chats(conn)?
            .into_iter()
            .map(|(mono, chat)| (chat.as_str().to_string(), mono))
            .collect();
        let archived = conn
            .prepare(
                "SELECT a.session_id, COALESCE(s.archived, 1) FROM agent_activity a
                   LEFT JOIN sessions s ON s.id = a.session_id",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let asks: Vec<(String, String, String)> = conn
            .prepare("SELECT id, parent_actor, child_actor FROM mono_asks WHERE state = 'open' ORDER BY id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let edges = edges(conn, &asks)?;
        let paused = strings(
            conn,
            "SELECT s.id FROM agent_activity a JOIN sessions s ON s.id = a.session_id
              WHERE s.queue_status = 'paused'",
            [],
        )?
        .into_iter()
        .collect();
        // `mono_messages::unqueued`'s rows, plus those a slot holds untaken.
        // A blocked question travels as an outbox row, not as its message.
        let untaken = strings(
            conn,
            "SELECT DISTINCT m.handled_by_session FROM mono_messages m
              WHERE m.delivered_at IS NULL AND m.status IN ('open', 'delivered')
                AND m.handled_by_session IS NOT NULL AND m.tool IN ('send_message', 'reply')
                AND NOT EXISTS (SELECT 1 FROM mono_ask_events e WHERE e.message_id = m.id
                                 AND e.kind = 'question' AND e.blocked = 1)",
            [],
        )?
        .into_iter()
        .collect();
        let notices = owed_results(conn)?;
        let queued = grouped(pairs(
            conn,
            &format!(
                "SELECT s.id, json_extract(q.value, '$.id')
                   FROM agent_activity a JOIN sessions s ON s.id = a.session_id,
                        json_each(s.queued_messages_json) q
                  WHERE s.queued_messages_json <> '[]' AND json_valid(s.queued_messages_json)
                    AND json_type(q.value, '$.id') = 'text' AND NOT {}",
                crate::ask_scheduler::alarm_key_sql("json_extract(q.value, '$.id')")
            ),
            [],
        )?);
        let episodes = conn
            .prepare(
                "SELECT actor, id, cause, stalled_since_active_ms FROM alarm_episodes
                  WHERE closed_active_ms IS NULL",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(actor, id, cause, stalled_since)| {
                let episode = Episode {
                    cause: Cause::parse(&cause),
                    stalled_since,
                };
                (actor, (id, episode))
            })
            .collect();
        let stall_minutes = conn
            .prepare("SELECT actor, stall_minutes FROM agent_alarm_settings")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            activity: activity::all(conn)?,
            chats,
            archived,
            asks,
            edges,
            paused,
            untaken,
            waiting: crate::ask_scheduler::waiting_recipients(conn)?,
            notices,
            queued,
            episodes,
            stall_minutes,
        })
    }

    fn actor_of(&self, session: &SessionId) -> Actor {
        match self.chats.get(session.as_str()) {
            Some(mono) => Actor::Mono(mono.clone()),
            None => Actor::Session(session.clone()),
        }
    }
}

/// The notice and queue items earlier episodes of `actors` already
/// listed: each counts toward one episode only (lead N5).
fn counted(conn: &Connection, actors: &[String]) -> rusqlite::Result<HashSet<(String, String)>> {
    if actors.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(pairs(
        conn,
        "SELECT e.actor, json_extract(w.value, '$.id')
           FROM alarm_episodes e, json_each(e.open_work_json) w
          WHERE e.actor IN (SELECT value FROM json_each(?1))
            AND e.closed_active_ms IS NOT NULL AND json_valid(e.open_work_json)
            AND json_extract(w.value, '$.kind') IN ('notice', 'queued')",
        [serde_json::to_string(actors).unwrap_or_default()],
    )?
    .into_iter()
    .collect())
}

/// What an episode lists: its open work, then what waits for a provider.
#[derive(serde::Serialize)]
#[serde(untagged)]
enum Listed<'a> {
    Work(&'a OpenWork),
    Wake(&'a Wake),
}

fn new_episode_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("al_{}", &id[..12])
}

/// The key of the outbox row that tells level `level` of `episode`. The
/// front end keeps a request id only in `[A-Za-z0-9_-]+`, so no `:` here.
pub(crate) fn outbox_key(episode: &str, level: i64) -> String {
    format!("alarm-{episode}-{level}")
}

/// One pass: its transaction, settings and time, and the chats it names.
struct Pass<'a> {
    tx: &'a Connection,
    rows: &'a Rows,
    counted: HashSet<(String, String)>,
    settings: Settings,
    active: i64,
    wall: i64,
    notices: Notices,
}

impl Pass<'_> {
    /// Opens the agent's episode and its level-1 send. The pass read no
    /// open episode for it in this transaction, so the insert is the one.
    fn open(
        &mut self,
        actor: &Actor,
        facts: &Facts,
        row: &Activity,
        wakes: &[Wake],
        (cause, stalled_since): (Cause, i64),
    ) -> rusqlite::Result<()> {
        let tx = self.tx;
        let id = new_episode_id();
        let to = provisioner::provisioner(provisioner::lineage(
            tx,
            actor,
            self.settings.root_giga.as_deref(),
        )?);
        let work = state::open_work(facts);
        let listed: Vec<Listed> = work
            .iter()
            .map(Listed::Work)
            .chain(wakes.iter().map(Listed::Wake))
            .collect();
        tx.execute(
            "INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause,
               open_work_json, dialog_text, stalled_since_active_ms, stalled_since_wall_ms,
               opened_active_ms, opened_wall_ms, level, next_escalate_active_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12)",
            params![
                id,
                actor.to_string(),
                provisioner::owner_mono(tx, actor)?
                    .as_ref()
                    .map(MonoId::as_str),
                to.stored(),
                cause.as_str(),
                serde_json::to_string(&listed).unwrap_or_default(),
                (cause == Cause::Dialog)
                    .then_some(row.dialog_text.as_deref())
                    .flatten(),
                stalled_since,
                crate::active_clock::wall_at(tx, stalled_since, (self.active, self.wall))?,
                self.active,
                self.wall,
                self.active + self.settings.escalate_minutes * MINUTE_MS,
            ],
        )?;
        super::route::opened(tx, &mut self.notices, &id, &self.settings, self.active)
    }

    /// Closes episode `id`; the alarms, nudges and steers it had not sent yet
    /// go with it.
    fn close(&self, id: &str, reason: CloseReason) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE alarm_episodes SET closed_active_ms = ?2, closed_wall_ms = ?3, close_reason = ?4
              WHERE id = ?1 AND closed_active_ms IS NULL",
            params![id, self.active, self.wall, reason.as_str()],
        )?;
        super::route::drop_unsent(self.tx, id)
    }

    /// What waits for the agent's provider (section 4): a message no
    /// provider took, an outbox reminder, a completion result not yet in
    /// its live chat, or an item its saved queue holds (review F4); none of
    /// the alarms' own rows (lead ruling S1 r3 a).
    /// A notice or queue item an earlier episode listed counts no more.
    fn wakes(&self, actor: &Actor, session: &SessionId, gone: bool) -> Vec<Wake> {
        let rows = self.rows;
        if rows.paused.contains(session.as_str()) {
            return Vec::new();
        }
        let own = Actor::Session(session.clone()).to_string();
        let actor_text = actor.to_string();
        let message = rows
            .untaken
            .contains(session.as_str())
            .then_some(Wake::Message);
        let reminder = (rows.waiting.contains(&actor_text) || rows.waiting.contains(&own))
            .then_some(Wake::Reminder);
        let fresh = |id: &&String| !self.counted.contains(&(actor_text.clone(), (*id).clone()));
        // A result counts only for a Mono chat that is live (lead N5).
        let live_chat = !gone && rows.chats.contains_key(session.as_str());
        let notices = rows
            .notices
            .get(session.as_str())
            .filter(|_| live_chat)
            .into_iter()
            .flatten()
            .filter(fresh)
            .map(|id| Wake::Notice { id: id.clone() });
        let queued = rows
            .queued
            .get(session.as_str())
            .into_iter()
            .flatten()
            .filter(fresh)
            .map(|id| Wake::Queued { id: id.clone() });
        message
            .into_iter()
            .chain(reminder)
            .chain(notices)
            .chain(queued)
            .collect()
    }

    /// The agent's own open work (section 4): its open asks and an
    /// unfinished todo list. A child's result comes from its children.
    fn own_open_work(&self, actor: &str, row: &Activity) -> Vec<OpenWork> {
        let asks = self
            .rows
            .asks
            .iter()
            .filter(|(_, _, child)| child == actor)
            .map(|(id, _, _)| OpenWork::Ask { id: id.clone() });
        asks.chain(row.todo_open.then_some(OpenWork::Todo))
            .collect()
    }

    fn limits(&self, actor: &str) -> Thresholds {
        self.settings
            .thresholds(self.rows.stall_minutes.get(actor).copied())
    }

    /// The agent's facts before `tree` judges its children.
    fn facts(&self, row: &Activity, actor: &Actor) -> Facts {
        let actor_text = actor.to_string();
        let gone = self
            .rows
            .archived
            .get(row.session.as_str())
            .copied()
            .unwrap_or(true);
        let wakes = self.wakes(actor, &row.session, gone);
        let wake_pending_since = match (wakes.is_empty(), row.wake_pending_since) {
            (false, Some(since)) => Some(since),
            (false, None) => Some(self.active),
            (true, _) => None,
        };
        Facts {
            gone,
            turn: row.turn,
            dialog: row.dialog,
            background: row.background,
            open_work: self.own_open_work(&actor_text, row),
            children: Vec::new(),
            settled_at: None,
            wake_pending_since,
            episode: self
                .rows
                .episodes
                .get(&actor_text)
                .map(|(_, episode)| episode.clone()),
        }
    }

    /// Runs `next_state` on one agent's judged facts and writes what it says.
    fn agent(
        &mut self,
        row: &Activity,
        actor: &Actor,
        facts: &Facts,
        limits: Thresholds,
    ) -> rusqlite::Result<()> {
        let tx = self.tx;
        let actor_text = actor.to_string();
        let episode = self.rows.episodes.get(&actor_text);
        if facts.gone && episode.is_none() {
            return activity::forget(tx, &row.session);
        }
        if facts.wake_pending_since != row.wake_pending_since {
            activity::set_wake_pending(tx, &row.session, facts.wake_pending_since)?;
        }
        match (state::next_state(facts, self.active, limits), episode) {
            (Transition::Stay, _) | (Transition::Close(_), None) => Ok(()),
            (
                Transition::Open {
                    cause,
                    stalled_since,
                },
                _,
            ) => {
                let wakes = self.wakes(actor, &row.session, facts.gone);
                self.open(actor, facts, row, &wakes, (cause, stalled_since))
            }
            (Transition::Close(reason), Some((id, _))) => self.close(id, reason),
        }
    }
}

/// The inputs of one pass beside the store: the taps' records, and the
/// turns `ControlHost` runs, read before the store lock (`None` when no
/// control host runs, so no row can be judged against it).
pub(crate) struct Sources<'a> {
    pub live: &'a Live,
    pub running: Option<&'a HashMap<String, String>>,
}

/// One detector pass at active time `active_ms`. Every effect is a
/// function of the rows, so a second pass, or one after a restart, writes
/// nothing new. Names the chat of each provisioner told. The taps' records
/// go back to the hub when the pass does not commit.
pub(crate) fn run(
    conn: &Connection,
    active_ms: i64,
    wall_ms: i64,
    sources: &Sources,
) -> rusqlite::Result<Notices> {
    let taken = sources.live.take();
    let passed = pass(conn, active_ms, wall_ms, &taken, sources);
    if passed.is_err() {
        sources.live.put_back(taken);
    }
    passed
}

fn pass(
    conn: &Connection,
    active_ms: i64,
    wall_ms: i64,
    taken: &Taken,
    sources: &Sources,
) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for (session, writes) in taken {
        for write in writes {
            activity::apply(&tx, session, write, (active_ms, wall_ms))?;
        }
    }
    if let Some(running) = sources.running {
        // A session with a turn record in this take is left to the next
        // pass: its start may be newer than the running set, or its end
        // the better time.
        let recorded: HashSet<&str> = taken
            .iter()
            .filter(|(_, writes)| {
                writes.iter().any(|write| {
                    matches!(write, Write::TurnStarted { .. } | Write::TurnEnded { .. })
                })
            })
            .map(|(session, _)| session.as_str())
            .collect();
        activity::reconcile(&tx, running, &recorded, active_ms, wall_ms)?;
    }
    let rows = Rows::load(&tx)?;
    let agents: Vec<(&Activity, Actor)> = rows
        .activity
        .iter()
        .map(|row| (row, rows.actor_of(&row.session)))
        .collect();
    let waiting_on: Vec<String> = agents
        .iter()
        .filter(|(row, _)| {
            rows.notices.contains_key(row.session.as_str())
                || rows.queued.contains_key(row.session.as_str())
        })
        .map(|(_, actor)| actor.to_string())
        .collect();
    let mut pass = Pass {
        tx: &tx,
        rows: &rows,
        counted: counted(&tx, &waiting_on)?,
        settings: Settings::load(&tx)?,
        active: active_ms,
        wall: wall_ms,
        notices: Notices::default(),
    };
    let mut judged: Vec<tree::Agent> = agents
        .iter()
        .map(|(row, actor)| {
            let key = actor.to_string();
            let limits = pass.limits(&key);
            (key, pass.facts(row, actor), limits)
        })
        .collect();
    tree::judge(&mut judged, &rows.edges, active_ms);
    // One agent's fault must not silence the others' alarms.
    let mut fault = None;
    for ((row, actor), (_, facts, limits)) in agents.iter().zip(&judged) {
        tx.execute_batch("SAVEPOINT alarm_agent")?;
        match pass.agent(row, actor, facts, *limits) {
            Ok(()) => tx.execute_batch("RELEASE alarm_agent")?,
            Err(error) => {
                tx.execute_batch("ROLLBACK TO alarm_agent; RELEASE alarm_agent")?;
                fault.get_or_insert_with(|| format!("{actor}: {error}"));
            }
        }
    }
    // An episode whose agent no row resolves to any more (its Mono was
    // deleted or its chat reset) is gone.
    let visited: HashSet<String> = agents.iter().map(|(_, actor)| actor.to_string()).collect();
    for (actor, (id, _)) in &rows.episodes {
        if !visited.contains(actor) {
            pass.close(id, CloseReason::AgentGone)?;
        }
    }
    if sources.live.new_fault(fault.clone()) {
        eprintln!(
            "The stall detector skipped an agent: {}",
            fault.unwrap_or_default()
        );
    }
    super::route::run(&tx, &mut pass.notices, &pass.settings, (active_ms, wall_ms))?;
    let notices = pass.notices;
    tx.commit()?;
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Defect (U1 r2 M1): an alarm's outbox key has a character the front
    // end's sessionStore drops from an appRequestId (`^[A-Za-z0-9_-]+$`),
    // so the alarm card is gone after a reload.
    #[test]
    fn every_alarm_outbox_key_is_a_front_end_request_id() {
        for level in 1..=5 {
            let key = outbox_key(&new_episode_id(), level);
            assert!(
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
                "{key}"
            );
        }
    }

    // Defect: section 9's defaults drift, an override is ignored, a bad
    // value breaks the pass, or a per-agent N outside 5..240 is stored.
    #[test]
    fn settings_default_per_section_9_and_take_valid_overrides() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        let defaults = Settings::load(&conn).unwrap();
        assert_eq!(
            (
                defaults.stall_minutes,
                defaults.dialog_minutes,
                defaults.wake_minutes,
                defaults.escalate_minutes,
                defaults.sleep_hold,
                defaults.tyler_sleep_hold,
                defaults.root_giga.clone(),
                defaults.short_sleep_skip_s,
            ),
            (15, 3, 2, 15, true, false, None, 10)
        );
        conn.execute_batch(
            "INSERT INTO alarm_settings VALUES ('stall_minutes', '30'), ('dialog_minutes', 'soon'),
               ('root_giga', 'general'), ('sleep_hold', 'off')",
        )
        .unwrap();
        let set = Settings::load(&conn).unwrap();
        assert_eq!(
            (
                set.stall_minutes,
                set.dialog_minutes,
                set.root_giga.as_deref(),
                set.sleep_hold
            ),
            (30, 3, Some("general"), false)
        );
        for (minutes, stored) in [(4, false), (5, true), (240, true), (241, false)] {
            let written = conn.execute(
                "INSERT OR REPLACE INTO agent_alarm_settings VALUES ('session:a', ?1)",
                [minutes],
            );
            assert_eq!(written.is_ok(), stored, "{minutes} minutes");
        }
    }
}
