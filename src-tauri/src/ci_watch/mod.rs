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
pub(crate) use store::ensure_tables;
#[cfg(test)]
pub(crate) use store::{OPEN_CAP, READS_PER_PASS};

/// The checks a wake names one by one unless the watch names others: the
/// required checks of the tyler/main ruleset.
const REQUIRED: [&str; 5] = [
    "gate/lint",
    "review/agent",
    "ci",
    "e2e-linux",
    "gate/integrity",
];

/// How often the thread looks for a PR that is due a read. A test's app
/// ticks fast, so a test can see the thread's first pass.
const TICK: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(15)
};

/// A `gh` read of a PR that takes longer than this is stopped and fails.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a `ci.*` call was refused: a code and a hint, as every app refusal.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    InvalidInput(String),
    UnknownCaller,
    NotifyForbidden,
    UnknownWatch,
    NotYourWatch,
    WatchEnded(String),
    TooManyWatches,
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
            Refusal::TooManyWatches => (
                "too_many_watches",
                format!(
                    "{} CI watches are open, the most MonoCode keeps. Cancel one (ci.list, ci.cancel) or wait for one to end.",
                    store::OPEN_CAP
                ),
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
    let repo_hint = "repo must be owner/name, such as tylers2222/Monocode-Unlocked.";
    let (owner, name) =
        crate::fs::split_github_repo(&input.repo).map_err(|_| invalid(repo_hint))?;
    let repo = format!("{owner}/{name}");
    let github_name = |byte: u8| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte);
    if !format!("{owner}{name}").bytes().all(github_name) {
        return Err(invalid(repo_hint));
    }
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
    let registered = store::register(
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
    let (id, duplicate) = match registered {
        store::Registered::New(id) => (id, false),
        store::Registered::Before(id) => (id, true),
        store::Registered::Full => return Err(Refusal::TooManyWatches),
    };
    Ok(json!({
        "id": id,
        "repo": repo,
        "pr": input.pr,
        "sha": sha,
        "notify": recipient.to_string(),
        "duplicate": duplicate,
    }))
}

/// One pass: deletes old ended watches, reads each due PR once, judges its
/// watches and returns the chats to name. A PR whose judging fails is
/// logged and waits its next turn; the pass goes on to the next PR.
/// `clock` gives (active, wall) time; the store lock is let go while `read`
/// runs.
pub(crate) fn poll(
    store: &SessionStore,
    clock: impl Fn(&Connection) -> rusqlite::Result<(i64, i64)>,
    mut read: impl FnMut(&str, i64) -> Result<Snapshot, String>,
) -> Result<Notices, String> {
    let due = {
        let conn = store.lock_conn()?;
        let (_, wall) = clock(&conn).map_err(|error| error.to_string())?;
        if let Err(error) = store::prune(&conn, wall) {
            eprintln!("Could not delete the old CI watches: {error}");
        }
        store::due(&conn, wall).map_err(|error| error.to_string())?
    };
    let mut notices = Notices::default();
    for (repo, pr) in due {
        let read = read(&repo, pr);
        let conn = store.lock_conn()?;
        let now = clock(&conn).map_err(|error| error.to_string())?;
        match store::apply(&conn, (&repo, pr), &read, now) {
            Ok(named) => notices.merge(named),
            Err(error) => {
                eprintln!("Could not judge the CI watches of {repo}#{pr}: {error}");
                if let Err(error) = store::mark_read(&conn, (&repo, pr), now.1) {
                    eprintln!("Could not mark {repo}#{pr} read: {error}");
                }
            }
        }
    }
    Ok(notices)
}

/// One GraphQL read of a PR through `gh`, under the app's shared GitHub
/// rate-limit backoff, stopped after `READ_TIMEOUT`. No logs are fetched.
fn read_pr(repo: &str, pr: i64) -> Result<Snapshot, String> {
    let pr = pr.to_string();
    let json = crate::fs::gh_run_within(
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
        READ_TIMEOUT,
    )?;
    judge::parse(&json)
}

/// Starts the thread that reads the watched PRs and sends the wakes. Tests
/// never call GitHub: each of their reads fails.
pub(crate) fn init<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let read: fn(&str, i64) -> Result<Snapshot, String> = if cfg!(test) {
        |_, _| Err("tests read no PR".into())
    } else {
        read_pr
    };
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        let store = app.state::<SessionStore>();
        let clock = |conn: &Connection| crate::alarms::now(&store.alarms, conn);
        match poll(&store, clock, read) {
            Ok(notices) => notices.emit(&app),
            Err(error) => eprintln!("Could not read the CI watches: {error}"),
        }
    });
}
