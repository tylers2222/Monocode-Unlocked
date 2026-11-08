//! Design sections 2, 8 and 9: the episode and send log, the settings, and
//! the detector pass that turns each agent's rows into `Facts`, runs
//! `next_state` and writes what it says in one transaction.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::activity::{self, Activity};
use super::provisioner::{self, Provisioner};
use super::state::{
    self, Cause, Child, CloseReason, Episode, Facts, OpenWork, Thresholds, Transition,
};
use crate::ask_gate::Actor;
use crate::mono_messages::{self, MonoId, Notices, SessionId};

const MINUTE_MS: i64 = 60_000;

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

fn agent_stall_minutes(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT stall_minutes FROM agent_alarm_settings WHERE actor = ?1",
        [actor.to_string()],
        |row| row.get(0),
    )
    .optional()
}

/// Each Mono's current chat, so a pass resolves every session once.
fn mono_chats(conn: &Connection) -> rusqlite::Result<HashMap<String, MonoId>> {
    let rows = conn
        .prepare("SELECT id FROM monos")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut chats = HashMap::new();
    for mono in rows.iter().filter_map(|id| MonoId::parse(id)) {
        if let Some(chat) = mono_messages::mono_row(conn, &mono)?.and_then(|row| row.chat) {
            chats.insert(chat.as_str().to_string(), mono);
        }
    }
    Ok(chats)
}

/// The session was archived or deleted. A Mono's current chat is a session
/// row too.
fn gone(conn: &Connection, session: &SessionId) -> rusqlite::Result<bool> {
    let archived: Option<bool> = conn
        .query_row(
            "SELECT archived FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(archived.unwrap_or(true))
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

/// The agent's own open work (section 4) that its rows hold: its open asks
/// and an unfinished todo list. A child's result comes from its children.
fn own_open_work(
    conn: &Connection,
    actor: &Actor,
    row: &Activity,
) -> rusqlite::Result<Vec<OpenWork>> {
    let asks = strings(
        conn,
        "SELECT id FROM mono_asks WHERE child_actor = ?1 AND state = 'open' ORDER BY id",
        [actor.to_string()],
    )?;
    let mut work: Vec<OpenWork> = asks.into_iter().map(|id| OpenWork::Ask { id }).collect();
    if row.todo_open {
        work.push(OpenWork::Todo);
    }
    Ok(work)
}

/// The sessions and Monos `actor` started or gave an open ask to.
fn child_actors(conn: &Connection, actor: &Actor) -> rusqlite::Result<Vec<String>> {
    let mut children = strings(
        conn,
        "SELECT child_actor FROM mono_asks WHERE parent_actor = ?1 AND state = 'open'",
        [actor.to_string()],
    )?;
    let started = match actor {
        Actor::Session(session) => strings(
            conn,
            "SELECT id FROM sessions WHERE lead_session_id = ?1 AND archived = 0",
            [session.as_str()],
        )?,
        Actor::Mono(mono) => strings(
            conn,
            "SELECT DISTINCT to_session_id FROM mono_messages
              WHERE tool = 'start_session' AND status <> 'rejected'
                AND from_mono_id = ?1 AND to_session_id IS NOT NULL",
            [mono.as_str()],
        )?,
    };
    children.extend(
        started
            .into_iter()
            .map(|session| format!("session:{session}")),
    );
    children.sort();
    children.dedup();
    Ok(children)
}

/// Whether a wake for the agent waits for its provider (section 4): a
/// message no provider took, or an outbox reminder or alarm, or a Mono's
/// completion result not yet in its chat.
fn wake_pending(conn: &Connection, actor: &Actor, chat: &SessionId) -> rusqlite::Result<bool> {
    // `mono_messages::unqueued`'s rows, plus those a slot holds untaken. A
    // blocked question travels as an outbox row, not as its message.
    let message: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_messages m
           WHERE m.handled_by_session = ?1 AND m.status IN ('open', 'delivered')
             AND m.delivered_at IS NULL AND m.tool IN ('send_message', 'reply')
             AND NOT EXISTS (SELECT 1 FROM mono_ask_events e WHERE e.message_id = m.id
                              AND e.kind = 'question' AND e.blocked = 1))",
        [chat.as_str()],
        |row| row.get(0),
    )?;
    if message || crate::ask_scheduler::waits(conn, chat)? {
        return Ok(true);
    }
    let claimed: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_outbox
           WHERE recipient IN (?1, ?2) AND sent_at IS NULL AND claimed_by IS NOT NULL)",
        params![actor.to_string(), Actor::Session(chat.clone()).to_string()],
        |row| row.get(0),
    )?;
    if claimed {
        return Ok(true);
    }
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mono_completions
           WHERE json_extract(saved_json, '$.origin.monoId') = ?1)",
        [chat.as_str()],
        |row| row.get(0),
    )
}

fn open_episode(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<(String, Episode)>> {
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT id, cause, stalled_since_active_ms FROM alarm_episodes
              WHERE actor = ?1 AND closed_active_ms IS NULL",
            [actor.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, cause, stalled_since)| {
        Cause::parse(&cause).map(|cause| {
            (
                id,
                Episode {
                    cause,
                    stalled_since,
                },
            )
        })
    }))
}

fn new_episode_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("al_{}", &id[..12])
}

/// The key of the outbox row that tells level `level` of `episode`.
pub(crate) fn outbox_key(episode: &str, level: i64) -> String {
    format!("alarm:{episode}:{level}")
}

/// One pass: its transaction, settings and time, and the chats it names.
struct Pass<'a> {
    tx: &'a Connection,
    settings: Settings,
    active: i64,
    wall: i64,
    notices: Notices,
}

impl Pass<'_> {
    /// Opens the agent's episode and its level-1 send, unless one is open
    /// (the partial unique index): a second pass writes nothing.
    fn open(
        &mut self,
        actor: &Actor,
        facts: &Facts,
        row: &Activity,
        (cause, stalled_since): (Cause, i64),
    ) -> rusqlite::Result<()> {
        let tx = self.tx;
        let id = new_episode_id();
        let to = provisioner::provisioner(provisioner::lineage(
            tx,
            actor,
            self.settings.root_giga.as_deref(),
        )?);
        let open_work = serde_json::to_string(&state::open_work(facts)).unwrap_or_default();
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO alarm_episodes (id, actor, mono_id, provisioner, cause,
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
                open_work,
                (cause == Cause::Dialog)
                    .then_some(row.dialog_text.as_deref())
                    .flatten(),
                stalled_since,
                self.wall - (self.active - stalled_since),
                self.active,
                self.wall,
                self.active + self.settings.escalate_minutes * MINUTE_MS,
            ],
        )? == 1;
        if !inserted {
            return Ok(());
        }
        let key = match &to {
            Provisioner::Agent(_) => Some(outbox_key(&id, 1)),
            Provisioner::Tyler => None,
        };
        tx.execute(
            "INSERT INTO alarm_sends (episode_id, level, recipient, outbox_key, created_active_ms)
             VALUES (?1, 1, ?2, ?3, ?4)",
            params![id, to.stored(), key, self.active],
        )?;
        if let (Provisioner::Agent(recipient), Some(key)) = (&to, key) {
            let recipient = recipient.to_string();
            tx.execute(
                "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
                 VALUES (?1, NULL, 'alarm', ?2, ?3) ON CONFLICT (key) DO NOTHING",
                params![key, recipient, self.active],
            )?;
            crate::ask_scheduler::name_chat(tx, &mut self.notices, &recipient)?;
        }
        Ok(())
    }

    /// Closes episode `id`; the alarms it had not sent yet go with it.
    fn close(&self, id: &str, reason: CloseReason) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE alarm_episodes SET closed_active_ms = ?2, closed_wall_ms = ?3, close_reason = ?4
              WHERE id = ?1 AND closed_active_ms IS NULL",
            params![id, self.active, self.wall, reason.as_str()],
        )?;
        self.tx.execute(
            "DELETE FROM mono_outbox WHERE sent_at IS NULL
               AND key IN (SELECT outbox_key FROM alarm_sends WHERE episode_id = ?1)",
            [id],
        )?;
        Ok(())
    }

    /// Runs `next_state` on one agent and writes what it says.
    fn agent(
        &mut self,
        row: &Activity,
        actor: &Actor,
        turns: &HashMap<String, state::Turn>,
    ) -> rusqlite::Result<()> {
        let tx = self.tx;
        let gone = gone(tx, &row.session)?;
        let episode = open_episode(tx, actor)?;
        if gone && episode.is_none() {
            return activity::forget(tx, &row.session);
        }
        let pending = wake_pending(tx, actor, &row.session)?;
        let wake_pending_since = match (pending, row.wake_pending_since) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(self.active),
            (false, _) => None,
        };
        if wake_pending_since != row.wake_pending_since {
            activity::set_wake_pending(tx, &row.session, wake_pending_since)?;
        }
        let children = child_actors(tx, actor)?
            .into_iter()
            .filter_map(|child| {
                let turn = *turns.get(&child)?;
                Some(Child { actor: child, turn })
            })
            .collect();
        let facts = Facts {
            gone,
            turn: row.turn,
            dialog: row.dialog,
            open_work: own_open_work(tx, actor, row)?,
            children,
            wake_pending_since,
            episode: episode.as_ref().map(|(_, episode)| *episode),
        };
        let limits = self.settings.thresholds(agent_stall_minutes(tx, actor)?);
        match (state::next_state(&facts, self.active, limits), &episode) {
            (Transition::Stay, _) | (Transition::Close(_), None) => Ok(()),
            (
                Transition::Open {
                    cause,
                    stalled_since,
                },
                _,
            ) => self.open(actor, &facts, row, (cause, stalled_since)),
            (Transition::Close(reason), Some((id, _))) => self.close(id, reason),
        }
    }
}

/// One detector pass at active time `active_ms`. Every effect is a
/// function of the rows, so a second pass, or one after a restart, writes
/// nothing new. Names the chat of each provisioner told.
pub(crate) fn run(conn: &Connection, active_ms: i64, wall_ms: i64) -> rusqlite::Result<Notices> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let chats = mono_chats(&tx)?;
    let rows = activity::all(&tx)?;
    let actor_of = |session: &SessionId| match chats.get(session.as_str()) {
        Some(mono) => Actor::Mono(mono.clone()),
        None => Actor::Session(session.clone()),
    };
    let turns: HashMap<String, state::Turn> = rows
        .iter()
        .map(|row| (actor_of(&row.session).to_string(), row.turn))
        .collect();
    let mut pass = Pass {
        tx: &tx,
        settings: Settings::load(&tx)?,
        active: active_ms,
        wall: wall_ms,
        notices: Notices::default(),
    };
    for row in &rows {
        pass.agent(row, &actor_of(&row.session), &turns)?;
    }
    let notices = pass.notices;
    tx.commit()?;
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;

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
