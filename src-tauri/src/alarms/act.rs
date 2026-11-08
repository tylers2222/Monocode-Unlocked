//! Design section 7: the `alarms.*` app actions. The caller must be on the
//! episode's chain, as the stored sends and the hierarchy rows say, never
//! as the call says. Acting rights accumulate (lead ruling): every
//! recipient told so far, and every provisioner above, may act. Each
//! caller acts one level down its chain: a Mono steers its lead, never the
//! worker. Every act is a row of `alarm_acts` and stops escalation.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use super::deliver::{deliver, logged_before, Caller, Delivery};
use super::provisioner::Provisioner;
use super::rights::{authorize, standing, target_of, Denied, How};
use super::route::{self, EpisodeRow};
use super::store::{Settings, MINUTE_MS, STALL_MINUTES};
use super::text;
use crate::ask_gate::Actor;
use crate::mono_messages::{self, MonoId, Notices, SessionId};
use crate::sleep_hold::store::Decision;

const TRANSCRIPT_BLOCKS: usize = 20;

/// Why an `alarms.*`, `holds.*` or `hold` call was refused: a code and a
/// hint, as every other app refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    InvalidInput(String),
    UnknownAlarm,
    AlarmClosed,
    /// The caller is not on the episode's chain, or is the stalled agent.
    NotYourAlarm,
    /// Only the current recipient or one above dismisses; the alarm went
    /// on to `current`.
    MovedUp {
        current: String,
    },
    /// Only the agent's own provisioner answers its dialog.
    AnswerViaRecipient {
        target: String,
    },
    DialogGone,
    NotTheParent,
    MonoOnly,
    UnknownHold,
    /// The caller is not the hold's provisioner or above it.
    NotYourHold,
    /// The first decision stands.
    HoldDecided(Decision),
    UnknownCaller,
    /// The store failed; the error goes to the log, not to the agent.
    Failed,
}

impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        eprintln!("An alarms call failed: {error}");
        Refusal::Failed
    }
}

impl From<String> for Refusal {
    fn from(error: String) -> Self {
        eprintln!("An alarms call failed: {error}");
        Refusal::Failed
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
            Refusal::NotTheParent => "not_the_parent",
            Refusal::MonoOnly => "mono_only",
            Refusal::UnknownHold => "unknown_hold",
            Refusal::NotYourHold => "not_your_hold",
            Refusal::HoldDecided(_) => "hold_decided",
            Refusal::UnknownCaller => "unknown_caller",
            Refusal::Failed => "failed",
        }
    }

    fn hint(&self) -> String {
        match self {
            Refusal::InvalidInput(hint) => hint.clone(),
            Refusal::UnknownAlarm => "No alarm has this id. Run alarms.list for yours.".into(),
            Refusal::AlarmClosed => "This alarm is closed: the agent moved on, or the act had no effect and a new alarm opened. Run alarms.list.".into(),
            Refusal::NotYourAlarm => "This alarm is not yours: only its recipient or a provisioner above it may act on it. Run alarms.list for yours.".into(),
            Refusal::MovedUp { current } => format!("This alarm went up to {current}, so only {current} or one above it may dismiss it. You may still read, nudge or steer."),
            Refusal::AnswerViaRecipient { target } => format!("Only the stalled agent's own provisioner answers its dialog. Steer {target} with alarms.steer, or nudge it with alarms.nudge."),
            Refusal::DialogGone => "The dialog is no longer pending: it was answered or withdrawn. Read the alarm again with alarms.read.".into(),
            Refusal::NotTheParent => "Only the agent's own provisioner (its lead, its Mono, its Giga) may set its stall minutes.".into(),
            Refusal::MonoOnly => "Only a Mono or a Giga runs alarms.report.".into(),
            Refusal::UnknownHold => "No sleep hold has this id.".into(),
            Refusal::NotYourHold => "This sleep hold is not yours: only the agent's provisioner or one above it may answer it.".into(),
            Refusal::HoldDecided(Decision::Timeout) => "This sleep hold timed out: no answer came in 2 min, so the agent was refused. It was told to run the wait in the background.".into(),
            Refusal::HoldDecided(decision) => format!("This sleep hold is decided already ({}). The first answer stands.", decision.as_str()),
            Refusal::UnknownCaller => "MonoCode has no saved session for this caller.".into(),
            Refusal::Failed => "MonoCode could not read or write its alarm rows. Try again.".into(),
        }
    }

    pub(crate) fn response(&self) -> Value {
        let hint = self.hint();
        json!({"ok": false, "code": self.code(), "hint": hint, "error": hint, "retryable": false})
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
pub(crate) fn named_actor(conn: &Connection, name: &str) -> rusqlite::Result<Option<Actor>> {
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

/// The episode a call names, its chain, where the caller stands on it, and
/// whom it acts on.
struct OnChain {
    episode: EpisodeRow,
    chain: Vec<Provisioner>,
    index: usize,
    target: Actor,
}

/// A call on one episode: the act this request already logged, or the
/// episode to act on.
enum Found {
    Before(Value),
    Open(Box<OnChain>),
}

fn on_chain(
    conn: &Connection,
    settings: &Settings,
    id: &str,
    caller: &Actor,
    how: How,
    request_id: &str,
) -> Result<Found, Refusal> {
    let episode = route::episode(conn, id)?.ok_or(Refusal::UnknownAlarm)?;
    let chain = route::chain(conn, &episode, settings.root_giga.as_deref())?;
    let index = standing(&chain, caller).ok_or(Refusal::NotYourAlarm)?;
    // A retry answers as the first call did, even once the act closed it.
    if how != How::Read {
        if let Some(before) = logged_before(conn, &episode.id, caller, request_id)? {
            return Ok(Found::Before(before));
        }
    }
    if !episode.open {
        return Err(Refusal::AlarmClosed);
    }
    let level = usize::try_from(episode.level).unwrap_or(usize::MAX);
    let target = match authorize(&chain, level, index, how) {
        Ok(target) => target.clone(),
        Err(Denied::NotOnChain) => return Err(Refusal::NotYourAlarm),
        Err(Denied::MovedUp) => {
            return Err(Refusal::MovedUp {
                current: match chain.get(level) {
                    Some(Provisioner::Agent(actor)) => route::who(conn, actor)?.label,
                    _ => "Tyler".into(),
                },
            })
        }
        Err(Denied::AnswerViaRecipient) => {
            return Err(Refusal::AnswerViaRecipient {
                target: target_of(&chain, index)
                    .map(|target| route::who(conn, target))
                    .transpose()?
                    .map_or_else(|| "Tyler".into(), |who| who.label),
            })
        }
    };
    Ok(Found::Open(Box::new(OnChain {
        episode,
        chain,
        index,
        target,
    })))
}

/// What an `alarms.*` call does next.
pub(crate) enum Outcome {
    Done {
        result: Value,
        notices: Notices,
        /// The session whose plan this act answered.
        answered_plan: Option<SessionId>,
    },
    /// A window does the act as `session`, then `forwarded` logs it.
    Forward(Forward),
}

/// An act a window does: `alarms.answer` in the window that owns the
/// agent's session, answered as the user answers; or a steer or nudge as
/// the caller's own `sessions.send` with `whenBusy: "steer"`, which steers
/// a running turn and starts one when the session is idle.
#[derive(Debug, Clone)]
pub(crate) struct Forward {
    pub episode: String,
    pub caller: Actor,
    pub target: Actor,
    /// The session the window acts as, and in whose window.
    pub session: String,
    pub action: &'static str,
    pub input: Value,
    pub how: &'static str,
    pub detail: String,
    pub request_id: String,
    /// The session whose plan this act answers.
    pub answered_plan: Option<SessionId>,
}

fn done_or(
    found: Found,
    act: impl FnOnce(OnChain) -> Result<Outcome, Refusal>,
) -> Result<Outcome, Refusal> {
    match found {
        Found::Before(result) => Ok(done(result)),
        Found::Open(on_chain) => act(*on_chain),
    }
}

fn done(result: Value) -> Outcome {
    Outcome::Done {
        result,
        notices: Notices::default(),
        answered_plan: None,
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
    let by = Caller {
        actor: &caller,
        session: caller_session,
        request_id,
    };
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
            let found = on_chain(
                conn,
                &settings,
                &input.required("alarm", 64)?,
                &caller,
                How::Read,
                request_id,
            )?;
            done_or(found, |found| {
                read(conn, &settings, &found, now.0).map(done)
            })
        }
        "alarms.answer" => {
            let input = Input::new(
                input,
                &["alarm", "allow", "deny", "reason", "text", "choice"],
            )?;
            let found = on_chain(
                conn,
                &settings,
                &input.required("alarm", 64)?,
                &caller,
                How::Answer,
                request_id,
            )?;
            done_or(found, |found| answer(conn, &found, &by, &input, now))
        }
        "alarms.steer" | "alarms.nudge" | "alarms.dismiss" => {
            let (fields, how): (&[&str], How) = match action {
                "alarms.steer" => (&["alarm", "text"], How::Steer),
                "alarms.dismiss" => (&["alarm", "reason"], How::Dismiss),
                _ => (&["alarm"], How::Nudge),
            };
            let input = Input::new(input, fields)?;
            let act = match how {
                How::Steer => Act::Steer(input.required("text", 8000)?),
                How::Dismiss => Act::Dismiss(input.required("reason", 2000)?),
                _ => Act::Nudge,
            };
            let found = on_chain(
                conn,
                &settings,
                &input.required("alarm", 64)?,
                &caller,
                how,
                request_id,
            )?;
            done_or(found, |found| {
                act_on(conn, &settings, &found, &by, act, now)
            })
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
        let Some(index) = standing(&chain, caller) else {
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
    active: i64,
) -> Result<Value, Refusal> {
    let episode = &found.episode;
    let stall = route::stall(conn, episode, settings, active)?;
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

/// The kind and request id of the dialog the agent waits on now.
fn pending_dialog(
    conn: &Connection,
    actor: &Actor,
) -> rusqlite::Result<Option<(String, Option<String>)>> {
    let Some(session) = route::session_of(conn, actor)? else {
        return Ok(None);
    };
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT dialog_kind, dialog_request_id FROM agent_activity WHERE session_id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(kind, id)| Some((kind?, id))))
}

fn answer(
    conn: &Connection,
    found: &OnChain,
    by: &Caller,
    input: &Input,
    now: (i64, i64),
) -> Result<Outcome, Refusal> {
    let agent = &found.episode.actor;
    let (allow, deny) = (input.flag("allow")?, input.flag("deny")?);
    let (text, choice) = (input.text("text", 8000)?, input.text("choice", 500)?);
    let reason = input.text("reason", 2000)?;
    let given = [allow, deny, text.is_some(), choice.is_some()]
        .iter()
        .filter(|given| **given)
        .count();
    if given != 1 {
        return Err(Refusal::InvalidInput(
            "Give exactly one of allow, deny, text or choice.".into(),
        ));
    }
    let (kind, pending) = pending_dialog(conn, agent)?.ok_or(Refusal::DialogGone)?;
    if reason.is_some() && !(kind == "plan" && deny) {
        return Err(Refusal::InvalidInput(
            "A reason goes only with deny on a plan.".into(),
        ));
    }
    let id = &found.episode.id;
    // The answer is for the dialog the alarm named, never a newer one.
    let shown = route::dialog_shown(conn, id)?;
    if kind == "plan" {
        // A plan has no request id: the alarm named it if it showed a
        // dialog with none.
        if shown.is_some() || found.episode.cause != "dialog" {
            return Err(Refusal::DialogGone);
        }
        return answer_plan(conn, found, by, allow, reason, now);
    }
    if shown.is_none() || pending != shown {
        return Err(Refusal::DialogGone);
    }
    let (mut forward, detail) = match kind.as_str() {
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
    forward["dialogRequestId"] = json!(shown);
    let session = route::session_of(conn, agent)?.ok_or(Refusal::DialogGone)?;
    Ok(Outcome::Forward(Forward {
        episode: id.clone(),
        caller: by.actor.clone(),
        target: agent.clone(),
        session: session.as_str().to_string(),
        action: "alarms.answer",
        input: forward,
        how: "answer",
        detail,
        request_id: by.request_id.to_string(),
        answered_plan: None,
    }))
}

/// Lead ruling (S2 r2 m1): a plan waits for no harness answer, so the
/// answer is a new message to the agent.
fn answer_plan(
    conn: &Connection,
    found: &OnChain,
    by: &Caller,
    allow: bool,
    reason: Option<String>,
    now: (i64, i64),
) -> Result<Outcome, Refusal> {
    let prompt = match (allow, reason) {
        (true, _) => "Your plan is approved. Implement it.".to_string(),
        (false, Some(reason)) => format!(
            "Your plan is not approved: {}. Revise it.",
            reason.trim().trim_end_matches('.')
        ),
        (false, None) => {
            return Err(Refusal::InvalidInput(
                "The dialog is a plan: answer it with allow, or with deny and a reason.".into(),
            ))
        }
    };
    let agent = &found.episode.actor;
    let answered_plan = route::session_of(conn, agent)?.ok_or(Refusal::DialogGone)?;
    deliver(
        conn,
        &found.episode,
        by,
        agent,
        Delivery {
            how: "answer",
            detail: Some(&prompt),
            prompt: Some(prompt.clone()),
            answered_plan: Some(answered_plan),
        },
        now,
    )
}

enum Act {
    Steer(String),
    Nudge,
    Dismiss(String),
}

fn act_on(
    conn: &Connection,
    settings: &Settings,
    found: &OnChain,
    by: &Caller,
    act: Act,
    (active, wall): (i64, i64),
) -> Result<Outcome, Refusal> {
    let episode = &found.episode;
    let target = &found.target;
    let from = by.actor.to_string();
    let (how, detail, prompt) = match &act {
        Act::Steer(text) => (
            "steer",
            Some(text.as_str()),
            Some(text::steer(&episode.id, &from, text)),
        ),
        Act::Nudge => {
            let stall = route::stall(conn, episode, settings, active)?;
            let level = usize::try_from(episode.level).unwrap_or(usize::MAX);
            let to = if target == &episode.actor {
                text::NudgeTo::Agent
            } else {
                text::NudgeTo::Recipient {
                    may_dismiss: found.index > level,
                }
            };
            ("nudge", None, Some(text::nudge(&stall, &from, to)))
        }
        Act::Dismiss(reason) => ("dismiss", Some(reason.as_str()), None),
    };
    deliver(
        conn,
        episode,
        by,
        target,
        Delivery {
            how,
            detail,
            prompt,
            answered_plan: None,
        },
        (active, wall),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
