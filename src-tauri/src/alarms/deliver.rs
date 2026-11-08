//! Design section 7: how an act is logged and reaches the one it acts on.
//! Every act is a row of `alarm_acts` and stops escalation; its message
//! goes through the caller's window to a session, or the outbox to a Mono.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};

use super::act::{caller, named_actor, Forward, Outcome, Refusal};
use super::rights::{standing, target_of};
use super::route::{self, EpisodeRow};
use super::store::Settings;
use crate::ask_gate::Actor;
use crate::mono_messages::{Notices, SessionId};

/// Who makes an act call: the actor, the session it calls from, and the
/// call's request id.
pub(super) struct Caller<'a> {
    pub actor: &'a Actor,
    pub session: &'a str,
    pub request_id: &'a str,
}

/// The outbox key of act `seq`'s message on `episode`. `-` separated: the
/// front end keeps a request id only if it matches `[A-Za-z0-9_-]+`.
pub(crate) fn nudge_key(episode: &str, seq: i64) -> String {
    format!("nudge-{episode}-{seq}")
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
pub(super) fn logged_before(
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
    let key = act.prompt.as_ref().map(|_| nudge_key(&episode.id, seq));
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

/// What an act logs and sends the one it acts on.
pub(super) struct Delivery<'a> {
    pub how: &'static str,
    pub detail: Option<&'a str>,
    pub prompt: Option<String>,
    pub answered_plan: Option<SessionId>,
}

fn session_cwd(conn: &Connection, session: &SessionId) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT cwd FROM sessions WHERE id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )
    .optional()
}

/// Logs the act; its prompt goes to `target`. A session's running turn
/// takes it only through the caller's window (`sessions.send` with
/// whenBusy steer); an idle one starts a turn. A Mono chat takes the
/// outbox row, which steers its running turn.
pub(super) fn deliver(
    conn: &Connection,
    episode: &EpisodeRow,
    by: &Caller,
    target: &Actor,
    delivery: Delivery,
    now: (i64, i64),
) -> Result<Outcome, Refusal> {
    let Delivery {
        how,
        detail,
        prompt,
        answered_plan,
    } = delivery;
    if let (Actor::Session(session), Some(prompt)) = (target, &prompt) {
        let mut input = json!({
            "sessionId": session.as_str(),
            "prompt": prompt,
            "whenBusy": "steer",
        });
        // The window finds a Mono's project from its chat's folder, which
        // is no project, so a Mono with two projects names the target's.
        if matches!(by.actor, Actor::Mono(_)) {
            if let Some(cwd) = session_cwd(conn, session)? {
                input["project"] = json!(cwd);
            }
        }
        return Ok(Outcome::Forward(Forward {
            episode: episode.id.clone(),
            caller: by.actor.clone(),
            target: target.clone(),
            session: by.session.to_string(),
            action: "sessions.send",
            input,
            how,
            detail: detail.unwrap_or_default().to_string(),
            request_id: by.request_id.to_string(),
            answered_plan,
        }));
    }
    let (result, notices) = logged_in_tx(
        conn,
        episode,
        &Logged {
            caller: by.actor,
            target,
            how,
            detail,
            prompt,
            request_id: by.request_id,
        },
        now,
    )?;
    Ok(Outcome::Done {
        result,
        notices,
        answered_plan,
    })
}

/// A window did `plan`'s act: it is logged.
pub(crate) fn forwarded(
    conn: &Connection,
    plan: &Forward,
    now: (i64, i64),
) -> Result<Value, Refusal> {
    let episode = route::episode(conn, &plan.episode)?.ok_or(Refusal::UnknownAlarm)?;
    let logged = logged_in_tx(
        conn,
        &episode,
        &Logged {
            caller: &plan.caller,
            target: &plan.target,
            how: plan.how,
            detail: (!plan.detail.is_empty()).then_some(plan.detail.as_str()),
            prompt: None,
            request_id: &plan.request_id,
        },
        now,
    )?;
    Ok(logged.0)
}

/// A plain `messages.send` or `sessions.send` from `caller_session` to
/// `to` was accepted: it is an act (`message`) on each open episode the
/// caller was told of, where `to` is the one it acts on. A provisioner the
/// alarm has not reached sends routine messages, not replies to it.
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
        let Some(index) = standing(&chain, &caller) else {
            continue;
        };
        let told = i64::try_from(index).is_ok_and(|index| index <= episode.level);
        if !told || target_of(&chain, index) != Some(&to) {
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
