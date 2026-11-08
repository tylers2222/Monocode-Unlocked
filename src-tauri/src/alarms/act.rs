//! Design section 7: the `alarms.*` app actions. The caller must be the
//! episode's current recipient or a provisioner above it, as the stored
//! sends and the hierarchy rows say, never as the call says. Each caller
//! acts one level down its chain: a Mono steers its lead, never the
//! worker. Every act is a row of `alarm_acts` and stops escalation.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};

use super::provisioner::Provisioner;
use super::route::{self, EpisodeRow};
use super::store::{Settings, STALL_MINUTES};
use super::text;
use crate::ask_gate::Actor;
use crate::mono_messages::{self, MonoId, Notices, SessionId};

const MINUTE_MS: i64 = 60_000;
const TRANSCRIPT_BLOCKS: usize = 20;

/// Why an `alarms.*` call was refused: a code and a hint, as every other
/// app refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    InvalidInput(String),
    UnknownAlarm,
    AlarmClosed,
    /// The caller is not on the episode's chain, or is the stalled agent.
    NotYourAlarm,
    /// The caller was told at a lower level; the alarm went on to `current`.
    MovedUp {
        current: String,
    },
    /// Only the agent's own provisioner answers its dialog.
    AnswerViaRecipient {
        target: String,
    },
    DialogGone,
    PlanDialog,
    NotTheParent,
    MonoOnly,
    UnknownCaller,
    Failed(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        Refusal::Failed(error.to_string())
    }
}

impl Refusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Refusal::InvalidInput(_) => "invalid_input",
            Refusal::UnknownAlarm => "unknown_alarm",
            Refusal::AlarmClosed => "alarm_closed",
            Refusal::NotYourAlarm => "not_your_alarm",
            Refusal::MovedUp { .. } => "alarm_moved_up",
            Refusal::AnswerViaRecipient { .. } => "answer_via_recipient",
            Refusal::DialogGone => "dialog_gone",
            Refusal::PlanDialog => "plan_dialog",
            Refusal::NotTheParent => "not_the_parent",
            Refusal::MonoOnly => "mono_only",
            Refusal::UnknownCaller => "unknown_caller",
            Refusal::Failed(_) => "failed",
        }
    }

    fn hint(&self) -> String {
        match self {
            Refusal::InvalidInput(hint) => hint.clone(),
            Refusal::UnknownAlarm => "No alarm has this id. Run alarms.list for yours.".into(),
            Refusal::AlarmClosed => "This alarm is closed: the agent moved on, or the act had no effect and a new alarm opened. Run alarms.list.".into(),
            Refusal::NotYourAlarm => "This alarm is not yours: only its recipient or a provisioner above it may act on it. Run alarms.list for yours.".into(),
            Refusal::MovedUp { current } => format!("This alarm went up to {current}, so it no longer waits on you. Act on your own alarms (alarms.list)."),
            Refusal::AnswerViaRecipient { target } => format!("Only the stalled agent's own provisioner answers its dialog. Steer {target} with alarms.steer, or nudge it with alarms.nudge."),
            Refusal::DialogGone => "The dialog is no longer pending: it was answered or withdrawn. Read the alarm again with alarms.read.".into(),
            Refusal::PlanDialog => "A plan waits for the agent's next turn, not for an answer. Tell it what to do with alarms.steer.".into(),
            Refusal::NotTheParent => "Only the agent's own provisioner (its lead, its Mono, its Giga) may set its stall minutes.".into(),
            Refusal::MonoOnly => "Only a Mono or a Giga runs alarms.report.".into(),
            Refusal::UnknownCaller => "MonoCode has no saved session for this caller.".into(),
            Refusal::Failed(error) => error.clone(),
        }
    }

    pub(crate) fn response(&self) -> Value {
        let hint = self.hint();
        json!({"ok": false, "code": self.code(), "hint": hint, "error": hint, "retryable": false})
    }
}

/// Where the caller stands on an episode's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    NotOnChain,
    /// Told at a level below the current one.
    Below,
    /// The current recipient or a provisioner above it, at this index.
    At(usize),
}

/// `chain[0]` is the stalled agent, `chain[k]` the recipient of level k.
pub(crate) fn standing(chain: &[Provisioner], level: usize, caller: &Actor) -> Standing {
    let caller = Provisioner::Agent(caller.clone());
    let mut found = Standing::NotOnChain;
    for (index, link) in chain.iter().enumerate().skip(1) {
        if *link == caller {
            if index >= level {
                return Standing::At(index);
            }
            found = Standing::Below;
        }
    }
    found
}

/// The agent the caller at `index` acts on: the one just below it.
fn target_of(chain: &[Provisioner], index: usize) -> Option<&Actor> {
    match chain.get(index.checked_sub(1)?)? {
        Provisioner::Agent(actor) => Some(actor),
        Provisioner::Tyler => None,
    }
}

/// The actor a call from `session` speaks for: a Mono for its current chat.
pub(crate) fn caller(conn: &Connection, session: &str) -> rusqlite::Result<Option<Actor>> {
    let Some(session) = SessionId::parse(session) else {
        return Ok(None);
    };
    Ok(Some(match mono_messages::mono_of_chat(conn, &session)? {
        Some(mono) => Actor::Mono(mono),
        None => Actor::Session(session),
    }))
}

/// A `send_to` or `sessionId` value: a Mono by id when one exists, else a session.
fn named_actor(conn: &Connection, name: &str) -> rusqlite::Result<Option<Actor>> {
    if let Some(actor) = Actor::parse(name) {
        return Ok(Some(actor));
    }
    if let Some(mono) = MonoId::parse(name) {
        if mono_messages::mono_row(conn, &mono)?.is_some() {
            return Ok(Some(Actor::Mono(mono)));
        }
    }
    Ok(SessionId::parse(name).map(Actor::Session))
}

/// An app call's input: only `allowed` fields.
struct Input<'a>(&'a Map<String, Value>);

impl<'a> Input<'a> {
    fn new(input: &'a Value, allowed: &[&str]) -> Result<Self, Refusal> {
        let map = input
            .as_object()
            .ok_or_else(|| Refusal::InvalidInput("Input must be a JSON object.".into()))?;
        if let Some(unknown) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(Refusal::InvalidInput(format!(
                "Unknown field {unknown}. This action takes: {}.",
                if allowed.is_empty() {
                    "no fields".to_string()
                } else {
                    allowed.join(", ")
                }
            )));
        }
        Ok(Self(map))
    }

    fn text(&self, field: &str, max: usize) -> Result<Option<String>, Refusal> {
        match self.0.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) if !text.trim().is_empty() && text.chars().count() <= max => {
                Ok(Some(text.clone()))
            }
            Some(_) => Err(Refusal::InvalidInput(format!(
                "{field} must be a string of 1 to {max} characters."
            ))),
        }
    }

    fn required(&self, field: &str, max: usize) -> Result<String, Refusal> {
        self.text(field, max)?
            .ok_or_else(|| Refusal::InvalidInput(format!("{field} is required.")))
    }

    fn integer(&self, field: &str) -> Result<i64, Refusal> {
        self.0
            .get(field)
            .and_then(Value::as_i64)
            .ok_or_else(|| Refusal::InvalidInput(format!("{field} must be an integer.")))
    }

    fn flag(&self, field: &str) -> Result<bool, Refusal> {
        match self.0.get(field) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(false),
            Some(Value::Bool(true)) => Ok(true),
            Some(_) => Err(Refusal::InvalidInput(format!("{field} must be true."))),
        }
    }
}

/// The episode a call names, its chain, and where the caller stands on it.
struct OnChain {
    episode: EpisodeRow,
    chain: Vec<Provisioner>,
    index: usize,
}

fn on_chain(
    conn: &Connection,
    settings: &Settings,
    id: &str,
    caller: &Actor,
) -> Result<OnChain, Refusal> {
    let episode = route::episode(conn, id)?.ok_or(Refusal::UnknownAlarm)?;
    let chain = route::chain(conn, &episode, settings.root_giga.as_deref())?;
    let level = usize::try_from(episode.level).unwrap_or(usize::MAX);
    match standing(&chain, level, caller) {
        Standing::NotOnChain => Err(Refusal::NotYourAlarm),
        Standing::Below => Err(Refusal::MovedUp {
            current: match chain.get(level) {
                Some(Provisioner::Agent(actor)) => route::who(conn, actor)?.label,
                _ => "Tyler".into(),
            },
        }),
        Standing::At(_) if !episode.open => Err(Refusal::AlarmClosed),
        Standing::At(index) => Ok(OnChain {
            episode,
            chain,
            index,
        }),
    }
}

/// What an `alarms.*` call does next.
pub(crate) enum Outcome {
    Done {
        result: Value,
        notices: Notices,
    },
    /// `alarms.answer`: the window that owns the agent's session answers
    /// its dialog with `forward`, then `answered` logs the act.
    Answer(AnswerPlan),
}

#[derive(Debug, Clone)]
pub(crate) struct AnswerPlan {
    pub episode: String,
    pub caller: Actor,
    pub target: Actor,
    /// The agent's session, which the owning window answers in.
    pub session: String,
    pub forward: Value,
    pub detail: String,
    pub request_id: String,
}

fn done(result: Value) -> Outcome {
    Outcome::Done {
        result,
        notices: Notices::default(),
    }
}

/// One `alarms.*` call from `caller_session` at `(active, wall)`.
pub(crate) fn call(
    conn: &Connection,
    caller_session: &str,
    action: &str,
    input: &Value,
    request_id: &str,
    now: (i64, i64),
) -> Result<Outcome, Refusal> {
    let caller = caller(conn, caller_session)?.ok_or(Refusal::UnknownCaller)?;
    let settings = Settings::load(conn)?;
    match action {
        "alarms.list" => {
            Input::new(input, &[])?;
            list(conn, &settings, &caller, now).map(done)
        }
        "alarms.report" => {
            let input = Input::new(input, &["from", "to"])?;
            if !matches!(caller, Actor::Mono(_)) {
                return Err(Refusal::MonoOnly);
            }
            report(conn, input.integer("from")?, input.integer("to")?).map(done)
        }
        "alarms.set" => {
            let input = Input::new(input, &["agent", "stall_minutes"])?;
            set(
                conn,
                &settings,
                &caller,
                &input.required("agent", 256)?,
                input.integer("stall_minutes")?,
            )
            .map(done)
        }
        "alarms.read" => {
            let input = Input::new(input, &["alarm"])?;
            let found = on_chain(conn, &settings, &input.required("alarm", 64)?, &caller)?;
            read(conn, &settings, &found, now).map(done)
        }
        "alarms.answer" => {
            let input = Input::new(input, &["alarm", "allow", "deny", "text", "choice"])?;
            let found = on_chain(conn, &settings, &input.required("alarm", 64)?, &caller)?;
            answer(conn, &found, &caller, &input, request_id).map(Outcome::Answer)
        }
        "alarms.steer" | "alarms.nudge" | "alarms.dismiss" => {
            let fields: &[&str] = match action {
                "alarms.steer" => &["alarm", "text"],
                "alarms.dismiss" => &["alarm", "reason"],
                _ => &["alarm"],
            };
            let input = Input::new(input, fields)?;
            let found = on_chain(conn, &settings, &input.required("alarm", 64)?, &caller)?;
            let act = match action {
                "alarms.steer" => Act::Steer(input.required("text", 8000)?),
                "alarms.dismiss" => Act::Dismiss(input.required("reason", 2000)?),
                _ => Act::Nudge,
            };
            act_on(conn, &settings, &found, &caller, act, request_id, now)
        }
        _ => Err(Refusal::InvalidInput(format!(
            "Unknown action {action}. Run app --help for the alarms.* actions."
        ))),
    }
}

fn list(
    conn: &Connection,
    settings: &Settings,
    caller: &Actor,
    (active, _): (i64, i64),
) -> Result<Value, Refusal> {
    let mut alarms = Vec::new();
    for episode in route::open_episodes(conn)? {
        let chain = route::chain(conn, &episode, settings.root_giga.as_deref())?;
        let level = usize::try_from(episode.level).unwrap_or(usize::MAX);
        let Standing::At(index) = standing(&chain, level, caller) else {
            continue;
        };
        alarms.push(json!({
            "alarm": episode.id,
            "agent": episode.actor.to_string(),
            "agentLabel": route::who(conn, &episode.actor)?.label,
            "cause": episode.cause,
            "level": episode.level,
            "stalledMinutes": (active - episode.stalled_since_active) / MINUTE_MS,
            "actsOn": target_of(&chain, index).map(ToString::to_string),
            "actedBy": episode.acted_by,
            "actedHow": episode.acted_how,
        }));
    }
    Ok(json!({ "alarms": alarms }))
}

/// Design section 8, the Mono Reviewer query: stall minutes per Mono over
/// the wall window `[from, to)`.
pub(crate) fn report(conn: &Connection, from: i64, to: i64) -> Result<Value, Refusal> {
    if to <= from {
        return Err(Refusal::InvalidInput(
            "to must be after from (wall ms).".into(),
        ));
    }
    let rows = conn
        .prepare(
            "SELECT mono_id, COUNT(*) AS episodes,
               SUM(MIN(COALESCE(closed_wall_ms, :to), :to) - MAX(stalled_since_wall_ms, :from)) / 60000.0
                 AS stall_minutes,
               SUM(acted_by IS NOT NULL) AS acted, MAX(level) AS max_level
             FROM alarm_episodes
             WHERE stalled_since_wall_ms < :to AND COALESCE(closed_wall_ms, :to) > :from
             GROUP BY mono_id ORDER BY stall_minutes DESC",
        )?
        .query_map(
            rusqlite::named_params! {":from": from, ":to": to},
            |row| {
                Ok(json!({
                    "monoId": row.get::<_, Option<String>>(0)?,
                    "episodes": row.get::<_, i64>(1)?,
                    "stallMinutes": row.get::<_, f64>(2)?,
                    "acted": row.get::<_, i64>(3)?,
                    "maxLevel": row.get::<_, i64>(4)?,
                }))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({ "from": from, "to": to, "monos": rows }))
}

fn set(
    conn: &Connection,
    settings: &Settings,
    caller: &Actor,
    agent: &str,
    stall_minutes: i64,
) -> Result<Value, Refusal> {
    if !STALL_MINUTES.contains(&stall_minutes) {
        return Err(Refusal::InvalidInput(format!(
            "stall_minutes must be {} to {}.",
            STALL_MINUTES.start(),
            STALL_MINUTES.end()
        )));
    }
    let agent = named_actor(conn, agent)?
        .ok_or_else(|| Refusal::InvalidInput("agent must name a session or a Mono.".into()))?;
    let parent = route::provisioner_of(conn, &agent, settings.root_giga.as_deref())?;
    if parent != Provisioner::Agent(caller.clone()) {
        return Err(Refusal::NotTheParent);
    }
    conn.execute(
        "INSERT INTO agent_alarm_settings (actor, stall_minutes) VALUES (?1, ?2)
         ON CONFLICT (actor) DO UPDATE SET stall_minutes = ?2",
        params![agent.to_string(), stall_minutes],
    )?;
    Ok(json!({ "agent": agent.to_string(), "stall_minutes": stall_minutes }))
}

/// The last blocks of the agent's saved transcript, tools included.
fn transcript(conn: &Connection, actor: &Actor) -> rusqlite::Result<Vec<Value>> {
    let Some(session) = route::session_of(conn, actor)? else {
        return Ok(Vec::new());
    };
    let blocks: Option<String> = conn
        .query_row(
            "SELECT blocks_json FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let blocks: Vec<Value> = blocks
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let skip = blocks.len().saturating_sub(TRANSCRIPT_BLOCKS);
    Ok(blocks
        .iter()
        .skip(skip)
        .map(|block| {
            let mut row = json!({
                "role": block["role"],
                "text": route::short(block["text"].as_str().unwrap_or_default(), 400),
            });
            if let Some(tool) = block.get("tool") {
                row["tool"] = json!({
                    "title": tool["title"].as_str().or_else(|| tool["kind"].as_str()),
                    "status": tool["status"],
                });
            }
            row
        })
        .collect())
}

fn read(
    conn: &Connection,
    settings: &Settings,
    found: &OnChain,
    (active, wall): (i64, i64),
) -> Result<Value, Refusal> {
    let episode = &found.episode;
    let stall = route::stall(conn, episode, settings, active, wall)?;
    let sends = conn
        .prepare(
            "SELECT level, recipient, sent_wall_ms FROM alarm_sends
              WHERE episode_id = ?1 ORDER BY level",
        )?
        .query_map([&episode.id], |row| {
            Ok(json!({
                "level": row.get::<_, i64>(0)?,
                "recipient": row.get::<_, String>(1)?,
                "sentWallMs": row.get::<_, Option<i64>>(2)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let acts = conn
        .prepare(
            "SELECT seq, actor, target, how, detail, at_wall_ms FROM alarm_acts
              WHERE episode_id = ?1 ORDER BY seq",
        )?
        .query_map([&episode.id], |row| {
            Ok(json!({
                "seq": row.get::<_, i64>(0)?,
                "by": row.get::<_, String>(1)?,
                "target": row.get::<_, String>(2)?,
                "how": row.get::<_, String>(3)?,
                "detail": row.get::<_, Option<String>>(4)?,
                "atWallMs": row.get::<_, i64>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let session = route::session_of(conn, &episode.actor)?;
    Ok(json!({
        "alarm": episode.id,
        "agent": stall.agent.actor,
        "agentLabel": stall.agent.label,
        "sessionId": session.as_ref().map(SessionId::as_str),
        "cause": episode.cause,
        "level": episode.level,
        "stalledSince": stall.since,
        "stalledMinutes": stall.minutes,
        "openWork": stall.open_work,
        "lastActivity": stall.last_activity,
        "dialog": stall.dialog.as_ref().map(|dialog| json!({
            "kind": dialog.kind, "minutes": dialog.minutes, "text": dialog.text,
        })),
        "actsOn": target_of(&found.chain, found.index).map(ToString::to_string),
        "sends": sends,
        "acts": acts,
        "transcript": transcript(conn, &episode.actor)?,
    }))
}

fn pending_dialog(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<String>> {
    let Some(session) = route::session_of(conn, actor)? else {
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

fn answer(
    conn: &Connection,
    found: &OnChain,
    caller: &Actor,
    input: &Input,
    request_id: &str,
) -> Result<AnswerPlan, Refusal> {
    let agent = &found.episode.actor;
    let target = target_of(&found.chain, found.index).ok_or(Refusal::NotYourAlarm)?;
    if target != agent {
        return Err(Refusal::AnswerViaRecipient {
            target: route::who(conn, target)?.label,
        });
    }
    let (allow, deny) = (input.flag("allow")?, input.flag("deny")?);
    let (text, choice) = (input.text("text", 8000)?, input.text("choice", 500)?);
    let given = [allow, deny, text.is_some(), choice.is_some()]
        .iter()
        .filter(|given| **given)
        .count();
    if given != 1 {
        return Err(Refusal::InvalidInput(
            "Give exactly one of allow, deny, text or choice.".into(),
        ));
    }
    let kind = pending_dialog(conn, agent)?.ok_or(Refusal::DialogGone)?;
    let id = &found.episode.id;
    let (forward, detail) = match kind.as_str() {
        "plan" => return Err(Refusal::PlanDialog),
        "question" => match (text, choice) {
            (Some(text), _) => (json!({"alarm": id, "kind": "question", "text": text}), text),
            (None, Some(choice)) => (
                json!({"alarm": id, "kind": "question", "choice": choice}),
                choice,
            ),
            (None, None) => {
                return Err(Refusal::InvalidInput(
                    "The dialog is a question: answer it with text or choice.".into(),
                ))
            }
        },
        _ if allow || deny => {
            let decision = if allow { "allow" } else { "deny" };
            (
                json!({"alarm": id, "kind": "permission", "decision": decision}),
                decision.to_string(),
            )
        }
        _ => {
            return Err(Refusal::InvalidInput(
                "The dialog is a permission: answer it with allow or deny.".into(),
            ))
        }
    };
    let session = route::session_of(conn, agent)?.ok_or(Refusal::DialogGone)?;
    Ok(AnswerPlan {
        episode: id.clone(),
        caller: caller.clone(),
        target: agent.clone(),
        session: session.as_str().to_string(),
        forward,
        detail,
        request_id: request_id.to_string(),
    })
}

enum Act {
    Steer(String),
    Nudge,
    Dismiss(String),
}

/// A row an act writes: what it did, to whom, and the outbox prompt it
/// sends that one, if any.
struct Logged<'a> {
    caller: &'a Actor,
    target: &'a Actor,
    how: &'a str,
    detail: Option<&'a str>,
    prompt: Option<String>,
    request_id: &'a str,
}

/// The act `caller` logged on `episode` under `request_id` before, as its
/// answer.
fn logged_before(
    conn: &Connection,
    episode: &str,
    caller: &Actor,
    request_id: &str,
) -> rusqlite::Result<Option<Value>> {
    conn.query_row(
        "SELECT episode_id, seq, how, target FROM alarm_acts
          WHERE episode_id = ?1 AND actor = ?2 AND request_id = ?3",
        params![episode, caller.to_string(), request_id],
        |row| {
            Ok(json!({
                "alarm": row.get::<_, String>(0)?,
                "seq": row.get::<_, i64>(1)?,
                "acted": row.get::<_, String>(2)?,
                "target": row.get::<_, String>(3)?,
                "duplicate": true,
            }))
        },
    )
    .optional()
}

/// Logs one act on `episode` and marks it acted, so escalation stops.
fn log(
    tx: &Connection,
    episode: &EpisodeRow,
    act: &Logged,
    notices: &mut Notices,
    (active, wall): (i64, i64),
) -> rusqlite::Result<Value> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM alarm_acts WHERE episode_id = ?1",
        [&episode.id],
        |row| row.get(0),
    )?;
    let key = act
        .prompt
        .as_ref()
        .map(|_| format!("nudge:{}:{seq}", episode.id));
    tx.execute(
        "INSERT INTO alarm_acts (episode_id, seq, level, actor, target, how, detail, outbox_key,
           request_id, at_active_ms, at_wall_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            episode.id,
            seq,
            episode.level,
            act.caller.to_string(),
            act.target.to_string(),
            act.how,
            act.detail,
            key,
            act.request_id,
            active,
            wall
        ],
    )?;
    tx.execute(
        "UPDATE alarm_episodes SET acted_by = ?2, acted_how = ?3, acted_active_ms = ?4
          WHERE id = ?1",
        params![episode.id, act.caller.to_string(), act.how, active],
    )?;
    if let (Some(key), Some(prompt)) = (&key, &act.prompt) {
        let target = act.target.to_string();
        tx.execute(
            "INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms)
             VALUES (?1, NULL, 'nudge', ?2, ?3)",
            params![key, target, active],
        )?;
        tx.execute(
            "INSERT INTO alarm_prompts (outbox_key, episode_id, text) VALUES (?1, ?2, ?3)",
            params![key, episode.id, prompt],
        )?;
        crate::ask_scheduler::name_chat(tx, notices, &target)?;
    }
    Ok(json!({
        "alarm": episode.id,
        "seq": seq,
        "acted": act.how,
        "target": act.target.to_string(),
    }))
}

fn logged_in_tx(
    conn: &Connection,
    episode: &EpisodeRow,
    act: &Logged,
    now: (i64, i64),
) -> rusqlite::Result<(Value, Notices)> {
    if let Some(result) = logged_before(conn, &episode.id, act.caller, act.request_id)? {
        return Ok((result, Notices::default()));
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut notices = Notices::default();
    let result = log(&tx, episode, act, &mut notices, now)?;
    tx.commit()?;
    Ok((result, notices))
}

fn act_on(
    conn: &Connection,
    settings: &Settings,
    found: &OnChain,
    caller: &Actor,
    act: Act,
    request_id: &str,
    (active, wall): (i64, i64),
) -> Result<Outcome, Refusal> {
    let episode = &found.episode;
    let target = target_of(&found.chain, found.index).ok_or(Refusal::NotYourAlarm)?;
    let from = caller.to_string();
    let (how, detail, prompt) = match &act {
        Act::Steer(text) => (
            "steer",
            Some(text.as_str()),
            Some(text::steer(&episode.id, &from, text)),
        ),
        Act::Nudge => {
            let stall = route::stall(conn, episode, settings, active, wall)?;
            (
                "nudge",
                None,
                Some(text::nudge(&stall, &from, target == &episode.actor)),
            )
        }
        Act::Dismiss(reason) => ("dismiss", Some(reason.as_str()), None),
    };
    let (result, notices) = logged_in_tx(
        conn,
        episode,
        &Logged {
            caller,
            target,
            how,
            detail,
            prompt,
            request_id,
        },
        (active, wall),
    )?;
    Ok(Outcome::Done { result, notices })
}

/// The agent's window answered its dialog for `plan`: the act is logged.
pub(crate) fn answered(
    conn: &Connection,
    plan: &AnswerPlan,
    now: (i64, i64),
) -> Result<Value, Refusal> {
    let episode = route::episode(conn, &plan.episode)?.ok_or(Refusal::UnknownAlarm)?;
    let logged = logged_in_tx(
        conn,
        &episode,
        &Logged {
            caller: &plan.caller,
            target: &plan.target,
            how: "answer",
            detail: Some(&plan.detail),
            prompt: None,
            request_id: &plan.request_id,
        },
        now,
    )?;
    Ok(logged.0)
}

/// A plain `messages.send` or `sessions.send` from `caller_session` to
/// `to` was accepted: it is an act (`message`) on each open episode where
/// the caller stands at or above the current level and `to` is the one
/// it acts on.
pub(crate) fn message(
    conn: &Connection,
    caller_session: &str,
    to: &str,
    request_id: &str,
    now: (i64, i64),
) -> rusqlite::Result<()> {
    let (Some(caller), Some(to)) = (caller(conn, caller_session)?, named_actor(conn, to)?) else {
        return Ok(());
    };
    let settings = Settings::load(conn)?;
    for episode in route::open_episodes(conn)? {
        let chain = route::chain(conn, &episode, settings.root_giga.as_deref())?;
        let level = usize::try_from(episode.level).unwrap_or(usize::MAX);
        let Standing::At(index) = standing(&chain, level, &caller) else {
            continue;
        };
        if target_of(&chain, index) != Some(&to) {
            continue;
        }
        let act = Logged {
            caller: &caller,
            target: &to,
            how: "message",
            detail: None,
            prompt: None,
            request_id,
        };
        if let Err(error) = logged_in_tx(conn, &episode, &act, now) {
            eprintln!("Could not log a message act on {}: {error}", episode.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> Actor {
        Actor::Session(SessionId::parse(id).unwrap())
    }

    fn mono(id: &str) -> Actor {
        Actor::Mono(MonoId::parse(id).unwrap())
    }

    // Defect: a caller below the current level, the stalled agent, or a
    // stranger acts on an alarm; or the current recipient or one above it
    // is refused; or a caller acts on anyone but the one just below it.
    #[test]
    fn only_the_current_recipient_or_one_above_acts_and_each_acts_one_level_down() {
        let (worker, lead, pr_mono, giga, stranger) = (
            session("worker"),
            session("lead"),
            mono("pr-mono"),
            mono("general"),
            mono("other"),
        );
        let chain = vec![
            Provisioner::Agent(worker.clone()),
            Provisioner::Agent(lead.clone()),
            Provisioner::Agent(pr_mono.clone()),
            Provisioner::Agent(giga.clone()),
            Provisioner::Tyler,
        ];
        // Rows: caller; columns: episode level 1..=4 (4 is Tyler's).
        let table: [(&Actor, [Standing; 4]); 5] = [
            (&worker, [Standing::NotOnChain; 4]),
            (
                &lead,
                [
                    Standing::At(1),
                    Standing::Below,
                    Standing::Below,
                    Standing::Below,
                ],
            ),
            (
                &pr_mono,
                [
                    Standing::At(2),
                    Standing::At(2),
                    Standing::Below,
                    Standing::Below,
                ],
            ),
            (
                &giga,
                [
                    Standing::At(3),
                    Standing::At(3),
                    Standing::At(3),
                    Standing::Below,
                ],
            ),
            (&stranger, [Standing::NotOnChain; 4]),
        ];
        for (caller, row) in table {
            for (level, expected) in (1..=4).zip(row) {
                assert_eq!(
                    standing(&chain, level, caller),
                    expected,
                    "{caller} at level {level}"
                );
            }
        }
        assert_eq!(
            target_of(&chain, 1),
            Some(&worker),
            "the lead acts on its worker"
        );
        assert_eq!(
            target_of(&chain, 2),
            Some(&lead),
            "the Mono acts on its lead"
        );
        assert_eq!(
            target_of(&chain, 3),
            Some(&pr_mono),
            "the Giga acts on its Mono"
        );
    }

    // Defect: a refusal reaches the caller without a code and hint, or as
    // retryable.
    #[test]
    fn a_refusal_has_a_code_and_a_hint_like_every_app_refusal() {
        let response = Refusal::MovedUp {
            current: "Fork Orchestrator".into(),
        }
        .response();
        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "alarm_moved_up");
        assert_eq!(response["retryable"], false);
        assert!(response["hint"]
            .as_str()
            .unwrap()
            .contains("Fork Orchestrator"));
        assert_eq!(response["error"], response["hint"]);
    }
}
