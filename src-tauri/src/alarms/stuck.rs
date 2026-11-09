//! ADE-277: what an alarm says the agent is stuck on, read from the rows
//! for the text alone. `open_work_json` and the detector stay as they are.
//!
//! The stuck item, by cause:
//! - `wake_missed`: the oldest listed wake (message, reminder, notice, queued
//!   item) whose age is known, else the first listed.
//! - `idle_open_work`: the child result that ended last, else the ask the
//!   agent's last turn took, else its newest ask, else its todo list.
//! - `dialog`, `hung`: the running turn and, for a dialog, its dialog.
//! - `hold_missing`: the sleep hold.
//!
//! Open work is tied to the item by a row, never by time or by words:
//! - a message or queued message: the asks its reply chain reaches, and the
//!   ask whose event carries a message of that chain;
//! - a reminder or queued reminder: the ask its outbox row names;
//! - a notice: the listed result of the session its completion row names;
//! - a stuck ask: the listed asks taken in the same turn;
//! - a dialog or hung turn: the listed asks taken in that turn.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

use super::route::{self, EpisodeRow};
use super::state::Cause;
use super::store::MINUTE_MS;
use super::text::{Replied, Stuck, Work};
use crate::ask_gate::Actor;
use crate::mono_messages::SessionId;

/// One entry of `open_work_json`: `state::OpenWork` or `state::Wake`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Listed {
    Ask {
        id: String,
    },
    ChildResult {
        child: String,
    },
    Todo,
    Message,
    Reminder,
    Notice {
        id: String,
    },
    Queued {
        id: String,
    },
    /// A kind from a newer build: it still counts.
    #[serde(other)]
    Other,
}

impl Listed {
    fn word(&self) -> &'static str {
        match self {
            Listed::Ask { .. } => "ask",
            Listed::ChildResult { .. } => "child result",
            Listed::Todo => "todo list",
            Listed::Message => "message",
            Listed::Reminder => "reminder",
            Listed::Notice { .. } => "notice",
            Listed::Queued { .. } => "queued item",
            Listed::Other => "other item",
        }
    }
}

/// `None`: the column does not hold a list, so nothing can be said of it.
fn listed(json: &str) -> Option<Vec<Listed>> {
    let items: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    Some(
        items
            .into_iter()
            .map(|item| serde_json::from_value(item).unwrap_or(Listed::Other))
            .collect(),
    )
}

/// The stuck item and what is tied to it.
#[derive(Debug, Default)]
struct Item {
    what: String,
    /// Which entry of the list it is, so it is not counted among the others.
    entry: Option<usize>,
    /// Minutes since it began waiting, when a row says.
    age: Option<i64>,
    asks: Vec<String>,
    children: Vec<String>,
}

impl Item {
    fn named(what: String) -> Self {
        Self {
            what,
            ..Self::default()
        }
    }

    /// `what`, `age` and the words for the age.
    fn aged(what: &str, age: Option<i64>) -> Self {
        let words = age.map_or("age unknown".to_string(), |age| format!("{age} min old"));
        Self {
            what: format!("{what}, {words}"),
            age,
            ..Self::default()
        }
    }

    fn at(self, entry: usize) -> Self {
        Self {
            entry: Some(entry),
            ..self
        }
    }
}

/// The agent's `agent_activity` row.
struct Activity {
    running: bool,
    turn_id: Option<String>,
    started: Option<i64>,
    ended: i64,
    progress: Option<i64>,
    updated_wall: i64,
    dialog_kind: Option<String>,
    hold_reason: Option<String>,
}

impl Activity {
    fn read(conn: &Connection, session: &SessionId) -> rusqlite::Result<Option<Self>> {
        conn.query_row(
            "SELECT turn_state = 'running', turn_id, turn_started_active_ms, turn_ended_active_ms,
                    last_progress_active_ms, updated_wall_ms, dialog_kind, hold_missing_reason
               FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| {
                Ok(Self {
                    running: row.get(0)?,
                    turn_id: row.get(1)?,
                    started: row.get(2)?,
                    ended: row.get(3)?,
                    progress: row.get(4)?,
                    updated_wall: row.get(5)?,
                    dialog_kind: row.get(6)?,
                    hold_reason: row.get(7)?,
                })
            },
        )
        .optional()
    }

    /// The agent's last move on the active clock.
    fn moved(&self) -> i64 {
        self.ended
            .max(self.started.unwrap_or(self.ended))
            .max(self.progress.unwrap_or(self.ended))
    }
}

/// The agent's last delivered message: id, turn, whether it wants a reply,
/// when it was delivered, and its sender's Mono and session.
type LastMessage = (String, Option<String>, bool, i64, Option<String>, String);

/// A listed ask that is still open, as the rows hold it.
struct Ask {
    id: String,
    entry: usize,
    parent: String,
    created_wall: i64,
    /// The turn of the agent's session that took it.
    turn: Option<String>,
}

struct Reader<'a> {
    conn: &'a Connection,
    episode: &'a EpisodeRow,
    active: i64,
    /// Wall time now, from the stall's own pair, so a row's wall time and
    /// the alarm's active minutes agree.
    wall: i64,
    session: Option<SessionId>,
    activity: Option<Activity>,
}

/// What the alarm says of `episode`. Never fails: the stall must still alarm
/// when a row here cannot be read, so it says what is unknown instead.
pub(crate) fn read(conn: &Connection, episode: &EpisodeRow, active: i64) -> Stuck {
    gather(conn, episode, active).unwrap_or_else(|error| {
        eprintln!(
            "Could not read what alarm {} is stuck on: {error}",
            episode.id
        );
        let unread = "its rows could not be read";
        Stuck {
            item: format!("unknown, {unread}"),
            moved: "unknown".into(),
            replied: Replied::Unknown(unread.into()),
            work: None,
        }
    })
}

fn gather(conn: &Connection, episode: &EpisodeRow, active: i64) -> rusqlite::Result<Stuck> {
    let session = route::session_of(conn, &episode.actor)?;
    let activity = match &session {
        Some(session) => Activity::read(conn, session)?,
        None => None,
    };
    let reader = Reader {
        conn,
        episode,
        active,
        wall: episode.stalled_since_wall + active - episode.stalled_since_active,
        session,
        activity,
    };
    let entries = listed(&episode.open_work_json);
    let item = reader.item(entries.as_deref().unwrap_or_default())?;
    let work = match &entries {
        Some(entries) => Some(reader.work(entries, &item)?),
        None => None,
    };
    Ok(Stuck {
        moved: reader.moved()?,
        replied: reader.replied()?,
        item: item.what,
        work,
    })
}

impl Reader<'_> {
    fn minutes_since(&self, active: i64) -> i64 {
        ((self.active - active) / MINUTE_MS).max(0)
    }

    fn minutes_since_wall(&self, wall: i64) -> i64 {
        ((self.wall - wall) / MINUTE_MS).max(0)
    }

    fn label(&self, actor: &str) -> rusqlite::Result<String> {
        Ok(match Actor::parse(actor) {
            Some(actor) => route::who(self.conn, &actor)?.label,
            None => "unknown".into(),
        })
    }

    fn item(&self, entries: &[Listed]) -> rusqlite::Result<Item> {
        match Cause::parse(&self.episode.cause) {
            Ok(Cause::WakeMissed) => self.wake_item(entries),
            Ok(Cause::IdleOpenWork) => self.work_item(entries),
            Ok(Cause::Dialog) => self.dialog_item(entries),
            Ok(Cause::Hung) => self.hung_item(entries),
            Ok(Cause::HoldMissing) => Ok(self.hold_item()),
            Err(cause) => Ok(Item::named(format!(
                "unknown, this build does not read cause {}",
                cause.0
            ))),
        }
    }

    /// The oldest wake a row dates, else the first listed.
    fn wake_item(&self, entries: &[Listed]) -> rusqlite::Result<Item> {
        let mut oldest: Option<Item> = None;
        for (entry, listed) in entries.iter().enumerate() {
            let item = match listed {
                Listed::Message => self.message_wake()?,
                Listed::Reminder => self.reminder_wake()?,
                Listed::Notice { id } => self.notice_wake(id)?,
                Listed::Queued { id } => self.queued_wake(id)?,
                _ => continue,
            }
            .at(entry);
            if oldest.as_ref().is_none_or(|best| item.age > best.age) {
                oldest = Some(item);
            }
        }
        Ok(oldest.unwrap_or_else(|| Item::aged("a wake, which one unknown", None)))
    }

    /// The asks message `id`'s reply chain reaches: each message of it can be
    /// an ask, or an event of one.
    fn asks_of_message(&self, id: &str) -> rusqlite::Result<Vec<String>> {
        self.conn
            .prepare(
                "WITH RECURSIVE chain(id, hops) AS (
                   SELECT ?1, 0
                   UNION SELECT m.reply_to, c.hops + 1 FROM mono_messages m
                     JOIN chain c ON m.id = c.id WHERE m.reply_to IS NOT NULL AND c.hops < 8)
                 SELECT id FROM chain
                 UNION SELECT e.ask_id FROM mono_ask_events e JOIN chain c ON e.message_id = c.id",
            )?
            .query_map([id], |row| row.get(0))?
            .collect()
    }

    /// `kind` of message `id`, from whom, a little of its body, and what it
    /// ties to; `None` when no row holds it.
    fn message_item(
        &self,
        kind: &str,
        id: &str,
        age: Option<i64>,
    ) -> rusqlite::Result<Option<Item>> {
        let row: Option<(Option<String>, String, String)> = self
            .conn
            .query_row(
                "SELECT from_mono_id, from_session_id, body FROM mono_messages WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((mono, session, body)) = row else {
            return Ok(None);
        };
        let from = match mono {
            Some(mono) => format!("mono:{mono}"),
            None => format!("session:{session}"),
        };
        let what = format!(
            "{kind} {id} from {} \"{}\"",
            self.label(&from)?,
            route::short(&body, 80)
        );
        Ok(Some(Item {
            asks: self.asks_of_message(id)?,
            ..Item::aged(&what, age)
        }))
    }

    /// The message that has waited longest, by `store`'s `untaken` rule.
    fn message_wake(&self) -> rusqlite::Result<Item> {
        let session = self.session.as_ref().map(SessionId::as_str);
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT m.id, m.created_at FROM mono_messages m
                  WHERE m.delivered_at IS NULL AND m.status IN ('open', 'delivered')
                    AND m.handled_by_session = ?1 AND m.tool IN ('send_message', 'reply')
                    AND NOT EXISTS (SELECT 1 FROM mono_ask_events e WHERE e.message_id = m.id
                                     AND e.kind = 'question' AND e.blocked = 1)
                  ORDER BY m.created_at, m.id LIMIT 1",
                [session],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, created)) = row {
            let age = Some(self.minutes_since_wall(created));
            if let Some(item) = self.message_item("message", &id, age)? {
                return Ok(item);
            }
        }
        Ok(Item::aged("a message that no row holds now", None))
    }

    /// The reminder that has waited longest, by `waiting_recipients`' rule.
    fn reminder_wake(&self) -> rusqlite::Result<Item> {
        let own = self
            .session
            .as_ref()
            .map(|session| format!("session:{}", session.as_str()));
        let row: Option<(String, Option<String>, i64)> = self
            .conn
            .query_row(
                &format!(
                    "SELECT stage, ask_id, at_active_ms FROM mono_outbox
                      WHERE sent_at IS NULL AND recipient IN (?1, ?2)
                        AND (claimed_by IS NOT NULL OR ask_id IS NOT NULL) AND NOT {}
                      ORDER BY at_active_ms, key LIMIT 1",
                    crate::ask_scheduler::alarm_key_sql("key")
                ),
                params![self.episode.actor.to_string(), own],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((stage, ask, at)) = row else {
            return Ok(Item::aged("a reminder that no row holds now", None));
        };
        let what = match &ask {
            Some(ask) => format!("{stage} reminder of ask {ask}"),
            None => format!("{stage} reminder"),
        };
        Ok(Item {
            asks: ask.into_iter().collect(),
            ..Item::aged(&what, Some(self.minutes_since(at)))
        })
    }

    /// A completion row has no time of its own: its age is the end of its
    /// session's last turn, when the row of that session says.
    fn notice_wake(&self, id: &str) -> rusqlite::Result<Item> {
        let id_words = route::short(id, 40);
        let child: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT json_extract(saved_json, '$.request.sessionId') FROM mono_completions
                  WHERE json_valid(saved_json)
                    AND (request_id = ?1 OR json_extract(saved_json, '$.notice') = ?1)
                  ORDER BY request_id LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(child) = child.flatten() else {
            return Ok(Item::aged(
                &format!("notice {id_words} that no row holds now"),
                None,
            ));
        };
        let actor = format!("session:{child}");
        let ended: Option<i64> = self
            .conn
            .query_row(
                "SELECT turn_ended_active_ms FROM agent_activity
                  WHERE session_id = ?1 AND turn_state = 'idle'",
                [&child],
                |row| row.get(0),
            )
            .optional()?;
        let age = ended.map(|ended| self.minutes_since(ended));
        let words = age.map_or("age unknown".to_string(), |age| {
            format!("its session's last turn ended {age} min ago")
        });
        Ok(Item {
            what: format!(
                "notice {id_words}: the result of {} not in its chat, {words}",
                self.label(&actor)?
            ),
            age,
            children: vec![actor],
            ..Item::default()
        })
    }

    /// An item of the saved queue: its message, or the outbox row of its key.
    fn queued_wake(&self, id: &str) -> rusqlite::Result<Item> {
        let id_words = route::short(id, 40);
        let row: Option<(Option<i64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT json_extract(q.value, '$.queuedAt'), json_extract(q.value, '$.monoMessageId')
                   FROM sessions s, json_each(s.queued_messages_json) q
                  WHERE s.id = ?1 AND json_valid(s.queued_messages_json)
                    AND json_extract(q.value, '$.id') = ?2",
                params![self.session.as_ref().map(SessionId::as_str), id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((queued, message)) = row else {
            return Ok(Item::aged(
                &format!("queued item {id_words} that no row holds now"),
                None,
            ));
        };
        let age = queued.map(|queued| self.minutes_since_wall(queued));
        let message = message.unwrap_or_else(|| id.to_string());
        if let Some(item) = self.message_item("queued message", &message, age)? {
            return Ok(item);
        }
        let ask: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT ask_id FROM mono_outbox WHERE key = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(Item {
            asks: ask.flatten().into_iter().collect(),
            ..Item::aged(&format!("queued item {id_words}"), age)
        })
    }

    /// The listed asks still open, with the turn of this session that took
    /// each.
    fn open_asks(&self, entries: &[Listed]) -> rusqlite::Result<Vec<Ask>> {
        let mut asks = Vec::new();
        for (entry, listed) in entries.iter().enumerate() {
            let Listed::Ask { id } = listed else {
                continue;
            };
            let row = self
                .conn
                .query_row(
                    "SELECT a.parent_actor, a.created_wall_ms,
                            CASE WHEN m.handled_by_session = ?2 THEN m.turn_id END
                       FROM mono_asks a LEFT JOIN mono_messages m ON m.id = a.id
                      WHERE a.id = ?1 AND a.state = 'open'",
                    params![id, self.session.as_ref().map(SessionId::as_str)],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((parent, created_wall, turn)) = row {
                asks.push(Ask {
                    id: id.clone(),
                    entry,
                    parent,
                    created_wall,
                    turn,
                });
            }
        }
        Ok(asks)
    }

    fn last_turn(&self) -> Option<&str> {
        self.activity.as_ref()?.turn_id.as_deref()
    }

    fn work_item(&self, entries: &[Listed]) -> rusqlite::Result<Item> {
        if let Some(item) = self.child_item(entries)? {
            return Ok(item);
        }
        let asks = self.open_asks(entries)?;
        let last = self.last_turn();
        let stuck = asks.iter().max_by_key(|ask| {
            (
                ask.turn.is_some() && ask.turn.as_deref() == last,
                ask.created_wall,
            )
        });
        if let Some(ask) = stuck {
            let what = format!(
                "{}, from {}",
                route::ask_line(self.conn, &ask.id, self.active)?,
                self.label(&ask.parent)?
            );
            let same_turn = asks
                .iter()
                .filter(|other| other.turn.is_some() && other.turn == ask.turn)
                .map(|other| other.id.clone());
            return Ok(Item {
                asks: same_turn.collect(),
                ..Item::aged(&what, Some(self.minutes_since_wall(ask.created_wall))).at(ask.entry)
            });
        }
        let todo = entries.iter().position(|listed| *listed == Listed::Todo);
        Ok(match todo {
            Some(entry) => {
                let age = self.minutes_since(self.episode.stalled_since_active);
                Item::aged("its unfinished todo list", Some(age)).at(entry)
            }
            None => Item::aged("open work that no row holds now", None),
        })
    }

    /// The child result that came last: it set the stall's clock.
    fn child_item(&self, entries: &[Listed]) -> rusqlite::Result<Option<Item>> {
        let mut last: Option<(usize, &str, Option<i64>)> = None;
        for (entry, listed) in entries.iter().enumerate() {
            let Listed::ChildResult { child } = listed else {
                continue;
            };
            let ended = match Actor::parse(child)
                .map(|actor| route::session_of(self.conn, &actor))
                .transpose()?
                .flatten()
            {
                Some(session) => Activity::read(self.conn, &session)?.map(|row| row.ended),
                None => None,
            };
            if last.is_none_or(|(_, _, best)| ended > best) {
                last = Some((entry, child.as_str(), ended));
            }
        }
        let Some((entry, child, ended)) = last else {
            return Ok(None);
        };
        let line = route::child_line(self.conn, child)?;
        let words = ended.map_or("age unknown".to_string(), |ended| {
            format!("its last turn ended {} min ago", self.minutes_since(ended))
        });
        Ok(Some(Item::named(format!("{line}, {words}")).at(entry)))
    }

    /// The listed asks the agent's running turn took.
    fn asks_of_turn(&self, entries: &[Listed]) -> rusqlite::Result<Vec<String>> {
        let turn = self.last_turn();
        Ok(self
            .open_asks(entries)?
            .into_iter()
            .filter(|ask| ask.turn.is_some() && ask.turn.as_deref() == turn)
            .map(|ask| ask.id)
            .collect())
    }

    fn dialog_item(&self, entries: &[Listed]) -> rusqlite::Result<Item> {
        let kind = self
            .activity
            .as_ref()
            .and_then(|row| row.dialog_kind.as_deref())
            .unwrap_or("unknown kind of");
        Ok(Item {
            asks: self.asks_of_turn(entries)?,
            ..Item::named(format!(
                "{kind} dialog, {} min old, on the Dialog line",
                self.minutes_since(self.episode.stalled_since_active)
            ))
        })
    }

    fn hung_item(&self, entries: &[Listed]) -> rusqlite::Result<Item> {
        let started = self.activity.as_ref().and_then(|row| row.started);
        let started = started.map_or("start unknown".to_string(), |started| {
            format!("started {} min ago", self.minutes_since(started))
        });
        Ok(Item {
            asks: self.asks_of_turn(entries)?,
            ..Item::named(format!(
                "its running turn, {started}, no output for {} min",
                self.minutes_since(self.episode.stalled_since_active)
            ))
        })
    }

    fn hold_item(&self) -> Item {
        let reason = self
            .episode
            .dialog_text
            .as_deref()
            .or(self
                .activity
                .as_ref()
                .and_then(|row| row.hold_reason.as_deref()))
            .unwrap_or("reason unknown");
        Item::named(format!(
            "the sleep hold, missing for {} min ({})",
            self.minutes_since(self.episode.stalled_since_active),
            route::short(reason, 120)
        ))
    }

    /// Splits the list around the stuck item. An ask that was acked since
    /// the episode opened is no longer open work.
    fn work(&self, entries: &[Listed], item: &Item) -> rusqlite::Result<Work> {
        let open: Vec<usize> = self
            .open_asks(entries)?
            .iter()
            .map(|ask| ask.entry)
            .collect();
        let mut tied = Vec::new();
        let mut other: Vec<(&'static str, usize)> = Vec::new();
        for (entry, listed) in entries.iter().enumerate() {
            if item.entry == Some(entry) {
                continue;
            }
            match listed {
                Listed::Ask { .. } if !open.contains(&entry) => {}
                Listed::Ask { id } if item.asks.contains(id) => {
                    tied.push(route::ask_line(self.conn, id, self.active)?);
                }
                Listed::ChildResult { child } if item.children.contains(child) => {
                    tied.push(route::child_line(self.conn, child)?);
                }
                listed => match other.iter_mut().find(|(word, _)| *word == listed.word()) {
                    Some((_, count)) => *count += 1,
                    None => other.push((listed.word(), 1)),
                },
            }
        }
        Ok(Work { tied, other })
    }

    /// Its own session, and the sessions it started by `store`'s edge rule.
    fn moved(&self) -> rusqlite::Result<String> {
        let own = match &self.activity {
            Some(row) => format!("its own {} min ago", self.minutes_since(row.moved())),
            None => "its own unknown".to_string(),
        };
        let mono = match &self.episode.actor {
            Actor::Mono(mono) => Some(mono.as_str()),
            Actor::Session(_) => None,
        };
        let started: Option<i64> = self.conn.query_row(
            "SELECT MAX(MAX(COALESCE(a.turn_started_active_ms, a.turn_ended_active_ms),
                            a.turn_ended_active_ms,
                            COALESCE(a.last_progress_active_ms, a.turn_ended_active_ms)))
               FROM agent_activity a
              WHERE a.session_id IN (
                SELECT id FROM sessions WHERE lead_session_id = ?1 AND archived = 0
                UNION SELECT to_session_id FROM mono_messages
                 WHERE tool = 'start_session' AND status <> 'rejected' AND from_mono_id = ?2)",
            params![self.session.as_ref().map(SessionId::as_str), mono],
            |row| row.get(0),
        )?;
        Ok(match started {
            Some(started) => format!(
                "{own}; sessions it started {} min ago",
                self.minutes_since(started)
            ),
            None => format!("{own}; it started none on record"),
        })
    }

    /// `Yes` only on a row that shows the reply; `No` only once the turn is
    /// over and the rows have seen the turn that took the message.
    fn replied(&self) -> rusqlite::Result<Replied> {
        let Some(session) = &self.session else {
            return Ok(Replied::Unknown("no session on record".into()));
        };
        let last: Option<LastMessage> = self
            .conn
            .query_row(
                "SELECT id, turn_id, wanting_reply, delivered_at, from_mono_id, from_session_id
                   FROM mono_messages
                  WHERE handled_by_session = ?1 AND delivered_at IS NOT NULL
                  ORDER BY delivered_at DESC, id DESC LIMIT 1",
                [session.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, turn, wanted, delivered, from_mono, from_session)) = last else {
            return Ok(Replied::Unknown("no delivered message on record".into()));
        };
        let reply: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM mono_messages
                  WHERE created_at >= ?1 AND status NOT IN ('rejected', 'dropped')
                    AND tool IN ('send_message', 'reply')
                    AND (reply_to = ?2 OR (from_session_id = ?3
                         AND (to_mono_id = ?4 OR to_session_id = ?5)))
                  ORDER BY created_at, id LIMIT 1",
                params![delivered, id, session.as_str(), from_mono, from_session],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(reply) = reply {
            return Ok(Replied::Yes(format!("{id}, by {reply}")));
        }
        let Some(activity) = &self.activity else {
            return Ok(Replied::Unknown(format!("{id}, no turn on record")));
        };
        if activity.running {
            return Ok(Replied::Unknown(format!("{id}, its turn still runs")));
        }
        let took = turn.is_some() && turn == activity.turn_id;
        if !took && delivered > activity.updated_wall {
            return Ok(Replied::Unknown(format!(
                "{id}, the turn that took it is not recorded yet"
            )));
        }
        let why = match (wanted, took) {
            (false, _) => "it wanted no reply",
            (true, false) => "its last turn did not take it",
            (true, true) => "its last turn took it",
        };
        Ok(Replied::No(format!("{id}, {why}")))
    }
}
