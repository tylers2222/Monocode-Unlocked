//! Design sections 2, 5 and 6: who hears of an episode at each level and
//! what they read. A level not acted on 15 active minutes after its row
//! was written goes one provisioner up, taken or not (lead ruling a); every chain ends at Tyler's OS
//! notification, which never escalates.

use rusqlite::{params, Connection, OptionalExtension};

use super::limit;
use super::provisioner::{self, Provisioner};
use super::store::{agent_stall_minutes, outbox_key, Settings, MINUTE_MS};
use super::text::{self, DialogLine, Missed, Next, Stall, Who};
use crate::ask_gate::Actor;
use crate::mono_messages::{self, Notices, SessionId};

/// Worker, lead, Mono, Giga, Tyler, and the agent: a longer chain is a
/// loop in the rows, and ends at Tyler.
const MAX_CHAIN: usize = 8;

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alarm_prompts (
           outbox_key TEXT PRIMARY KEY,
           episode_id TEXT NOT NULL REFERENCES alarm_episodes(id),
           text       TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS alarm_prompts_episode ON alarm_prompts (episode_id);
         CREATE TABLE IF NOT EXISTS alarm_dialogs (
           episode_id TEXT PRIMARY KEY REFERENCES alarm_episodes(id),
           request_id TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS alarm_acts (
           episode_id   TEXT NOT NULL REFERENCES alarm_episodes(id),
           seq          INTEGER NOT NULL CHECK (seq >= 1),
           level        INTEGER NOT NULL,
           actor        TEXT NOT NULL,
           target       TEXT NOT NULL,
           how          TEXT NOT NULL CHECK (how IN ('answer', 'steer', 'nudge', 'dismiss', 'message')),
           detail       TEXT,
           outbox_key   TEXT UNIQUE,
           request_id   TEXT,
           at_active_ms INTEGER NOT NULL,
           at_wall_ms   INTEGER NOT NULL,
           PRIMARY KEY (episode_id, seq),
           UNIQUE (episode_id, actor, request_id)
         );",
    )
}

/// An episode row as routing reads it.
#[derive(Debug, Clone)]
pub(crate) struct EpisodeRow {
    pub id: String,
    pub actor: Actor,
    pub cause: String,
    pub open_work_json: String,
    pub dialog_text: Option<String>,
    pub stalled_since_active: i64,
    pub stalled_since_wall: i64,
    pub level: i64,
    pub next_escalate: Option<i64>,
    pub acted_active: Option<i64>,
    pub acted_by: Option<String>,
    pub acted_how: Option<String>,
    pub open: bool,
}

const EPISODE: &str = "SELECT id, actor, cause, open_work_json, dialog_text,
       stalled_since_active_ms, stalled_since_wall_ms, level, next_escalate_active_ms,
       acted_active_ms, acted_by, acted_how, closed_active_ms IS NULL
     FROM alarm_episodes";

fn episode_row(row: &rusqlite::Row) -> rusqlite::Result<Option<EpisodeRow>> {
    let Some(actor) = Actor::parse(&row.get::<_, String>(1)?) else {
        return Ok(None);
    };
    Ok(Some(EpisodeRow {
        id: row.get(0)?,
        actor,
        cause: row.get(2)?,
        open_work_json: row.get(3)?,
        dialog_text: row.get(4)?,
        stalled_since_active: row.get(5)?,
        stalled_since_wall: row.get(6)?,
        level: row.get(7)?,
        next_escalate: row.get(8)?,
        acted_active: row.get(9)?,
        acted_by: row.get(10)?,
        acted_how: row.get(11)?,
        open: row.get(12)?,
    }))
}

pub(crate) fn episode(conn: &Connection, id: &str) -> rusqlite::Result<Option<EpisodeRow>> {
    Ok(conn
        .query_row(&format!("{EPISODE} WHERE id = ?1"), [id], episode_row)
        .optional()?
        .flatten())
}

pub(crate) fn open_episodes(conn: &Connection) -> rusqlite::Result<Vec<EpisodeRow>> {
    let rows = conn
        .prepare(&format!(
            "{EPISODE} WHERE closed_active_ms IS NULL ORDER BY opened_active_ms, id"
        ))?
        .query_map([], episode_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

fn stored(recipient: &str) -> Provisioner {
    Actor::parse(recipient).map_or(Provisioner::Tyler, Provisioner::Agent)
}

/// `from`, then each provisioner above it up to Tyler. A loop in the rows
/// ends at Tyler.
pub(crate) fn climb(
    from: Provisioner,
    mut up: impl FnMut(&Actor) -> rusqlite::Result<Provisioner>,
) -> rusqlite::Result<Vec<Provisioner>> {
    let mut chain = vec![from];
    while let Some(Provisioner::Agent(actor)) = chain.last() {
        let next = up(actor)?;
        let next = if chain.contains(&next) || chain.len() + 1 >= MAX_CHAIN {
            Provisioner::Tyler
        } else {
            next
        };
        chain.push(next);
    }
    Ok(chain)
}

pub(crate) fn provisioner_of(
    conn: &Connection,
    actor: &Actor,
    root_giga: Option<&str>,
) -> rusqlite::Result<Provisioner> {
    Ok(provisioner::provisioner(provisioner::lineage(
        conn, actor, root_giga,
    )?))
}

/// The episode's chain: index 0 is the stalled agent, index k the
/// recipient of level k as stored, and above the current level the
/// provisioners still to come, up to Tyler.
pub(crate) fn chain(
    conn: &Connection,
    episode: &EpisodeRow,
    root_giga: Option<&str>,
) -> rusqlite::Result<Vec<Provisioner>> {
    let sent = conn
        .prepare("SELECT recipient FROM alarm_sends WHERE episode_id = ?1 ORDER BY level")?
        .query_map([&episode.id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut chain = vec![Provisioner::Agent(episode.actor.clone())];
    chain.extend(sent.iter().map(|recipient| stored(recipient)));
    let top = chain.pop().unwrap_or(Provisioner::Tyler);
    chain.extend(climb(top, |actor| provisioner_of(conn, actor, root_giga))?);
    Ok(chain)
}

/// A chat-bearing session of `actor`: its own, or a Mono's current chat.
pub(crate) fn session_of(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<SessionId>> {
    Ok(match actor {
        Actor::Session(session) => Some(session.clone()),
        Actor::Mono(mono) => mono_messages::mono_row(conn, mono)?.and_then(|row| row.chat),
    })
}

pub(crate) fn who(conn: &Connection, actor: &Actor) -> rusqlite::Result<Who> {
    let label: Option<String> = match actor {
        Actor::Mono(mono) => conn
            .query_row(
                "SELECT name FROM monos WHERE id = ?1",
                [mono.as_str()],
                |row| row.get(0),
            )
            .optional()?,
        Actor::Session(session) => conn
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .optional()?,
    };
    Ok(Who {
        label: label
            .map(|label| short(&label, 80))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| actor.to_string()),
        actor: actor.to_string(),
    })
}

/// `text` on one line, at most `max` characters.
pub(crate) fn short(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The local clock time of wall time `wall_ms`, as `HH:MM`.
fn clock(conn: &Connection, wall_ms: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT strftime('%H:%M', ?1 / 1000, 'unixepoch', 'localtime')",
        [wall_ms],
        |row| row.get(0),
    )
}

fn ask_line(conn: &Connection, id: &str, active: i64) -> rusqlite::Result<String> {
    let row: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT a.due_active_ms, m.body FROM mono_asks a
               LEFT JOIN mono_messages m ON m.id = a.id WHERE a.id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (due, body) = row.unwrap_or((None, None));
    let due = match due {
        None => "no deadline".to_string(),
        Some(due) if due >= active => format!("due in {} min", (due - active) / MINUTE_MS),
        Some(due) => format!("overdue {} min", (active - due) / MINUTE_MS),
    };
    Ok(match body {
        Some(body) => format!("ask {id} \"{}\" ({due})", short(&body, 80)),
        None => format!("ask {id} ({due})"),
    })
}

/// The episode's open work (section 4), one line per item.
fn work_lines(conn: &Connection, json: &str, active: i64) -> rusqlite::Result<Vec<String>> {
    let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    let mut lines = Vec::new();
    for item in items {
        let line = match item["kind"].as_str() {
            Some("ask") => ask_line(conn, item["id"].as_str().unwrap_or_default(), active)?,
            Some("child_result") => {
                let child = item["child"].as_str().unwrap_or_default();
                match Actor::parse(child) {
                    Some(actor) => format!("the result of {} not read", who(conn, &actor)?.label),
                    None => format!("the result of {child} not read"),
                }
            }
            Some("todo") => "an unfinished todo list".into(),
            _ => continue,
        };
        lines.push(line);
    }
    Ok(lines)
}

/// The last tool block of a transcript, as `title (status)`.
fn last_tool(blocks_json: &str) -> Option<String> {
    let blocks: Vec<serde_json::Value> = serde_json::from_str(blocks_json).ok()?;
    let tool = blocks.iter().rev().find_map(|block| block.get("tool"))?;
    let title = tool["title"]
        .as_str()
        .or_else(|| tool["kind"].as_str())
        .unwrap_or("tool");
    Some(format!(
        "{} ({})",
        short(title, 120),
        tool["status"].as_str().unwrap_or("unknown")
    ))
}

fn last_activity(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<String>> {
    let Some(session) = session_of(conn, actor)? else {
        return Ok(None);
    };
    let row: Option<(Option<i64>, String)> = conn
        .query_row(
            "SELECT a.last_progress_wall_ms, s.blocks_json FROM sessions s
               LEFT JOIN agent_activity a ON a.session_id = s.id WHERE s.id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((progress, blocks)) = row else {
        return Ok(None);
    };
    let at = progress.map(|wall| clock(conn, wall)).transpose()?;
    Ok(match (at, last_tool(&blocks)) {
        (Some(at), Some(tool)) => Some(format!("{at} {tool}")),
        (None, Some(tool)) => Some(tool),
        (Some(at), None) => Some(format!("{at} output")),
        (None, None) => None,
    })
}

fn dialog_kind(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<String>> {
    let Some(session) = session_of(conn, actor)? else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            "SELECT dialog_kind FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// What every level says about the stalled agent, read now.
pub(crate) fn stall(
    conn: &Connection,
    episode: &EpisodeRow,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<Stall> {
    let minutes = (active - episode.stalled_since_active) / MINUTE_MS;
    let dialog = if episode.cause == "dialog" {
        Some(DialogLine {
            kind: dialog_kind(conn, &episode.actor)?.unwrap_or_else(|| "permission".into()),
            minutes,
            text: episode.dialog_text.clone().unwrap_or_default(),
        })
    } else {
        None
    };
    Ok(Stall {
        episode: episode.id.clone(),
        cause: episode.cause.clone(),
        agent: who(conn, &episode.actor)?,
        since: clock(conn, episode.stalled_since_wall)?,
        minutes,
        open_work: work_lines(conn, &episode.open_work_json, active)?,
        last_activity: last_activity(conn, &episode.actor)?,
        dialog,
        escalate_minutes: settings.escalate_minutes,
        held: 0,
    })
}

/// What level `level`'s recipient did not act on, seen from the agent.
fn missed_what(chain: &[Provisioner], level: usize) -> String {
    if level > 2 {
        return format!("the level-{} alarm", level - 1);
    }
    match (chain.first(), chain.get(1)) {
        (
            Some(Provisioner::Agent(Actor::Session(_))),
            Some(Provisioner::Agent(Actor::Session(_))),
        ) => "its worker's stall",
        (Some(Provisioner::Agent(Actor::Session(_))), Some(Provisioner::Agent(Actor::Mono(_)))) => {
            "its lead's stall"
        }
        (Some(Provisioner::Agent(Actor::Mono(_))), _) => "its Mono's stall",
        _ => "the stall",
    }
    .into()
}

fn next_of(conn: &Connection, chain: &[Provisioner], level: usize) -> rusqlite::Result<Next> {
    if matches!(chain.get(level), Some(Provisioner::Tyler) | None) {
        return Ok(Next::None);
    }
    Ok(match chain.get(level + 1) {
        Some(Provisioner::Agent(actor)) => Next::Agent(who(conn, actor)?),
        Some(Provisioner::Tyler) => Next::Tyler,
        None => Next::None,
    })
}

/// The outbox text of level `level` of `episode` on `chain`.
fn level_text(
    conn: &Connection,
    episode: &EpisodeRow,
    chain: &[Provisioner],
    level: usize,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<String> {
    let mut stall = stall(conn, episode, settings, active)?;
    if level == 1 {
        stall.held = limit::grouped(conn, episode)?;
    }
    let missed = match chain.get(level - 1) {
        Some(Provisioner::Agent(actor)) if level >= 2 => Some(Missed {
            who: who(conn, actor)?,
            what: missed_what(chain, level),
        }),
        _ => None,
    };
    let level_number = i64::try_from(level).unwrap_or(i64::MAX);
    Ok(text::alarm(
        &stall,
        level_number,
        missed.as_ref(),
        &next_of(conn, chain, level)?,
    ))
}

/// Writes level `level` of `episode` for `to` once, and says if it did: its
/// send row, and its escalation clock, which starts now; Tyler's level never
/// escalates. `chain` reads these rows by position, so a level whose message
/// is held has its row all the same. A level already written is left as it is.
fn log_level(
    tx: &Connection,
    episode: &str,
    level: i64,
    to: &Provisioner,
    (active, escalate_ms): (i64, i64),
) -> rusqlite::Result<bool> {
    let key = match to {
        Provisioner::Agent(_) => Some(outbox_key(episode, level)),
        Provisioner::Tyler => None,
    };
    let inserted = tx.execute(
        "INSERT INTO alarm_sends (episode_id, level, recipient, outbox_key, created_active_ms)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (episode_id, level) DO NOTHING",
        params![episode, level, to.stored(), key, active],
    )? == 1;
    if inserted {
        let due = matches!(to, Provisioner::Agent(_)).then_some(active + escalate_ms);
        tx.execute(
            "UPDATE alarm_episodes SET level = ?2, next_escalate_active_ms = ?3 WHERE id = ?1",
            params![episode, level, due],
        )?;
    }
    Ok(inserted)
}

/// Puts level `level`'s message for `recipient` in the outbox, with its text,
/// and names the recipient's chat.
fn enqueue(
    tx: &Connection,
    notices: &mut Notices,
    episode: &str,
    level: i64,
    recipient: &Actor,
    text: &str,
    active: i64,
) -> rusqlite::Result<()> {
    let key = outbox_key(episode, level);
    let recipient = recipient.to_string();
    tx.execute(
        "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
         VALUES (?1, NULL, 'alarm', ?2, ?3) ON CONFLICT (key) DO NOTHING",
        params![key, recipient, active],
    )?;
    tx.execute(
        "INSERT INTO alarm_prompts (outbox_key, episode_id, text) VALUES (?1, ?2, ?3)
         ON CONFLICT (outbox_key) DO NOTHING",
        params![key, episode, text],
    )?;
    crate::ask_scheduler::name_chat(tx, notices, &recipient)
}

/// Writes a level and its message at once: every level above the first.
fn send(
    tx: &Connection,
    notices: &mut Notices,
    episode: &str,
    level: i64,
    to: &Provisioner,
    text: &str,
    clock: (i64, i64),
) -> rusqlite::Result<()> {
    if let (true, Provisioner::Agent(recipient)) = (log_level(tx, episode, level, to, clock)?, to) {
        enqueue(tx, notices, episode, level, recipient, text, clock.0)?;
    }
    Ok(())
}

/// The level-1 message goes now, whether it was just opened or was held: its
/// window starts and the count it carries is spent.
fn tell_level_one(
    tx: &Connection,
    notices: &mut Notices,
    episode: &EpisodeRow,
    chain: &[Provisioner],
    recipient: &Actor,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<()> {
    let text = level_text(tx, episode, chain, 1, settings, active)?;
    limit::sent(tx, episode, active)?;
    enqueue(tx, notices, &episode.id, 1, recipient, &text, active)
}

/// The held level-1 message of `episode` goes now. Its escalation clock has
/// run since the episode opened, so it says the whole minutes that are left.
pub(super) fn send_held(
    tx: &Connection,
    notices: &mut Notices,
    episode: &EpisodeRow,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<()> {
    let chain = chain(tx, episode, settings.root_giga.as_deref())?;
    let Some(Provisioner::Agent(recipient)) = chain.get(1) else {
        return Ok(());
    };
    let left = episode.next_escalate.unwrap_or(active) - active;
    let settings = Settings {
        escalate_minutes: (left / MINUTE_MS).max(1),
        ..settings.clone()
    };
    tell_level_one(tx, notices, episode, &chain, recipient, &settings, active)
}

/// Episode `id` just opened: level 1 goes to its stored provisioner, unless
/// the limit holds its message. Its send row and escalation clock start either
/// way.
pub(crate) fn opened(
    tx: &Connection,
    notices: &mut Notices,
    id: &str,
    settings: &Settings,
    active: i64,
) -> rusqlite::Result<()> {
    let Some(episode) = episode(tx, id)? else {
        return Ok(());
    };
    let to: Option<String> = tx
        .query_row(
            "SELECT provisioner FROM alarm_episodes WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let to = stored(to.as_deref().unwrap_or("tyler"));
    let mut chain = vec![Provisioner::Agent(episode.actor.clone())];
    chain.extend(climb(to.clone(), |actor| {
        provisioner_of(tx, actor, settings.root_giga.as_deref())
    })?);
    if episode.cause == "dialog" {
        if let Some(session) = session_of(tx, &episode.actor)? {
            tx.execute(
                "INSERT INTO alarm_dialogs (episode_id, request_id)
                 SELECT ?1, dialog_request_id FROM agent_activity
                  WHERE session_id = ?2 AND dialog_request_id IS NOT NULL
                 ON CONFLICT (episode_id) DO NOTHING",
                params![id, session.as_str()],
            )?;
        }
    }
    let clock = (active, settings.escalate_minutes * MINUTE_MS);
    log_level(tx, id, 1, &to, clock)?;
    let Provisioner::Agent(recipient) = &to else {
        return Ok(());
    };
    if limit::holds(tx, &episode, active)? {
        return Ok(());
    }
    tell_level_one(tx, notices, &episode, &chain, recipient, settings, active)
}

/// The request id of the dialog episode `id` alarmed about.
pub(crate) fn dialog_shown(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT request_id FROM alarm_dialogs WHERE episode_id = ?1",
        [id],
        |row| row.get(0),
    )
    .optional()
}

/// The unsent alarm rows of episode `id` go with it when it closes. A
/// nudge or steer is its sender's message and is still delivered.
pub(crate) fn drop_unsent(tx: &Connection, id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM mono_outbox WHERE sent_at IS NULL AND stage = 'alarm'
           AND key IN (SELECT outbox_key FROM alarm_prompts WHERE episode_id = ?1)",
        [id],
    )?;
    Ok(())
}

/// The outbox row of a level was taken by its recipient's provider: the
/// level is logged sent. Its escalation clock started when it was written.
fn record_sent(tx: &Connection) -> rusqlite::Result<()> {
    let taken = tx
        .prepare(
            "SELECT s.episode_id, s.level, o.sent_at FROM alarm_sends s
               JOIN mono_outbox o ON o.key = s.outbox_key
               JOIN alarm_episodes e ON e.id = s.episode_id AND e.closed_active_ms IS NULL
              WHERE s.sent_wall_ms IS NULL AND o.sent_at IS NOT NULL",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (episode, level, sent_at) in taken {
        tx.execute(
            "UPDATE alarm_sends SET sent_wall_ms = ?3 WHERE episode_id = ?1 AND level = ?2",
            params![episode, level, sent_at],
        )?;
    }
    Ok(())
}

fn escalate(
    tx: &Connection,
    notices: &mut Notices,
    episode: &EpisodeRow,
    settings: &Settings,
    at: (i64, i64),
) -> rusqlite::Result<()> {
    let chain = chain(tx, episode, settings.root_giga.as_deref())?;
    let Ok(level) = usize::try_from(episode.level + 1) else {
        return Ok(());
    };
    let Some(to) = chain.get(level) else {
        return tx
            .execute(
                "UPDATE alarm_episodes SET next_escalate_active_ms = NULL WHERE id = ?1",
                [&episode.id],
            )
            .map(drop);
    };
    let text = match to {
        Provisioner::Agent(_) => level_text(tx, episode, &chain, level, settings, at.0)?,
        Provisioner::Tyler => String::new(),
    };
    send(
        tx,
        notices,
        &episode.id,
        episode.level + 1,
        to,
        &text,
        (at.0, settings.escalate_minutes * MINUTE_MS),
    )
}

fn close_no_effect(tx: &Connection, id: &str, (active, wall): (i64, i64)) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE alarm_episodes SET closed_active_ms = ?2, closed_wall_ms = ?3,
           close_reason = 'act_no_effect'
         WHERE id = ?1 AND closed_active_ms IS NULL",
        params![id, active, wall],
    )?;
    drop_unsent(tx, id)
}

/// The routing half of a detector pass, in its transaction: a level not
/// acted on 15 minutes after its row was written goes a level up, taken or
/// not; an act that left the agent stalled N minutes closes the
/// episode `act_no_effect`, so the next pass opens a new one.
pub(crate) fn run(
    tx: &Connection,
    notices: &mut Notices,
    settings: &Settings,
    at: (i64, i64),
) -> rusqlite::Result<()> {
    let active = at.0;
    record_sent(tx)?;
    limit::release_due(tx, notices, settings, active)?;
    for episode in open_episodes(tx)? {
        if let Some(acted) = episode.acted_active {
            let stall_minutes =
                agent_stall_minutes(tx, &episode.actor)?.unwrap_or(settings.stall_minutes);
            if active - acted >= stall_minutes * MINUTE_MS {
                close_no_effect(tx, &episode.id, at)?;
            }
            continue;
        }
        if episode.next_escalate.is_some_and(|due| active >= due) {
            escalate(tx, notices, &episode, settings, at)?;
        }
    }
    Ok(())
}

/// A level that ends at Tyler and has not been shown yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TylerAlarm {
    pub episode: String,
    pub level: i64,
    pub title: String,
    pub body: String,
}

pub(crate) fn tyler_pending(conn: &Connection, active: i64) -> rusqlite::Result<Vec<TylerAlarm>> {
    let rows = conn
        .prepare(
            "SELECT s.episode_id, s.level FROM alarm_sends s
               JOIN alarm_episodes e ON e.id = s.episode_id AND e.closed_active_ms IS NULL
              WHERE s.recipient = 'tyler' AND s.sent_wall_ms IS NULL
              ORDER BY s.created_active_ms, s.episode_id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let settings = Settings::load(conn)?;
    let mut pending = Vec::new();
    for (id, level) in rows {
        let Some(episode) = episode(conn, &id)? else {
            continue;
        };
        let (title, body) = text::notification(&stall(conn, &episode, &settings, active)?);
        pending.push(TylerAlarm {
            episode: id,
            level,
            title,
            body,
        });
    }
    Ok(pending)
}

/// Tyler was shown level `level` of `episode`.
pub(crate) fn tyler_told(
    conn: &Connection,
    episode: &str,
    level: i64,
    wall: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE alarm_sends SET sent_wall_ms = ?3
          WHERE episode_id = ?1 AND level = ?2 AND recipient = 'tyler' AND sent_wall_ms IS NULL",
        params![episode, level, wall],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mono_messages::MonoId;

    fn session(id: &str) -> Actor {
        Actor::Session(SessionId::parse(id).unwrap())
    }

    fn mono(id: &str) -> Actor {
        Actor::Mono(MonoId::parse(id).unwrap())
    }

    // Defect: escalation skips a level, stops before Tyler, goes past him,
    // or loops forever on rows that name each other.
    #[test]
    fn escalation_climbs_one_provisioner_at_a_time_and_ends_at_tyler() {
        let up = |actor: &Actor| -> rusqlite::Result<Provisioner> {
            Ok(match actor.to_string().as_str() {
                "session:worker" => Provisioner::Agent(session("lead")),
                "session:lead" => Provisioner::Agent(mono("pr-mono")),
                "mono:pr-mono" => Provisioner::Agent(mono("general")),
                "mono:loop-a" => Provisioner::Agent(mono("loop-b")),
                "mono:loop-b" => Provisioner::Agent(mono("loop-a")),
                _ => Provisioner::Tyler,
            })
        };
        let cases = [
            (
                session("worker"),
                vec![
                    Provisioner::Agent(session("worker")),
                    Provisioner::Agent(session("lead")),
                    Provisioner::Agent(mono("pr-mono")),
                    Provisioner::Agent(mono("general")),
                    Provisioner::Tyler,
                ],
            ),
            (
                mono("general"),
                vec![Provisioner::Agent(mono("general")), Provisioner::Tyler],
            ),
            (
                mono("loop-a"),
                vec![
                    Provisioner::Agent(mono("loop-a")),
                    Provisioner::Agent(mono("loop-b")),
                    Provisioner::Tyler,
                ],
            ),
        ];
        for (from, expected) in cases {
            assert_eq!(
                climb(Provisioner::Agent(from.clone()), up).unwrap(),
                expected,
                "{from}"
            );
        }
        assert_eq!(
            climb(Provisioner::Tyler, up).unwrap(),
            vec![Provisioner::Tyler],
            "nothing after Tyler"
        );
    }

    // Defect: a level-2 alarm says the wrong relation for whose stall
    // the level-1 recipient ignored.
    #[test]
    fn a_missed_level_names_whose_stall_it_was() {
        let chain = |agent: Actor, first: Actor| {
            vec![
                Provisioner::Agent(agent),
                Provisioner::Agent(first),
                Provisioner::Tyler,
            ]
        };
        assert_eq!(
            missed_what(&chain(session("w"), session("l")), 2),
            "its worker's stall"
        );
        assert_eq!(
            missed_what(&chain(session("l"), mono("m")), 2),
            "its lead's stall"
        );
        assert_eq!(
            missed_what(&chain(mono("m"), mono("g")), 2),
            "its Mono's stall"
        );
        assert_eq!(
            missed_what(&chain(session("w"), session("l")), 3),
            "the level-2 alarm"
        );
    }
}
