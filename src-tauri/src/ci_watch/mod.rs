//! `ci.watch`: wake a lead or Mono when CI finishes for a PR head, so no
//! agent sleeps or polls for CI. A watch is a row (`store`); a thread reads
//! each watched PR from GitHub at most once a minute, `judge` decides, and
//! an ended watch's wake goes out as a `ci` row of `mono_outbox`, which the
//! window that owns the recipient's chat delivers as it does a reminder: a
//! new turn when the chat is idle, a steer when its turn takes one.

mod judge;
mod store;

use std::time::Duration;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager;

use crate::alarms::act::{caller, named_actor};
use crate::ask_gate::Actor;
use crate::mono_messages::Notices;
use crate::session_store::SessionStore;

#[cfg(test)]
pub(crate) use judge::parse as parse_snapshot;
pub(crate) use judge::Snapshot;

/// The checks a wake names one by one unless the watch names others: the
/// required checks of the tyler/main ruleset.
const REQUIRED: [&str; 5] = [
    "gate/lint",
    "review/agent",
    "ci",
    "e2e-linux",
    "gate/integrity",
];

/// How often the thread looks for a PR that is due a read.
const TICK: Duration = Duration::from_secs(15);

pub(crate) fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    store::ensure_tables(conn)
}

/// Why a `ci.*` call was refused: a code and a hint, as every app refusal.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    InvalidInput(String),
    UnknownCaller,
    NotifyForbidden,
    UnknownWatch,
    NotYourWatch,
    WatchEnded(String),
    Failed,
}

impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        eprintln!("A ci.watch call failed: {error}");
        Refusal::Failed
    }
}

impl Refusal {
    fn response(&self) -> Value {
        let (code, hint) = match self {
            Refusal::InvalidInput(hint) => ("invalid_input", hint.clone()),
            Refusal::UnknownCaller => (
                "unknown_caller",
                "MonoCode has no saved session for this caller.".into(),
            ),
            Refusal::NotifyForbidden => (
                "notify_forbidden",
                "notify must be you, or an agent you provisioned (your worker, or a lead you started).".into(),
            ),
            Refusal::UnknownWatch => ("unknown_watch", "No watch has this id. Run ci.list.".into()),
            Refusal::NotYourWatch => (
                "not_your_watch",
                "Only the one who registered a watch, or the one it wakes, cancels it.".into(),
            ),
            Refusal::WatchEnded(outcome) => (
                "watch_ended",
                format!("This watch already ended ({outcome})."),
            ),
            Refusal::Failed => (
                "failed",
                "MonoCode could not read or write its watch rows. Try again.".into(),
            ),
        };
        json!({"ok": false, "code": code, "hint": hint, "error": hint, "retryable": false})
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchInput {
    repo: String,
    pr: i64,
    sha: String,
    notify: Option<String>,
    required: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelInput {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

fn input<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, Refusal> {
    serde_json::from_value(input.clone()).map_err(|error| Refusal::InvalidInput(error.to_string()))
}

/// One `ci.*` call from session `caller_session` at wall time `wall`, as
/// the app answers it.
pub(crate) fn call(
    conn: &Connection,
    caller_session: &str,
    action: &str,
    raw: &Value,
    request_id: &str,
    wall: i64,
) -> Value {
    match act(conn, caller_session, action, raw, request_id, wall) {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(refusal) => refusal.response(),
    }
}

fn act(
    conn: &Connection,
    caller_session: &str,
    action: &str,
    raw: &Value,
    request_id: &str,
    wall: i64,
) -> Result<Value, Refusal> {
    let by = caller(conn, caller_session)?.ok_or(Refusal::UnknownCaller)?;
    match action {
        "ci.watch" => watch(conn, &by, input(raw)?, request_id, wall),
        "ci.list" => {
            input::<NoInput>(raw)?;
            Ok(json!({"watches": store::list(conn, &by)?}))
        }
        "ci.cancel" => {
            let CancelInput { id } = input(raw)?;
            match store::cancel(conn, &by, &id, wall)? {
                Ok(()) => Ok(json!({"id": id, "cancelled": true})),
                Err(store::CancelRefused::Unknown) => Err(Refusal::UnknownWatch),
                Err(store::CancelRefused::NotYours) => Err(Refusal::NotYourWatch),
                Err(store::CancelRefused::Ended(outcome)) => Err(Refusal::WatchEnded(outcome)),
            }
        }
        other => Err(Refusal::InvalidInput(format!(
            "Unknown action {other}. Use ci.watch, ci.list or ci.cancel."
        ))),
    }
}

fn watch(
    conn: &Connection,
    by: &Actor,
    input: WatchInput,
    request_id: &str,
    wall: i64,
) -> Result<Value, Refusal> {
    let invalid = |hint: &str| Refusal::InvalidInput(hint.into());
    let (owner, name) = crate::fs::split_github_repo(&input.repo)
        .map_err(|_| invalid("repo must be owner/name, such as tylers2222/Monocode-Unlocked."))?;
    let repo = format!("{owner}/{name}");
    if input.pr <= 0 {
        return Err(invalid("pr must be a positive PR number."));
    }
    let sha = input.sha.trim().to_ascii_lowercase();
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("sha must be the full 40-character head commit."));
    }
    let required = match input.required {
        None => REQUIRED.map(String::from).to_vec(),
        Some(names)
            if names.len() <= 20
                && names
                    .iter()
                    .all(|name| !name.trim().is_empty() && name.len() <= 100) =>
        {
            names
        }
        Some(_) => return Err(invalid("required is a list of up to 20 check names.")),
    };
    let recipient = match input.notify.as_deref() {
        None => by.clone(),
        Some(name) => named_actor(conn, name)?
            .ok_or_else(|| invalid("notify must be a session or Mono id."))?,
    };
    if !crate::alarms::provisions(conn, by, &recipient)? {
        return Err(Refusal::NotifyForbidden);
    }
    let (id, duplicate) = store::register(
        conn,
        &store::New {
            repo: &repo,
            pr: input.pr,
            sha: &sha,
            required: &required,
            recipient: &recipient,
            by,
            request_id,
        },
        wall,
    )?;
    Ok(json!({
        "id": id,
        "repo": repo,
        "pr": input.pr,
        "sha": sha,
        "notify": recipient.to_string(),
        "duplicate": duplicate,
    }))
}

/// One pass: reads each due PR once, judges its watches and returns the
/// chats to name. `clock` gives (active, wall) time; the store lock is let
/// go while `read` runs.
pub(crate) fn poll(
    store: &SessionStore,
    clock: impl Fn(&Connection) -> rusqlite::Result<(i64, i64)>,
    mut read: impl FnMut(&str, i64) -> Result<Snapshot, String>,
) -> Result<Notices, String> {
    let due = {
        let conn = store.lock_conn()?;
        let (_, wall) = clock(&conn).map_err(|error| error.to_string())?;
        store::due(&conn, wall).map_err(|error| error.to_string())?
    };
    let mut notices = Notices::default();
    for (repo, pr) in due {
        let read = read(&repo, pr);
        let conn = store.lock_conn()?;
        let now = clock(&conn).map_err(|error| error.to_string())?;
        notices.merge(
            store::apply(&conn, (&repo, pr), &read, now).map_err(|error| error.to_string())?,
        );
    }
    Ok(notices)
}

/// One GraphQL read of a PR through `gh`, under the app's shared GitHub
/// rate-limit backoff. No logs are fetched.
fn read_pr(repo: &str, pr: i64) -> Result<Snapshot, String> {
    let pr = pr.to_string();
    let json = crate::fs::gh_run(
        &std::env::temp_dir(),
        &[
            "pr",
            "view",
            &pr,
            "--repo",
            repo,
            "--json",
            "state,headRefOid,statusCheckRollup",
        ],
        false,
    )?;
    judge::parse(&json)
}

/// Starts the thread that reads the watched PRs and sends the wakes.
pub(crate) fn init(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        let store = app.state::<SessionStore>();
        let clock = |conn: &Connection| crate::alarms::now(&store.alarms, conn);
        match poll(&store, clock, read_pr) {
            Ok(notices) => notices.emit(&app),
            Err(error) => eprintln!("Could not read the CI watches: {error}"),
        }
    });
}
