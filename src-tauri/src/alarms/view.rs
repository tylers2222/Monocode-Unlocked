//! Row U2: what the front end reads of the alarms and the sleep holds
//! (`alarms_view`), the event that says it changed, and the writes the app
//! user makes: settings, a session's stall minutes, Tyler's own acts and
//! his answers to his holds. Reads only the tables the detector, routing
//! and the hold service write.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use super::act::{self, TYLER};
use super::provisioner::{self, Provisioner};
use super::route::{self, EpisodeRow};
use super::store::{Settings, MINUTE_MS, STALL_MINUTES};
use super::text::Next;
use crate::ask_gate::Actor;
use crate::mono_messages::{self, MonoId};
use crate::mono_roster::MonoRole;
use crate::session_store::SessionStore;
use crate::sleep_hold::service as hold_service;

/// The app-wide event: `alarms_view` would read something new.
pub(crate) const CHANGED: &str = "monocode-alarms-changed";

/// A decided Tyler hold stays listed this long after its request (wall).
const DECIDED_SHOWN_MS: i64 = 6 * 60 * MINUTE_MS;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentWire {
    pub actor: String,
    pub name: String,
    pub role: &'static str,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum NextWire {
    Agent { agent: AgentWire },
    Tyler,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendWire {
    pub level: i64,
    pub recipient: Option<AgentWire>,
    pub at: Option<String>,
    pub missed: Option<String>,
    pub next: Option<NextWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DialogWire {
    pub kind: String,
    pub minutes: i64,
    pub text: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActedWire {
    pub how: String,
    pub by: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EpisodeWire {
    pub id: String,
    pub agent: AgentWire,
    pub cause: String,
    pub level: i64,
    pub stalled_since: String,
    pub minutes: i64,
    pub open_work: Vec<String>,
    pub last_activity: Option<String>,
    pub dialog: Option<DialogWire>,
    pub hold_missing: Option<String>,
    pub sends: Vec<SendWire>,
    pub next_minutes: Option<i64>,
    pub acted: Option<ActedWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum DecisionWire {
    Allow(Decided),
    Deny(Decided),
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Decided {
    pub by: String,
    pub at: String,
    pub via: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HoldWire {
    pub id: String,
    pub session_id: String,
    pub tool_use_id: String,
    pub agent: AgentWire,
    pub command: String,
    pub how_long: String,
    pub reason: Option<String>,
    pub remaining_ms: i64,
    pub window_ms: i64,
    pub decision: Option<DecisionWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GigaWire {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsWire {
    pub stall_minutes: i64,
    pub escalate_minutes: i64,
    pub sleep_hold: bool,
    pub tyler_sleep_hold: bool,
    pub short_sleep_skip_seconds: i64,
    pub root_giga: Option<GigaWire>,
    pub gigas: Vec<GigaWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StallMinutesWire {
    pub actor: String,
    pub session_id: Option<String>,
    pub minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AlarmsSnapshot {
    pub read_wall_ms: i64,
    pub episodes: Vec<EpisodeWire>,
    pub holds: Vec<HoldWire>,
    pub settings: SettingsWire,
    pub stall_minutes: Vec<StallMinutesWire>,
}

fn agent(conn: &Connection, settings: &Settings, actor: &Actor) -> rusqlite::Result<AgentWire> {
    let role = match actor {
        Actor::Mono(mono) => match mono_messages::mono_row(conn, mono)?.map(|row| row.role) {
            Some(MonoRole::Giga) => "giga",
            Some(MonoRole::Mono) | None => "mono",
        },
        Actor::Session(_) => {
            match route::provisioner_of(conn, actor, settings.root_giga.as_deref())? {
                Provisioner::Agent(Actor::Session(_)) => "worker",
                Provisioner::Agent(Actor::Mono(_)) => "lead",
                Provisioner::Tyler => "session",
            }
        }
    };
    Ok(AgentWire {
        actor: actor.to_string(),
        name: route::who(conn, actor)?.label,
        role,
        session_id: route::session_of(conn, actor)?.map(|session| session.as_str().to_string()),
    })
}

/// Who an act or a decision was by, as the card says it.
fn by_label(conn: &Connection, stored: &str) -> rusqlite::Result<String> {
    if stored == TYLER {
        return Ok("Tyler".into());
    }
    Ok(match Actor::parse(stored) {
        Some(actor) => route::who(conn, &actor)?.label,
        None => stored.to_string(),
    })
}

fn sends(
    conn: &Connection,
    settings: &Settings,
    episode: &EpisodeRow,
) -> rusqlite::Result<Vec<SendWire>> {
    let chain = route::chain(conn, episode, settings.root_giga.as_deref())?;
    let rows = conn
        .prepare(
            "SELECT level, recipient, sent_wall_ms FROM alarm_sends
              WHERE episode_id = ?1 ORDER BY level",
        )?
        .query_map([&episode.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut sends = Vec::new();
    for (level, recipient, sent) in rows {
        let index = usize::try_from(level).unwrap_or(usize::MAX);
        let next = match route::next_of(conn, &chain, index)? {
            Next::Agent(who) => match Actor::parse(&who.actor) {
                Some(actor) => Some(NextWire::Agent {
                    agent: agent(conn, settings, &actor)?,
                }),
                None => None,
            },
            Next::Tyler => Some(NextWire::Tyler),
            Next::None => None,
        };
        sends.push(SendWire {
            level,
            recipient: Actor::parse(&recipient)
                .map(|actor| agent(conn, settings, &actor))
                .transpose()?,
            at: sent.map(|wall| route::clock(conn, wall)).transpose()?,
            missed: (level >= 2).then(|| route::missed_what(&chain, index)),
            next,
        });
    }
    Ok(sends)
}

fn acted(conn: &Connection, episode: &EpisodeRow) -> rusqlite::Result<Option<ActedWire>> {
    if episode.acted_by.is_none() {
        return Ok(None);
    }
    let last: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT actor, how, at_wall_ms FROM alarm_acts
              WHERE episode_id = ?1 ORDER BY seq DESC LIMIT 1",
            [&episode.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    last.map(|(actor, how, at)| {
        Ok(ActedWire {
            how,
            by: by_label(conn, &actor)?,
            at: route::clock(conn, at)?,
        })
    })
    .transpose()
}

/// Minutes until `due`, rounded up, from `active`; never below 0.
fn minutes_until(due: i64, active: i64) -> i64 {
    let left = (due - active).max(0);
    (left + MINUTE_MS - 1) / MINUTE_MS
}

fn episode(
    conn: &Connection,
    settings: &Settings,
    episode: &EpisodeRow,
    active: i64,
) -> rusqlite::Result<EpisodeWire> {
    let stall = route::stall(conn, episode, settings, active)?;
    let dialog = match stall.dialog {
        Some(dialog) => Some(DialogWire {
            kind: dialog.kind,
            minutes: dialog.minutes,
            text: dialog.text,
            request_id: route::dialog_shown(conn, &episode.id)?,
        }),
        None => None,
    };
    Ok(EpisodeWire {
        id: episode.id.clone(),
        agent: agent(conn, settings, &episode.actor)?,
        cause: episode.cause.clone(),
        level: episode.level,
        stalled_since: stall.since,
        minutes: stall.minutes,
        open_work: stall.open_work,
        last_activity: stall.last_activity,
        dialog,
        hold_missing: stall.hold_missing,
        sends: sends(conn, settings, episode)?,
        next_minutes: episode.next_escalate.map(|due| minutes_until(due, active)),
        acted: acted(conn, episode)?,
    })
}

/// Tyler's holds the card lists: undecided ones, and those decided (not
/// skipped) that were requested in the last 6 h of wall time.
const TYLER_HOLDS: &str = "FROM sleep_holds
  WHERE provisioner = 'tyler'
    AND (decision IS NULL OR (decision <> 'skipped' AND requested_wall_ms >= ?1))";

fn holds(
    conn: &Connection,
    settings: &Settings,
    (active, wall): (i64, i64),
) -> rusqlite::Result<Vec<HoldWire>> {
    struct Row {
        id: String,
        actor: String,
        session: String,
        tool_use_id: String,
        command: String,
        wait_kind: String,
        wait_seconds: Option<f64>,
        reason: Option<String>,
        requested: i64,
        deadline: i64,
        decision: Option<String>,
        decided_by: Option<String>,
        decided_wall: Option<i64>,
        decision_reason: Option<String>,
        decided_request_id: Option<String>,
    }
    let rows = conn
        .prepare(&format!(
            "SELECT id, actor, session_id, tool_use_id, command, wait_kind, wait_seconds, reason,
                    requested_active_ms, deadline_active_ms, decision, decided_by,
                    decided_wall_ms, decision_reason, decided_request_id
             {TYLER_HOLDS} ORDER BY requested_active_ms, id"
        ))?
        .query_map([wall - DECIDED_SHOWN_MS], |row| {
            Ok(Row {
                id: row.get(0)?,
                actor: row.get(1)?,
                session: row.get(2)?,
                tool_use_id: row.get(3)?,
                command: row.get(4)?,
                wait_kind: row.get(5)?,
                wait_seconds: row.get(6)?,
                reason: row.get(7)?,
                requested: row.get(8)?,
                deadline: row.get(9)?,
                decision: row.get(10)?,
                decided_by: row.get(11)?,
                decided_wall: row.get(12)?,
                decision_reason: row.get(13)?,
                decided_request_id: row.get(14)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut holds = Vec::new();
    for row in rows {
        let Some(actor) = Actor::parse(&row.actor) else {
            continue;
        };
        let decided = |conn: &Connection| -> rusqlite::Result<Decided> {
            Ok(Decided {
                by: by_label(conn, row.decided_by.as_deref().unwrap_or(TYLER))?,
                at: row
                    .decided_wall
                    .map(|wall| route::clock(conn, wall))
                    .transpose()?
                    .unwrap_or_default(),
                via: if row.decided_request_id.is_some() {
                    "app"
                } else {
                    "notification"
                },
                reason: row.decision_reason.clone(),
            })
        };
        let decision = match row.decision.as_deref() {
            None => None,
            Some("allow") => Some(DecisionWire::Allow(decided(conn)?)),
            Some("deny") => Some(DecisionWire::Deny(decided(conn)?)),
            Some(_) => Some(DecisionWire::Timeout),
        };
        holds.push(HoldWire {
            remaining_ms: match decision {
                None => (row.deadline - active).max(0),
                Some(_) => 0,
            },
            window_ms: row.deadline - row.requested,
            agent: agent(conn, settings, &actor)?,
            how_long: hold_service::how_long_text(&row.wait_kind, row.wait_seconds),
            id: row.id,
            session_id: row.session,
            tool_use_id: row.tool_use_id,
            command: row.command,
            reason: row.reason,
            decision,
        });
    }
    Ok(holds)
}

fn settings_wire(conn: &Connection, settings: &Settings) -> rusqlite::Result<SettingsWire> {
    let gigas = provisioner::gigas(conn)?;
    let root = provisioner::root_giga(settings.root_giga.as_deref(), &gigas);
    let wire = |giga: &provisioner::Giga| GigaWire {
        id: giga.id.as_str().to_string(),
        name: giga.name.clone(),
    };
    Ok(SettingsWire {
        stall_minutes: settings.stall_minutes,
        escalate_minutes: settings.escalate_minutes,
        sleep_hold: settings.sleep_hold,
        tyler_sleep_hold: settings.tyler_sleep_hold,
        short_sleep_skip_seconds: settings.short_sleep_skip_s,
        root_giga: root.and_then(|root| gigas.iter().find(|giga| giga.id == root).map(wire)),
        gigas: gigas.iter().map(wire).collect(),
    })
}

fn stall_minutes(conn: &Connection) -> rusqlite::Result<Vec<StallMinutesWire>> {
    let rows = conn
        .prepare("SELECT actor, stall_minutes FROM agent_alarm_settings ORDER BY actor")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(actor, minutes)| {
            let session = match Actor::parse(&actor) {
                Some(parsed) => route::session_of(conn, &parsed)?,
                None => None,
            };
            Ok(StallMinutesWire {
                actor,
                session_id: session.map(|session| session.as_str().to_string()),
                minutes,
            })
        })
        .collect()
}

/// What the front end shows of the alarms at `(active, wall)`.
pub(crate) fn snapshot(conn: &Connection, now: (i64, i64)) -> rusqlite::Result<AlarmsSnapshot> {
    let settings = Settings::load(conn)?;
    let episodes = route::open_episodes(conn)?
        .iter()
        .map(|row| episode(conn, &settings, row, now.0))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(AlarmsSnapshot {
        read_wall_ms: now.1,
        episodes,
        holds: holds(conn, &settings, now)?,
        settings: settings_wire(conn, &settings)?,
        stall_minutes: stall_minutes(conn)?,
    })
}

/// An open episode as the digest reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EpisodeDigest {
    pub id: String,
    pub level: i64,
    pub cause: String,
    pub stalled_since_active: i64,
    pub next_escalate: Option<i64>,
    pub acted_by: Option<String>,
    pub acted_how: Option<String>,
    pub acted_active: Option<i64>,
    pub dialog_text: Option<String>,
    pub dialog_request_id: Option<String>,
    /// How many of its levels were taken.
    pub sent: i64,
}

/// A listed Tyler hold as the digest reads it: id, decision, by, request id.
pub(crate) type HoldDigest = (String, Option<String>, Option<String>, Option<String>);

/// The rows the snapshot shows that change, read with a few small
/// queries (no transcript), for the digest.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DigestRows {
    pub episodes: Vec<EpisodeDigest>,
    pub holds: Vec<HoldDigest>,
    pub settings: Vec<(String, String)>,
    pub stall_minutes: Vec<(String, i64)>,
    pub gigas: Vec<(String, String)>,
}

fn pairs<T: rusqlite::types::FromSql>(
    conn: &Connection,
    sql: &str,
) -> rusqlite::Result<Vec<(String, T)>> {
    conn.prepare(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

pub(crate) fn digest_rows(conn: &Connection, wall: i64) -> rusqlite::Result<DigestRows> {
    let episodes = conn
        .prepare(
            "SELECT e.id, e.level, e.cause, e.stalled_since_active_ms, e.next_escalate_active_ms,
                    e.acted_by, e.acted_how, e.acted_active_ms, e.dialog_text, d.request_id,
                    (SELECT COUNT(sent_wall_ms) FROM alarm_sends s WHERE s.episode_id = e.id)
               FROM alarm_episodes e LEFT JOIN alarm_dialogs d ON d.episode_id = e.id
              WHERE e.closed_active_ms IS NULL ORDER BY e.id",
        )?
        .query_map([], |row| {
            Ok(EpisodeDigest {
                id: row.get(0)?,
                level: row.get(1)?,
                cause: row.get(2)?,
                stalled_since_active: row.get(3)?,
                next_escalate: row.get(4)?,
                acted_by: row.get(5)?,
                acted_how: row.get(6)?,
                acted_active: row.get(7)?,
                dialog_text: row.get(8)?,
                dialog_request_id: row.get(9)?,
                sent: row.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let holds = conn
        .prepare(&format!(
            "SELECT id, decision, decided_by, decided_request_id {TYLER_HOLDS} ORDER BY id"
        ))?
        .query_map([wall - DECIDED_SHOWN_MS], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(DigestRows {
        episodes,
        holds,
        settings: pairs(conn, "SELECT key, value FROM alarm_settings ORDER BY key")?,
        stall_minutes: pairs(
            conn,
            "SELECT actor, stall_minutes FROM agent_alarm_settings ORDER BY actor",
        )?,
        gigas: pairs(
            conn,
            "SELECT id, name FROM monos WHERE role = 'giga' ORDER BY sort_order, created_at, id",
        )?,
    })
}

/// A digest of what the snapshot shows at active time `active`: it moves
/// when an episode opens, closes, goes up, is acted on or its minutes
/// (stalled, or until the next level) turn over; when a hold is listed,
/// decided or drops out; and when a setting changes. Not with the hold
/// countdown, which the front end runs.
pub(crate) fn digest(rows: &DigestRows, active: i64) -> u64 {
    let mut hasher = DefaultHasher::new();
    for episode in &rows.episodes {
        episode.hash(&mut hasher);
        ((active - episode.stalled_since_active) / MINUTE_MS).hash(&mut hasher);
        episode
            .next_escalate
            .map(|due| minutes_until(due, active))
            .hash(&mut hasher);
    }
    rows.holds.hash(&mut hasher);
    rows.settings.hash(&mut hasher);
    rows.stall_minutes.hash(&mut hasher);
    rows.gigas.hash(&mut hasher);
    hasher.finish()
}

fn current_digest(store: &SessionStore) -> Result<u64, String> {
    let conn = store.lock_conn()?;
    let (active, wall) = hold_service::now(store, &conn)
        .map_err(|error| error.to_string())?
        .closing();
    let rows = digest_rows(&conn, wall).map_err(|error| error.to_string())?;
    Ok(digest(&rows, active))
}

pub(crate) fn changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(CHANGED, ());
}

/// Emits [`CHANGED`] whenever the digest moves: on every hold change at
/// once, else at most every detector pass. Any process runs it; it only
/// reads.
pub(crate) fn watch<R: Runtime>(app: &AppHandle<R>) {
    let pass = Duration::from_millis(u64::try_from(super::PASS_MS).unwrap_or(5_000));
    let mut last = None;
    loop {
        let seen = hold_service::version();
        match current_digest(&app.state::<SessionStore>()) {
            Ok(digest) => {
                if last.is_some_and(|last| last != digest) {
                    changed(app);
                }
                last = Some(digest);
            }
            Err(error) => eprintln!("Could not read the alarms digest: {error}"),
        }
        hold_service::wait_change(seen, pass);
    }
}

fn switch(value: &str) -> Result<&'static str, String> {
    match value {
        "on" => Ok("on"),
        "off" => Ok("off"),
        _ => Err("The value must be on or off.".into()),
    }
}

/// The app user sets one global alarm setting (Settings > Alarms).
pub(crate) fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    let stored = match key {
        "stall_minutes" => match value.trim().parse::<i64>() {
            Ok(minutes) if STALL_MINUTES.contains(&minutes) => minutes.to_string(),
            _ => {
                return Err(format!(
                    "stall_minutes must be a whole number of minutes, {} to {}.",
                    STALL_MINUTES.start(),
                    STALL_MINUTES.end()
                ))
            }
        },
        "sleep_hold" | "tyler_sleep_hold" => switch(value)?.to_string(),
        "root_giga" => {
            let giga = MonoId::parse(value)
                .filter(|id| {
                    provisioner::gigas(conn)
                        .map(|gigas| gigas.iter().any(|giga| giga.id == *id))
                        .unwrap_or(false)
                })
                .ok_or_else(|| format!("No Giga has the id {value}."))?;
            giga.as_str().to_string()
        }
        _ => return Err(format!("{key} is not an alarm setting the app sets.")),
    };
    conn.execute(
        "INSERT INTO alarm_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = ?2",
        params![key, stored],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// The app user sets session `session`'s own stall minutes (a Mono's chat
/// sets its Mono's), or clears them with `None`.
pub(crate) fn set_stall_minutes(
    conn: &Connection,
    session: &str,
    minutes: Option<i64>,
) -> Result<(), String> {
    let saved: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
            [session],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let actor = act::caller(conn, session)
        .map_err(|error| error.to_string())?
        .filter(|_| saved)
        .ok_or_else(|| format!("MonoCode has no saved session {session}."))?;
    let written = match minutes {
        None => conn.execute(
            "DELETE FROM agent_alarm_settings WHERE actor = ?1",
            [actor.to_string()],
        ),
        Some(minutes) if STALL_MINUTES.contains(&minutes) => conn.execute(
            "INSERT INTO agent_alarm_settings (actor, stall_minutes) VALUES (?1, ?2)
             ON CONFLICT (actor) DO UPDATE SET stall_minutes = ?2",
            params![actor.to_string(), minutes],
        ),
        Some(_) => {
            return Err(format!(
                "minutes must be {} to {}, or null to clear them.",
                STALL_MINUTES.start(),
                STALL_MINUTES.end()
            ))
        }
    };
    written.map(drop).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn alarms_view(store: State<'_, SessionStore>) -> Result<AlarmsSnapshot, String> {
    let conn = store.lock_conn()?;
    let now = hold_service::now(&store, &conn)
        .map_err(|error| error.to_string())?
        .closing();
    snapshot(&conn, now).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn alarms_settings_set(
    app: AppHandle,
    store: State<'_, SessionStore>,
    key: String,
    value: String,
) -> Result<(), String> {
    set_setting(&*store.lock_conn()?, &key, &value)?;
    changed(&app);
    Ok(())
}

#[tauri::command(async)]
pub fn alarms_stall_minutes_set(
    app: AppHandle,
    store: State<'_, SessionStore>,
    session_id: String,
    minutes: Option<i64>,
) -> Result<(), String> {
    set_stall_minutes(&*store.lock_conn()?, &session_id, minutes)?;
    changed(&app);
    Ok(())
}

/// Tyler's act on a stalled agent from its own session's window: logged
/// here, its effect done by the front end (`act::tyler_act`).
#[tauri::command(async)]
pub fn alarms_tyler_act(
    app: AppHandle,
    store: State<'_, SessionStore>,
    alarm: String,
    how: String,
    detail: Option<String>,
    request_id: String,
) -> Value {
    let acted = store
        .lock_conn()
        .map_err(act::Refusal::from)
        .and_then(|conn| {
            let now = super::now(&store.alarms, &conn)?;
            act::tyler_act(&conn, &alarm, &how, detail.as_deref(), &request_id, now)
        });
    match acted {
        Ok(acted) => {
            if let Some(session) = &acted.answered_plan {
                store.alarms.plan_answered(session);
            }
            changed(&app);
            json!({"ok": true, "result": acted.result})
        }
        Err(refusal) => refusal.response(),
    }
}

/// Tyler's Allow or Deny on his hold's card in the app (frame 3).
#[tauri::command(async)]
pub fn holds_tyler_decide(
    app: AppHandle,
    store: State<'_, SessionStore>,
    hold: String,
    allow: bool,
    reason: Option<String>,
    request_id: String,
) -> Value {
    let decided = store
        .lock_conn()
        .map_err(act::Refusal::from)
        .and_then(|conn| {
            let now = hold_service::now(&store, &conn)?;
            hold_service::tyler_decides_in_app(
                &conn,
                &hold,
                allow,
                reason.as_deref(),
                &request_id,
                now,
            )
        });
    match decided {
        Ok(decided) => {
            changed(&app);
            json!({"ok": true, "decided": decided})
        }
        Err(refusal) => refusal.response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(id: &str) -> EpisodeDigest {
        EpisodeDigest {
            id: id.into(),
            level: 1,
            cause: "idle_open_work".into(),
            stalled_since_active: 0,
            next_escalate: Some(30 * MINUTE_MS),
            acted_by: None,
            acted_how: None,
            acted_active: None,
            dialog_text: None,
            dialog_request_id: None,
            sent: 0,
        }
    }

    fn rows() -> DigestRows {
        DigestRows {
            episodes: vec![open("al_1")],
            holds: vec![("sh_1".into(), None, None, None)],
            settings: vec![("stall_minutes".into(), "15".into())],
            stall_minutes: vec![("session:w".into(), 30)],
            gigas: vec![("general".into(), "Monocode General".into())],
        }
    }

    // Defect: the digest stays put when an episode opens, goes up, is acted
    // on, its minutes turn over, or a hold is decided, so the front end
    // shows a stale alarm; or it moves with nothing new, so the front end
    // reads the snapshot every pass for nothing.
    #[test]
    fn the_digest_moves_only_with_what_the_snapshot_shows() {
        let active = 15 * MINUTE_MS + 10_000;
        let base = digest(&rows(), active);
        assert_eq!(digest(&rows(), active), base, "nothing changed");
        assert_eq!(
            digest(&rows(), active + 5_000),
            base,
            "a pass within the same minute"
        );
        assert_ne!(
            digest(&rows(), 16 * MINUTE_MS),
            base,
            "the stalled minutes turn over"
        );
        let mut changed: Vec<(&str, DigestRows)> = Vec::new();
        let mut more = rows();
        more.episodes.push(open("al_2"));
        changed.push(("an episode opened", more));
        changed.push((
            "it closed",
            DigestRows {
                episodes: Vec::new(),
                ..rows()
            },
        ));
        let mut up = rows();
        up.episodes[0].level = 2;
        up.episodes[0].next_escalate = Some(45 * MINUTE_MS);
        changed.push(("it went up", up));
        let mut acted = rows();
        acted.episodes[0].acted_by = Some("tyler".into());
        acted.episodes[0].acted_how = Some("nudge".into());
        acted.episodes[0].acted_active = Some(active);
        changed.push(("acted", acted));
        let mut decided = rows();
        decided.holds[0] = (
            "sh_1".into(),
            Some("allow".into()),
            Some("tyler".into()),
            Some("c-1".into()),
        );
        changed.push(("a hold decided", decided));
        let mut set = rows();
        set.settings[0].1 = "30".into();
        changed.push(("a setting", set));
        let mut minutes = rows();
        minutes.stall_minutes.clear();
        changed.push(("an agent's stall minutes", minutes));
        for (what, rows) in changed {
            assert_ne!(digest(&rows, active), base, "{what}");
        }
    }

    // Defect: the minutes to the next level round down, so the card says
    // 0 min while a minute is left, or go negative once it is due.
    #[test]
    fn the_minutes_to_the_next_level_round_up_and_stop_at_zero() {
        assert_eq!(minutes_until(15 * MINUTE_MS, 0), 15);
        assert_eq!(minutes_until(15 * MINUTE_MS, 1), 15);
        assert_eq!(minutes_until(15 * MINUTE_MS, 14 * MINUTE_MS + 1), 1);
        assert_eq!(minutes_until(15 * MINUTE_MS, 15 * MINUTE_MS), 0);
        assert_eq!(minutes_until(15 * MINUTE_MS, 20 * MINUTE_MS), 0);
    }
}
