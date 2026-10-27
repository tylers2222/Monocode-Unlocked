//! Authenticated loopback transport. App windows own execution; callers never
//! receive arbitrary Tauri command access or direct database write access.
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WebviewWindow};

use crate::harness::HarnessHost;
use crate::mono_messages::{self, Notices, SessionId};
use crate::session_store::SessionStore;

const APP_TURN_INACTIVE: &str = "MonoCode app access is inactive. Use /operator once in this thread to enable it, then call the CLI during an active agent turn. Retrying this request now will not enable access.";
pub(crate) const UNKNOWN_TOKEN: &str = "This token is no longer valid. It ends when the app restarts, the window closes, its session is deleted or archived, or its orchestration run stops. Do not retry.";
/// The answer to a call the window did not answer in time. The call may still
/// run there; the CLI caller can repeat it with `--request-id`.
pub(crate) const REQUEST_TIMED_OUT: &str =
    "Control request timed out. Retry with the same request ID.";
/// A tool caller (`app-mcp`, omp host tools) cannot pass a request id, so the
/// tool path repeats a timed-out call itself, this many times, with the same
/// id; the app returns the first outcome instead of applying it twice.
pub(crate) const TOOL_CALL_RETRIES: usize = 2;
const OUTCOME_UNKNOWN: &str = "MonoCode did not confirm this call in time. The message may already be delivered. Do not send it again. Call get_monos to check that MonoCode responds, and if you asked for a reply, wait for it.";
/// A read changes nothing, so repeating it is safe (PR I3 review r1 m3).
const READ_OUTCOME_UNKNOWN: &str =
    "MonoCode did not confirm this call in time. Reading changes nothing, so call get_message again.";

/// What a tool caller gets when no attempt of `action` was confirmed (design
/// 3.4 `outcome_unknown`).
pub(crate) fn outcome_unknown(action: &str) -> Value {
    let hint = match action {
        "messages.get" => READ_OUTCOME_UNKNOWN,
        _ => OUTCOME_UNKNOWN,
    };
    json!({"ok": false, "code": "outcome_unknown", "hint": hint, "error": hint})
}

/// A token the app refuses. Both are final for that token, so neither is retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    UnknownToken,
    TurnInactive,
}
impl Refusal {
    fn response(self) -> Value {
        let (code, hint) = match self {
            Refusal::UnknownToken => ("unknown_token", UNKNOWN_TOKEN),
            Refusal::TurnInactive => ("turn_inactive", APP_TURN_INACTIVE),
        };
        json!({"ok": false, "code": code, "hint": hint, "error": hint, "retryable": false})
    }
}

#[derive(Debug, PartialEq)]
enum Failure {
    Refused(Refusal),
    Other(String),
}
impl Failure {
    fn response(self) -> Value {
        match self {
            Failure::Refused(refusal) => refusal.response(),
            Failure::Other(error) => json!({"ok": false, "error": error}),
        }
    }
}
impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Failure::Refused(refusal)
    }
}
impl From<String> for Failure {
    fn from(error: String) -> Self {
        Failure::Other(error)
    }
}
impl From<&str> for Failure {
    fn from(error: &str) -> Self {
        Failure::Other(error.to_string())
    }
}

#[derive(Clone)]
struct Grant {
    window: String,
    session: String,
    cwd: String,
    token: String,
}
struct Pending {
    window: String,
    reply: mpsc::Sender<Value>,
}
/// Who started a turn (ADE-138).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartedBy {
    /// MonoCode sent the turn's prompt.
    MonoCode,
    /// The harness ran again by itself: omp wakes when a background job
    /// ends. The window knows no turn there, so it can steer nothing into
    /// it, and Mono message delivery treats the session as idle.
    Harness,
}
struct ActiveTurn {
    window: String,
    cwd: String,
    app_allowed: bool,
    /// The web app's id for this turn (design 5.2). Mono messages handed to
    /// it carry it, and its end closes their handles.
    turn_id: String,
    started_by: StartedBy,
}
#[derive(Default)]
struct Inner {
    grants: HashMap<String, Grant>,
    app_grants: HashMap<String, Grant>,
    pending: HashMap<String, Pending>,
    workers: HashMap<String, String>,
    scratch: HashMap<String, PathBuf>,
    active: HashMap<String, ActiveTurn>,
    /// The sessions each window has loaded, as the window last reported.
    loaded: HashMap<String, HashSet<String>>,
}
impl Inner {
    // Provider processes can stay alive between turns, so install the token
    // before their first spawn. request_grant still requires an opted-in turn.
    fn prepare_app_grant(&mut self, session: &str, window: &str, cwd: &str) -> bool {
        if self.workers.contains_key(session) || self.grants.contains_key(session) {
            return false;
        }
        let token = self
            .app_grants
            .get(session)
            .map(|grant| grant.token.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                )
            });
        self.app_grants.insert(
            session.to_string(),
            Grant {
                window: window.to_string(),
                session: session.to_string(),
                cwd: cwd.to_string(),
                token,
            },
        );
        true
    }

    fn window_sessions(&self, label: &str) -> Vec<String> {
        let leads: Vec<String> = self
            .grants
            .values()
            .filter(|grant| grant.window == label)
            .map(|grant| grant.session.clone())
            .collect();
        let mut ids = leads.clone();
        ids.extend(
            self.app_grants
                .values()
                .filter(|grant| grant.window == label)
                .map(|grant| grant.session.clone()),
        );
        ids.extend(
            self.workers
                .iter()
                .filter(|(_, lead)| leads.contains(lead))
                .map(|(id, _)| id.clone()),
        );
        ids.extend(
            self.active
                .iter()
                .filter(|(_, turn)| turn.window == label)
                .map(|(id, _)| id.clone()),
        );
        ids.sort();
        ids.dedup();
        ids
    }
    /// A deleted or archived session keeps no token, turn or scratch folder.
    /// Returns the turn it drops, which the caller must end (design 5.3).
    fn forget_session(&mut self, session: &str) -> Option<ActiveTurn> {
        self.grants.remove(session);
        self.app_grants.remove(session);
        self.workers.remove(session);
        self.scratch.remove(session);
        self.active.remove(session)
    }
    /// Design 6 (PR 5 review): the one window that runs a Mono message's
    /// routes and reconcile. A running session belongs to the window of its
    /// turn; otherwise to the first window by label that has it loaded;
    /// otherwise to the caller, which loads it. The answer is the same
    /// whichever window asks, so two windows never both own a session.
    fn route_owner(&self, session: &str, caller: &str) -> String {
        if let Some(turn) = self.active.get(session) {
            return turn.window.clone();
        }
        self.loaded
            .iter()
            .filter(|(_, ids)| ids.contains(session))
            .map(|(label, _)| label)
            .min()
            .map_or_else(|| caller.to_string(), Clone::clone)
    }
    /// Authorizes `turn` of `session`. Returns the turn it replaces when that
    /// is another turn, which no one finished: its window reloaded (design
    /// 5.3), or it was a harness-started turn that a MonoCode turn takes over.
    fn authorize_turn(
        &mut self,
        session: &str,
        mut turn: ActiveTurn,
    ) -> Result<Option<ActiveTurn>, String> {
        // A harness run inside a MonoCode turn is part of that turn.
        if turn.started_by == StartedBy::Harness
            && self
                .active
                .get(session)
                .is_some_and(|running| running.started_by == StartedBy::MonoCode)
        {
            return Ok(None);
        }
        if let Some(lead) = self
            .grants
            .values()
            .find(|grant| paths_overlap(&grant.cwd, &turn.cwd))
        {
            if lead.window != turn.window
                || (lead.session != session && self.workers.get(session) != Some(&lead.session))
            {
                return Err("This checkout is controlled by an orchestrator. Stop that run before starting independent work.".into());
            }
        }
        let eligible = self.prepare_app_grant(session, &turn.window, &turn.cwd);
        turn.app_allowed &= eligible;
        let turn_id = turn.turn_id.clone();
        let replaced = self.active.insert(session.to_string(), turn);
        Ok(replaced.filter(|turn| turn.turn_id != turn_id))
    }
    /// Ends turn `turn_id` of `session` only if it is the turn that runs: a
    /// later turn of the session may have replaced it.
    fn finish_turn(&mut self, session: &str, turn_id: &str) -> Option<ActiveTurn> {
        if self
            .active
            .get(session)
            .is_some_and(|turn| turn.turn_id == turn_id)
        {
            self.active.remove(session)
        } else {
            None
        }
    }
    /// Design 2 (review r3): window `label` reloaded, so the turns it ran are
    /// over, and it has loaded nothing until it reports again (PR 4a review
    /// m11).
    fn boot_window(&mut self, label: &str) -> Vec<(String, ActiveTurn)> {
        self.loaded.remove(label);
        let mut ids: Vec<String> = self
            .active
            .iter()
            .filter(|(_, turn)| turn.window == label)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| self.active.remove(&id).map(|turn| (id, turn)))
            .collect()
    }
    /// Returns the turns it drops.
    fn close_window(&mut self, label: &str) -> Vec<(String, ActiveTurn)> {
        self.loaded.remove(label);
        let ids = self.window_sessions(label);
        self.grants.retain(|id, _| !ids.contains(id));
        self.app_grants.retain(|id, _| !ids.contains(id));
        self.workers.retain(|id, _| !ids.contains(id));
        self.scratch.retain(|id, _| !ids.contains(id));
        let ended = ids
            .iter()
            .filter_map(|id| self.active.remove(id).map(|turn| (id.clone(), turn)))
            .collect();
        self.pending.retain(|_, pending| {
            if pending.window != label {
                return true;
            }
            let _ = pending
                .reply
                .send(json!({"ok":false,"error":"MonoCode window closed"}));
            false
        });
        ended
    }
}
pub struct ControlHost {
    endpoint: String,
    inner: Arc<Mutex<Inner>>,
}
impl ControlHost {
    /// See [`Inner::route_owner`]. Read after the store transaction commits.
    pub(crate) fn route_owner(&self, session: &str, caller: &str) -> String {
        self.inner
            .lock()
            .map(|inner| inner.route_owner(session, caller))
            .unwrap_or_else(|_| caller.to_string())
    }

    /// Sessions with an authorized MonoCode turn, each with that turn's id:
    /// the gate's `running` (design 6, 16.6). A harness-started turn is not
    /// one: the window cannot steer into it (ADE-138).
    pub(crate) fn running_sessions(&self) -> HashMap<String, String> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .active
                    .iter()
                    .filter(|(_, turn)| turn.started_by == StartedBy::MonoCode)
                    .map(|(session, turn)| (session.clone(), turn.turn_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn sessions_running_outside(&self, label: &str) -> HashSet<String> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .active
                    .iter()
                    .filter(|(_, turn)| turn.window != label)
                    .map(|(session, _)| session.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/** Windows paths compare case-insensitively; POSIX paths must retain case. */
fn comparison_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    token: String,
    namespace: String,
    action: String,
    input: Value,
    request_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    id: String,
    namespace: String,
    session_id: String,
    /// The turn that authorized the call, so the web app binds the call to
    /// it (design 2 step 4).
    turn_id: Option<String>,
    request_id: String,
    action: String,
    input: Value,
}

fn request_grant(host: &Inner, namespace: &str, token: &str) -> Result<Grant, Failure> {
    let grant = match namespace {
        "control" => host.grants.values().find(|grant| grant.token == token),
        "app" => host.app_grants.values().find(|grant| grant.token == token),
        _ => return Err("Unknown control namespace".into()),
    }
    .cloned()
    .ok_or(Refusal::UnknownToken)?;
    if namespace == "app" {
        check_app_turn(host, &grant)?;
    }
    Ok(grant)
}

/// Design 2 step 3: an app grant works only while its session runs an
/// opted-in turn in the grant's window, and never for orchestration.
fn check_app_turn(host: &Inner, grant: &Grant) -> Result<(), Failure> {
    if host.grants.contains_key(&grant.session)
        || host.workers.contains_key(&grant.session)
        || !host
            .active
            .get(&grant.session)
            .is_some_and(|turn| turn.window == grant.window && turn.app_allowed)
    {
        return Err(Refusal::TurnInactive.into());
    }
    Ok(())
}

/// The grant an omp host tool call of `session` from `window` runs under.
/// omp's process belongs to the window running its turn, so no token is
/// involved: the window names the session and the turn check does the rest.
fn host_grant(host: &Inner, window: &str, session: &str) -> Result<Grant, Failure> {
    let grant = host
        .app_grants
        .get(session)
        .filter(|grant| grant.window == window)
        .cloned()
        .ok_or(Refusal::TurnInactive)?;
    check_app_turn(host, &grant)?;
    Ok(grant)
}

#[derive(Clone)]
struct Call {
    namespace: String,
    request_id: String,
    action: String,
    input: Value,
}

/// How long the app waits for a window's `control_reply`.
const REPLY_WAIT: Duration = Duration::from_secs(35);

/// Design 2 step 4: hand one authorized call to its grant's window as a
/// control event and wait for that window's `control_reply`.
fn dispatch(
    inner: &Mutex<Inner>,
    grant: impl FnOnce(&Inner) -> Result<Grant, Failure>,
    call: Call,
    emit: &mut impl FnMut(&str, Event) -> bool,
    wait: Duration,
) -> Result<Value, Failure> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel();
    let (grant, turn_id) = {
        let mut host = inner.lock().map_err(|_| "Control service unavailable")?;
        let grant = grant(&host)?;
        if host.pending.len() >= 24 {
            return Err("Too many pending control requests".into());
        }
        host.pending.insert(
            id.clone(),
            Pending {
                window: grant.window.clone(),
                reply: tx,
            },
        );
        // Read under the grant's lock (design 2 step 4), so the web app binds
        // the call to the turn that authorized it.
        let turn_id = host
            .active
            .get(&grant.session)
            .map(|turn| turn.turn_id.clone());
        (grant, turn_id)
    };
    let event = Event {
        id: id.clone(),
        namespace: call.namespace,
        session_id: grant.session,
        turn_id,
        request_id: call.request_id,
        action: call.action,
        input: call.input,
    };
    let result = if emit(&grant.window, event) {
        rx.recv_timeout(wait).map_err(|_| REQUEST_TIMED_OUT.into())
    } else {
        Err("MonoCode executor is unavailable".into())
    };
    if let Ok(mut host) = inner.lock() {
        host.pending.remove(&id);
    }
    result
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let endpoint = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .to_string();
    let inner = Arc::new(Mutex::new(Inner::default()));
    app.manage(ControlHost {
        endpoint,
        inner: inner.clone(),
    });
    let app = app.clone();
    std::thread::spawn(move || {
        // Limit concurrent readers, including unauthenticated sockets.
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(32);
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..8 {
            let rx = rx.clone();
            let app = app.clone();
            let inner = inner.clone();
            std::thread::spawn(move || loop {
                let stream = match rx.lock() {
                    Ok(rx) => rx.recv(),
                    Err(_) => return,
                };
                let Ok(stream) = stream else { return };
                serve(stream, &app, &inner);
            });
        }
        for stream in listener.incoming().flatten() {
            let _ = tx.try_send(stream);
        }
    });
    Ok(())
}

fn serve<R: Runtime>(mut stream: TcpStream, app: &AppHandle<R>, inner: &Arc<Mutex<Inner>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let result = (|| -> Result<Value, Failure> {
        let mut raw = String::new();
        BufReader::new(&mut stream)
            .take(262_145)
            .read_line(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > 262_144 {
            return Err("Request exceeds 256 KiB".into());
        }
        let request: Request = serde_json::from_str(&raw).map_err(|_| "Invalid control request")?;
        if !request.input.is_object()
            || request.request_id.is_empty()
            || request.request_id.len() > 128
        {
            return Err("Invalid input or request ID".into());
        }
        dispatch(
            inner,
            |host| request_grant(host, &request.namespace, &request.token),
            Call {
                namespace: request.namespace.clone(),
                request_id: request.request_id,
                action: request.action,
                input: request.input,
            },
            &mut |window, event| {
                app.emit_to(window, "monocode-control-request", event)
                    .is_ok()
            },
            REPLY_WAIT,
        )
    })();
    let response = result.unwrap_or_else(Failure::response);
    let _ = writeln!(stream, "{response}");
}

#[tauri::command]
pub fn control_enable(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    session_id: String,
    cwd: String,
) -> Result<String, String> {
    let cwd = std::fs::canonicalize(crate::fs::expand_home(&cwd)).map_err(|e| e.to_string())?;
    if !cwd.is_dir() {
        return Err("Choose a project folder first".into());
    }
    let cwd = comparison_path(&cwd);
    let mut inner = host
        .inner
        .lock()
        .map_err(|_| "Control service unavailable")?;
    if let Some((id, _)) = inner
        .active
        .iter()
        .find(|(id, turn)| *id != &session_id && paths_overlap(&turn.cwd, &cwd))
    {
        return Err(format!(
            "Another session ({id}) is running in this checkout. Stop it before enabling orchestration."
        ));
    }
    if inner.grants.values().any(|g| {
        paths_overlap(&g.cwd, &cwd) && (g.session != session_id || g.window != window.label())
    }) {
        return Err("This checkout already has an orchestrator in another session".into());
    }
    // A lead may return to ordinary chat without respawning its provider.
    // Keep its app token installed but unusable until a later opted-in turn.
    inner.prepare_app_grant(&session_id, window.label(), &cwd);
    inner.grants.insert(
        session_id.clone(),
        Grant {
            window: window.label().into(),
            session: session_id.clone(),
            cwd,
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        },
    );
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(executable.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn control_disable(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    session_id: String,
) -> Result<(), String> {
    let mut inner = host
        .inner
        .lock()
        .map_err(|_| "Control service unavailable")?;
    if inner
        .grants
        .get(&session_id)
        .is_some_and(|g| g.window == window.label())
    {
        inner.grants.remove(&session_id);
        inner.workers.retain(|_, parent| parent != &session_id);
        let workers = inner.workers.clone();
        inner.scratch.retain(|id, _| workers.contains_key(id));
    }
    Ok(())
}

#[tauri::command]
pub fn control_attach_worker(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    lead_id: String,
    session_id: String,
) -> Result<String, String> {
    let mut inner = host
        .inner
        .lock()
        .map_err(|_| "Control service unavailable")?;
    if inner
        .grants
        .get(&lead_id)
        .is_none_or(|grant| grant.window != window.label())
    {
        return Err("Lead connection is inactive".into());
    }
    let scratch = match inner.scratch.get(&session_id) {
        Some(path) if path.is_dir() => path.clone(),
        _ => create_worker_scratch()?,
    };
    inner.workers.insert(session_id.clone(), lead_id);
    inner.app_grants.remove(&session_id);
    inner.scratch.insert(session_id, scratch.clone());
    Ok(scratch.to_string_lossy().into_owned())
}

fn create_worker_scratch() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("monocode-worker-{}", uuid::Uuid::new_v4()));
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&path).map_err(|e| e.to_string())?;
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}

fn configure_worker_scratch(cmd: &mut Command, path: &Path) {
    // Native temp-file helpers and shell mktemp use the same private scope
    // that is named in the worker's assignment prompt.
    cmd.env("TMPDIR", path).env("TMP", path).env("TEMP", path);
}

// Async: a replaced turn runs a store transaction, never on the main thread.
#[tauri::command(async)]
pub fn control_authorize_turn<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    session_id: String,
    cwd: String,
    app_access: bool,
    turn_id: String,
) -> Result<(), String> {
    authorize_window_turn(
        &app,
        &window,
        &session_id,
        &cwd,
        app_access,
        &turn_id,
        StartedBy::MonoCode,
    )
}

/// ADE-138: the harness ran again by itself after a MonoCode turn of the
/// session. The window asks with that turn's `cwd` and `app_access`, so the
/// run gets the same access under the same checks, and
/// `control_turn_finished` ends it.
#[tauri::command(async)]
pub fn control_authorize_harness_turn<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    session_id: String,
    cwd: String,
    app_access: bool,
    turn_id: String,
) -> Result<(), String> {
    authorize_window_turn(
        &app,
        &window,
        &session_id,
        &cwd,
        app_access,
        &turn_id,
        StartedBy::Harness,
    )
}

fn authorize_window_turn<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    session: &str,
    cwd: &str,
    app_access: bool,
    turn_id: &str,
    started_by: StartedBy,
) -> Result<(), String> {
    let turn_id = mono_messages::parse_turn_id(turn_id)?;
    let workdir = crate::fs::expand_home(cwd);
    let cwd = std::fs::canonicalize(&workdir)
        .map_err(|e| format!("Working directory {}: {e}", workdir.display()))?;
    let turn = ActiveTurn {
        window: window.label().into(),
        cwd: comparison_path(&cwd),
        app_allowed: app_access,
        turn_id: turn_id.into(),
        started_by,
    };
    authorize_turn(
        &app.state::<ControlHost>().inner,
        &app.state::<SessionStore>(),
        session,
        turn,
    )?
    .emit(app);
    Ok(())
}

fn authorize_turn(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    session: &str,
    turn: ActiveTurn,
) -> Result<Notices, String> {
    let replaced = inner
        .lock()
        .map_err(|_| "Control service unavailable")?
        .authorize_turn(session, turn)?;
    Ok(end_turns(
        store,
        replaced.map(|turn| (session.to_string(), turn)),
    ))
}

/// A window reports the sessions it has loaded, for [`Inner::route_owner`].
#[tauri::command]
pub fn control_loaded_sessions(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    sessions: Vec<String>,
) {
    if let Ok(mut inner) = host.inner.lock() {
        inner
            .loaded
            .insert(window.label().to_string(), sessions.into_iter().collect());
    }
}

/// The registry's `finally` for turn `turn_id` of the session.
#[tauri::command(async)]
pub fn control_turn_finished<R: Runtime>(
    app: AppHandle<R>,
    host: State<'_, ControlHost>,
    session_id: String,
    turn_id: String,
) {
    finish_turn(
        &host.inner,
        &app.state::<SessionStore>(),
        &session_id,
        &turn_id,
    )
    .emit(&app);
}

fn finish_turn(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    session: &str,
    turn_id: &str,
) -> Notices {
    let ended = inner
        .lock()
        .ok()
        .and_then(|mut inner| inner.finish_turn(session, turn_id));
    end_turns(store, ended.map(|turn| (session.to_string(), turn)))
}

/// Design 5.3 (review r2): the Stop and kill paths. The process is gone, so
/// its turn is over whatever its id; the window may not know that id after a
/// reload.
#[tauri::command(async)]
pub fn control_kill_session(app: AppHandle, host: State<'_, ControlHost>, session_id: String) {
    kill_session(
        &host.inner,
        &app.state::<SessionStore>(),
        &session_id,
        &app.state::<HarnessHost>(),
    )
    .emit(&app);
}

fn kill_session(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    session: &str,
    harness: &HarnessHost,
) -> Notices {
    let ended = inner
        .lock()
        .ok()
        .and_then(|mut inner| inner.active.remove(session));
    // A session's `S#n` children (antigravity) carry its token too.
    harness.kill_session_tree(session);
    end_turns(store, ended.map(|turn| (session.to_string(), turn)))
}

/// Design 2 (review r4): a window that boots runs no turn yet, so every turn
/// that `active` gives it is left from before a reload. Awaited before the
/// window loads any session.
#[tauri::command(async)]
pub fn control_window_booted(app: AppHandle, window: WebviewWindow, host: State<'_, ControlHost>) {
    boot_window(
        &host.inner,
        &app.state::<SessionStore>(),
        window.label(),
        &app.state::<HarnessHost>(),
    )
    .emit(&app);
}

fn boot_window(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    label: &str,
    harness: &HarnessHost,
) -> Notices {
    let ended = match inner.lock() {
        Ok(mut inner) => inner.boot_window(label),
        Err(_) => Vec::new(),
    };
    for (session, _) in &ended {
        harness.kill_session_tree(session);
    }
    let mut notices = end_turns(store, ended);
    notices.merge(release_window_slots(store, label));
    notices
}

/// Whether this window runs the routes and reconcile of the session (design
/// 6, PR 4a review m19).
#[tauri::command]
pub fn control_owns_session(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    session_id: String,
) -> bool {
    host.route_owner(&session_id, window.label()) == window.label()
}

/// Design 5.3: the turn-end rules for turns that `active` no longer holds.
/// The control lock is never held here (5.3 lock order).
fn end_turns(
    store: &SessionStore,
    turns: impl IntoIterator<Item = (String, ActiveTurn)>,
) -> Notices {
    let mut notices = Notices::default();
    for (session, turn) in turns {
        let Some(session) = SessionId::parse(&session) else {
            continue;
        };
        let ended = store.lock_conn().and_then(|conn| {
            mono_messages::turn_ended(&conn, &session, &turn.turn_id, mono_messages::now_ms())
                .map_err(|error| error.to_string())
        });
        match ended {
            Ok(ended) => notices.merge(ended),
            Err(error) => eprintln!(
                "Turn {} of {} ended, but its Mono messages kept their state: {error}",
                turn.turn_id,
                session.as_str()
            ),
        }
    }
    notices
}

/// Design 2 (review r5): the slots window `label` set that no provider took.
fn release_window_slots(store: &SessionStore, label: &str) -> Notices {
    let released = store.lock_conn().and_then(|conn| {
        mono_messages::release_window_slots(&conn, label, mono_messages::now_ms())
            .map_err(|error| error.to_string())
    });
    released.unwrap_or_else(|error| {
        eprintln!("Window {label} closed, but its Mono message slots stay: {error}");
        Notices::default()
    })
}

/// Design 7: session `session` is reset, deleted or archived. Its token and
/// turn go first, before the store transaction opens (5.3 lock order).
/// `remove` then deletes or archives it and runs the clear in that one
/// transaction, so a failure leaves the session live and receiving (PR 6
/// review r1 M1). Emits the notices of both steps once, after the commit or
/// the failure.
pub(crate) fn end_session(
    app: &AppHandle,
    session: &str,
    remove: impl FnOnce(&Connection) -> rusqlite::Result<Notices>,
) -> Result<(), String> {
    let (notices, removed) = end_session_with(
        &app.state::<ControlHost>().inner,
        &app.state::<SessionStore>(),
        &app.state::<HarnessHost>(),
        session,
        remove,
    );
    notices.emit(app);
    removed
}

fn end_session_with(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    harness: &HarnessHost,
    session: &str,
    remove: impl FnOnce(&Connection) -> rusqlite::Result<Notices>,
) -> (Notices, Result<(), String>) {
    let mut notices = forget_session(inner, store, harness, session);
    let removed = store
        .lock_conn()
        .and_then(|conn| remove(&conn).map_err(|error| error.to_string()));
    match removed {
        Ok(cleared) => {
            notices.merge(cleared);
            (notices, Ok(()))
        }
        Err(error) => (notices, Err(error)),
    }
}

/// The turn the session ran ends with it (PR 5 review r2 m3): its questions
/// become unanswered and the texts its provider never took are freed, as
/// [`kill_session`] does.
fn forget_session(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    harness: &crate::harness::HarnessHost,
    session: &str,
) -> Notices {
    let ended = inner
        .lock()
        .ok()
        .and_then(|mut inner| inner.forget_session(session));
    // A live provider keeps the old token in its environment, and a later turn
    // of an unarchived session would mint a token that process never receives.
    harness.kill_session_tree(session);
    end_turns(store, ended.map(|turn| (session.to_string(), turn)))
}

pub fn window_closed(app: &AppHandle, label: &str) {
    close_window(
        &app.state::<ControlHost>().inner,
        &app.state::<SessionStore>(),
        label,
        &app.state::<HarnessHost>(),
    )
    .emit(app);
}

/// Design 2 (review r5): kills the window's processes, drops its turns and
/// grants, then ends those turns and frees the slots it set.
fn close_window(
    inner: &Mutex<Inner>,
    store: &SessionStore,
    label: &str,
    harness: &HarnessHost,
) -> Notices {
    let ids = {
        let Ok(inner) = inner.lock() else {
            return Notices::default();
        };
        inner.window_sessions(label)
    };
    // A session's `S#n` children (antigravity) carry its token too.
    for id in &ids {
        harness.kill_session_tree(id);
    }
    let ended = match inner.lock() {
        Ok(mut inner) => inner.close_window(label),
        Err(_) => Vec::new(),
    };
    let mut notices = end_turns(store, ended);
    notices.merge(release_window_slots(store, label));
    notices
}

pub fn configure_child<R: Runtime>(app: &AppHandle<R>, session_id: &str, cmd: &mut Command) {
    cmd.env_remove("MONOCODE_CONTROL_ENDPOINT")
        .env_remove("MONOCODE_CONTROL_TOKEN")
        .env_remove("MONOCODE_APP_ENDPOINT")
        .env_remove("MONOCODE_APP_TOKEN");
    let Some(host) = app.try_state::<ControlHost>() else {
        return;
    };
    if let Ok(inner) = host.inner.lock() {
        configure_session_child(&inner, &host.endpoint, session_id, cmd);
    };
}

/// A child keyed `S#n` (antigravity) is session S's process and gets S's app
/// token. Orchestration access (control token, worker scratch) stays with the
/// exact key: the orchestration stop paths kill by exact key, so an `S#n`
/// child they miss must not hold a lead's control token. A lead also holds an
/// app grant (`control_enable`), so its `S#n` gets that token; it is inert
/// while the session leads and works again only after `control_disable`.
fn configure_session_child(inner: &Inner, endpoint: &str, child_key: &str, cmd: &mut Command) {
    if let Some(grant) = inner.grants.get(child_key) {
        cmd.env("MONOCODE_CONTROL_ENDPOINT", endpoint)
            .env("MONOCODE_CONTROL_TOKEN", &grant.token);
    }
    if let Some(grant) = inner
        .app_grants
        .get(crate::harness::child_session(child_key))
    {
        cmd.env("MONOCODE_APP_ENDPOINT", endpoint)
            .env("MONOCODE_APP_TOKEN", &grant.token);
    }
    if let Some(scratch) = inner.scratch.get(child_key) {
        configure_worker_scratch(cmd, scratch);
    }
}

#[tauri::command]
pub fn app_cli_path() -> Result<String, String> {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

/// The Mono messaging tool schemas, for omp's `set_host_tools` (design 4).
#[tauri::command]
pub fn app_tool_schemas() -> Value {
    crate::control_cli::app_tools()
}

/// Design 4, omp: one Mono messaging host tool call of `session_id`, run as
/// an `app-mcp` call would run, under the same turn check and control event.
#[tauri::command(async)]
pub fn control_host_call(
    window: WebviewWindow,
    host: State<'_, ControlHost>,
    session_id: String,
    action: String,
    input: Value,
    request_id: String,
) -> Value {
    host_call(
        &host.inner,
        window.label(),
        &session_id,
        action,
        input,
        request_id,
        |label, event| {
            window
                .emit_to(label, "monocode-control-request", event)
                .is_ok()
        },
        REPLY_WAIT,
    )
    .unwrap_or_else(Failure::response)
}

#[expect(clippy::too_many_arguments, reason = "baseline 2026-10")]
fn host_call(
    inner: &Mutex<Inner>,
    window: &str,
    session: &str,
    action: String,
    input: Value,
    request_id: String,
    mut emit: impl FnMut(&str, Event) -> bool,
    wait: Duration,
) -> Result<Value, Failure> {
    let tools = crate::control_cli::app_tools();
    let is_tool = tools
        .as_array()
        .into_iter()
        .flatten()
        .any(|tool| tool["action"] == action.as_str());
    if !is_tool {
        return Err(format!("{action} is not a Mono messaging tool").into());
    }
    // Design 3.5: the request id must pass `RequestId::parse`.
    let valid_id = (1..=128).contains(&request_id.len())
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !input.is_object() || !valid_id {
        return Err("Invalid input or request ID".into());
    }
    let call = Call {
        namespace: "app".into(),
        request_id,
        action,
        input,
    };
    for _ in 0..=TOOL_CALL_RETRIES {
        match dispatch(
            inner,
            |host| host_grant(host, window, session),
            call.clone(),
            &mut emit,
            wait,
        ) {
            Err(Failure::Other(error)) if error == REQUEST_TIMED_OUT => continue,
            outcome => return outcome,
        }
    }
    Ok(outcome_unknown(&call.action))
}

#[tauri::command]
pub fn control_reply<R: Runtime>(
    window: WebviewWindow<R>,
    host: State<'_, ControlHost>,
    id: String,
    response: Value,
) -> Result<(), String> {
    let mut inner = host
        .inner
        .lock()
        .map_err(|_| "Control service unavailable")?;
    if inner
        .pending
        .get(&id)
        .is_some_and(|p| p.window == window.label())
    {
        if let Some(pending) = inner.pending.remove(&id) {
            let _ = pending.reply.send(response);
        }
    }
    Ok(())
}

#[tauri::command]
pub fn control_save(
    store: State<'_, crate::session_store::SessionStore>,
    lead_id: String,
    state: String,
) -> Result<(), String> {
    if state.len() > 8_000_000 {
        return Err("Orchestration history is too large".into());
    }
    let run: Value = serde_json::from_str(&state).map_err(|_| "Invalid run state")?;
    let conn = store.lock_conn()?;
    crate::session_store::save_orchestration(&conn, &lead_id, &run).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn control_load(
    store: State<'_, crate::session_store::SessionStore>,
    lead_id: String,
) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    store
        .lock_conn()?
        .query_row(
            "SELECT state FROM orchestration_runs WHERE lead_id=?1",
            [lead_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
}

fn resolve_scope(root: &Path, value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return Err("Write scopes must be project-relative paths without '..'".into());
    }
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let mut existing = root.join(path);
    let mut missing = Vec::new();
    while !existing.exists() {
        missing.push(existing.file_name().ok_or("Invalid scope")?.to_os_string());
        if !existing.pop() {
            return Err("Invalid scope".into());
        }
    }
    existing = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
    if !existing.starts_with(&root) {
        return Err("Write scope points outside the project".into());
    }
    for part in missing.into_iter().rev() {
        existing.push(part);
    }
    Ok(comparison_path(&existing))
}

/// Resolve reported writes as well as scopes: aliases and symlinks must not
/// turn a private scratch directory into an exemption for another worker's files.
#[tauri::command]
pub fn control_write_path(path: String) -> Result<String, String> {
    let path = Path::new(&path);
    if !path.is_absolute() {
        return Err("Reported write paths must be absolute".into());
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        // A dangling symlink cannot be treated as a new ordinary file.
        if std::fs::symlink_metadata(existing).is_ok() {
            return Err("Reported write path contains a dangling symlink".into());
        }
        missing.push(
            existing
                .file_name()
                .ok_or("Invalid write path")?
                .to_os_string(),
        );
        existing = existing.parent().ok_or("Invalid write path")?;
    }
    let mut resolved = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
    for part in missing.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved.to_string_lossy().replace('\\', "/"))
}

#[tauri::command]
pub fn control_scopes(cwd: String, files: Vec<String>) -> Result<Vec<String>, String> {
    if files.len() > 64 {
        return Err("At most 64 write scopes per task".into());
    }
    files
        .iter()
        .map(|file| {
            resolve_scope(&crate::fs::expand_home(&cwd), file)
                .map_err(|error| format!("Invalid write scope \"{file}\": {error}"))
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mono_messages::tests::{
        hand_off, handed_off, messaging_store, sent, steer_through_start_row, stored_row,
    };
    use crate::mono_messages::{get, MessageId, Status};

    /// A MonoCode turn with no app access.
    fn monocode_turn(window: &str, cwd: &str, turn_id: &str) -> ActiveTurn {
        ActiveTurn {
            window: window.into(),
            cwd: cwd.into(),
            app_allowed: false,
            turn_id: turn_id.into(),
            started_by: StartedBy::MonoCode,
        }
    }

    fn run(inner: &Mutex<Inner>, session: &str, window: &str, turn_id: &str) {
        inner.lock().unwrap().active.insert(
            session.into(),
            monocode_turn(window, &format!("/{session}"), turn_id),
        );
    }

    pub(crate) fn host() -> ControlHost {
        ControlHost {
            endpoint: String::new(),
            inner: Arc::default(),
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn start_turn_in_canonical_cwd(
        host: &ControlHost,
        store: &SessionStore,
        (session, cwd): &(String, String),
        window: &str,
    ) {
        let turn_id = format!("turn-{session}");
        authorize_turn(
            &host.inner,
            store,
            session,
            monocode_turn(window, cwd, &turn_id),
        )
        .unwrap();
    }

    #[cfg(not(windows))]
    pub(crate) fn boot(host: &ControlHost, store: &SessionStore, window: &str) {
        boot_window(&host.inner, store, window, &HarnessHost::new());
    }

    /// `control_turn_finished` for the turn `start_turn_in_canonical_cwd` began.
    #[cfg(not(windows))]
    pub(crate) fn finish_started_turn(host: &ControlHost, store: &SessionStore, session: &str) {
        finish_turn(&host.inner, store, session, &format!("turn-{session}"));
    }

    // Review r1 m1 (control level). Stop takes the lead's turn-4 out of
    // `active` before its end reaches the store; the next queued turn-5 is
    // authorized in between, and a steer through the start row is accepted
    // for it. Defect: the kill's turn end drops that steer, and the Mono
    // gets a false `dropped` notice.
    #[test]
    fn a_kill_ends_its_turn_after_the_next_turn_takes_a_pinned_steer() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "lead", "main", "turn-4");
        let harness = HarnessHost::new();
        let steer = std::thread::scope(|scope| {
            let held = store.lock_conn().unwrap();
            let killing = scope.spawn(|| kill_session(&inner, &store, "lead", &harness));
            while inner.lock().unwrap().active.contains_key("lead") {
                assert!(!killing.is_finished(), "the kill left turn-4 running");
                std::thread::yield_now();
            }
            authorize_turn(
                &inner,
                &store,
                "lead",
                monocode_turn("main", "/p", "turn-5"),
            )
            .unwrap();
            let (_, steer) = steer_through_start_row(&held, "turn-5");
            drop(held);
            killing.join().unwrap();
            steer
        });
        assert_eq!(inner.lock().unwrap().active["lead"].turn_id, "turn-5");
        assert_eq!(
            stored_row(&store.lock_conn().unwrap(), &steer).0,
            "delivered"
        );
    }

    fn status(store: &SessionStore, id: &MessageId) -> Status {
        get(&store.lock_conn().unwrap(), id)
            .unwrap()
            .unwrap()
            .status
    }

    /// The payload of the `mono-message-notices` event.
    fn payload(notices: &Notices) -> Value {
        serde_json::to_value(notices).unwrap()
    }

    // Design 5.3 review r1. Defect: the `finally` of a turn that already
    // ended runs after the session's next turn started and ends that turn:
    // its token stops working and its questions become unanswered while its
    // agent still works on them.
    #[test]
    fn finishing_an_old_turn_leaves_the_turn_that_replaced_it() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "main", "turn-2");
        let ask = handed_off(
            &store.lock_conn().unwrap(),
            "ask",
            true,
            "turn-2",
            "main",
            true,
        );
        assert_eq!(
            finish_turn(&inner, &store, "a-chat", "turn-1"),
            Notices::default()
        );
        assert_eq!(inner.lock().unwrap().active["a-chat"].turn_id, "turn-2");
        assert!(matches!(status(&store, &ask), Status::Open { .. }));
        let notices = finish_turn(&inner, &store, "a-chat", "turn-2");
        assert!(inner.lock().unwrap().active.is_empty());
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // Design 5.3. Defect: a turn that a reloaded window never finished stays
    // in `active`; when the next turn of its session replaces it, its
    // questions stay open, so their senders wait for good.
    #[test]
    fn authorizing_a_new_turn_ends_the_turn_it_replaces() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        let ask = handed_off(
            &store.lock_conn().unwrap(),
            "ask",
            true,
            "turn-1",
            "main",
            true,
        );
        let authorize = |turn_id| {
            authorize_turn(
                &inner,
                &store,
                "a-chat",
                monocode_turn("main", "/a", turn_id),
            )
            .unwrap()
        };
        assert_eq!(authorize("turn-1"), Notices::default());
        assert_eq!(authorize("turn-1"), Notices::default());
        assert!(matches!(status(&store, &ask), Status::Open { .. }));
        let notices = authorize("turn-2");
        assert_eq!(inner.lock().unwrap().active["a-chat"].turn_id, "turn-2");
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // Design 5.3 review r2. Defect: Stop after a reload cannot name the turn
    // it stops, so the killed turn's questions stay open and the hand-offs
    // it held stay held.
    #[test]
    fn killing_a_session_ends_its_turn_without_naming_it() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "main", "turn-1");
        let (ask, opener) = {
            let conn = store.lock_conn().unwrap();
            (
                handed_off(&conn, "ask", true, "turn-1", "main", true),
                handed_off(&conn, "opener", false, "turn-1", "main", false),
            )
        };
        let notices = kill_session(&inner, &store, "a-chat", &HarnessHost::new());
        assert!(inner.lock().unwrap().active.is_empty());
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert!(matches!(
            status(&store, &opener),
            Status::Delivered { slot: None, .. }
        ));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // PR 5 review r2 m3. Defect: deleting or archiving a session while it
    // runs drops its turn without the turn-end rules, so its questions stay
    // open and the text its provider never took keeps its slot until the app
    // restarts.
    #[test]
    fn forgetting_a_running_session_ends_its_turn() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "main", "turn-1");
        let (ask, opener) = {
            let conn = store.lock_conn().unwrap();
            (
                handed_off(&conn, "ask", true, "turn-1", "main", true),
                handed_off(&conn, "opener", false, "turn-1", "main", false),
            )
        };
        let notices = forget_session(&inner, &store, &HarnessHost::new(), "a-chat");
        finish_turn(&inner, &store, "a-chat", "turn-1");
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert!(matches!(
            status(&store, &opener),
            Status::Delivered { slot: None, .. }
        ));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // Design 7 (review r3 order), PR 6 review r1 m2. Defect: a reset, delete
    // or archive only revokes the token, so a message still queued for the
    // chat stays open with no chat to take it and its sender is never told;
    // the clear runs before the turn ends, so a question the agent saw is
    // dropped instead of closed as unanswered by the turn end; or the clear's
    // notices are lost. Only the clear names `b-chat`.
    #[test]
    fn clearing_a_running_session_ends_its_turn_then_drops_what_it_had_to_take() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "main", "turn-1");
        let (ask, queued) = {
            let conn = store.lock_conn().unwrap();
            conn.execute_batch(
                r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
                   VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat","messageAllowlist":["mono-a"]}', 2, 1, 1);
                   INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
                   VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1);"#,
            )
            .unwrap();
            (
                handed_off(&conn, "ask", true, "turn-1", "main", true),
                sent(&conn, ("mono-b", "b-chat"), "mono-a", "queued", true),
            )
        };
        let (notices, cleared) =
            end_session_with(&inner, &store, &HarnessHost::new(), "a-chat", |conn| {
                crate::session_store::set_archived(conn, "a-chat", true)
            });
        assert_eq!(cleared, Ok(()));
        assert!(inner.lock().unwrap().active.is_empty());
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert!(matches!(status(&store, &queued), Status::Dropped { .. }));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat", "b-chat"]})
        );
    }

    // PR 6 review r2 m2, decision 408. Defect: a failed delete or archive
    // reports success, so the window drops a chat that is still in
    // `sessions`, still receives and still holds its messages; or the failed
    // step's clear notices go out for rows it rolled back.
    #[test]
    fn a_failed_remove_fails_the_end_and_leaves_the_session_receiving() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "main", "turn-1");
        let (ask, queued) = {
            let conn = store.lock_conn().unwrap();
            conn.execute_batch(
                r#"INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
                   VALUES ('mono-b', 'B', 'mono', '{"sessionId":"b-chat","messageAllowlist":["mono-a"]}', 2, 1, 1);
                   INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
                   VALUES ('b-chat', '/', 'codex', 'm', 'default', 'B', 1, 1);
                   CREATE TEMP TRIGGER no_archive BEFORE UPDATE OF archived ON sessions
                     BEGIN SELECT RAISE(ABORT, 'disk full'); END;"#,
            )
            .unwrap();
            (
                handed_off(&conn, "ask", true, "turn-1", "main", true),
                sent(&conn, ("mono-b", "b-chat"), "mono-a", "queued", true),
            )
        };
        let (notices, removed) =
            end_session_with(&inner, &store, &HarnessHost::new(), "a-chat", |conn| {
                crate::session_store::set_archived(conn, "a-chat", true)
            });
        assert!(removed.unwrap_err().contains("disk full"));
        assert!(inner.lock().unwrap().active.is_empty());
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        assert!(matches!(status(&store, &queued), Status::Open { .. }));
        // Only the forget step's notices: none for `b-chat`, whose row stays.
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
        let conn = store.lock_conn().unwrap();
        sent(&conn, ("giga", "giga-chat"), "mono-a", "after", false);
    }

    // Design 2 review r3-r5, PR 4a review m11. Defect: a reloaded window
    // leaves its old turns in `active` with their questions open, keeps the
    // slots of openers it never sent, or is still named the owner of a
    // session it has not loaded again.
    #[test]
    fn a_booting_window_ends_its_old_turns_and_frees_the_slots_it_set() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "reloaded", "turn-a");
        run(&inner, "giga-chat", "reloaded", "turn-g");
        run(&inner, "project", "other", "turn-p");
        inner
            .lock()
            .unwrap()
            .loaded
            .insert("reloaded".into(), ["was-loaded".to_string()].into());
        let (to_a, to_giga, waiting, other) = {
            let conn = store.lock_conn().unwrap();
            let to_giga = sent(&conn, ("mono-a", "a-chat"), "giga", "to-giga", true);
            hand_off(&conn, &to_giga, "turn-g", "reloaded", true);
            (
                handed_off(&conn, "to-a", true, "turn-a", "reloaded", true),
                to_giga,
                handed_off(&conn, "waiting", false, "turn-x", "reloaded", false),
                handed_off(&conn, "other", false, "turn-y", "other", false),
            )
        };
        let notices = boot_window(&inner, &store, "reloaded", &HarnessHost::new());
        let inner = inner.into_inner().unwrap();
        assert_eq!(inner.active.keys().collect::<Vec<_>>(), ["project"]);
        assert_eq!(inner.route_owner("was-loaded", "caller"), "caller");
        for id in [&to_a, &to_giga] {
            assert!(matches!(status(&store, id), Status::Unanswered { .. }));
        }
        assert!(matches!(
            status(&store, &waiting),
            Status::Delivered { slot: None, .. }
        ));
        assert!(matches!(
            status(&store, &other),
            Status::Delivered { slot: Some(_), .. }
        ));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // Design 2 review r5, 5.3. Defect: a window that closes during a hand-off
    // strands the row: its slot outlives the window, so no reconcile ever
    // queues the row again; or the turn it drops keeps its questions open.
    #[test]
    fn closing_a_window_ends_its_turns_frees_its_slots_and_names_each_session_once() {
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        run(&inner, "a-chat", "closing", "turn-1");
        let (ask, opener, waiting, other) = {
            let conn = store.lock_conn().unwrap();
            (
                handed_off(&conn, "ask", true, "turn-1", "closing", true),
                handed_off(&conn, "opener", false, "turn-1", "closing", false),
                handed_off(&conn, "waiting", false, "turn-x", "closing", false),
                handed_off(&conn, "other", false, "turn-y", "open", false),
            )
        };
        let notices = close_window(&inner, &store, "closing", &HarnessHost::new());
        assert!(matches!(status(&store, &ask), Status::Unanswered { .. }));
        for id in [&opener, &waiting] {
            assert!(matches!(
                status(&store, id),
                Status::Delivered { slot: None, .. }
            ));
        }
        assert!(matches!(
            status(&store, &other),
            Status::Delivered { slot: Some(_), .. }
        ));
        assert_eq!(
            payload(&notices),
            json!({"sessions": ["a-chat", "giga-chat"]})
        );
    }

    // PR 4a review m7 (CT1), PR I2. Defect: the gate reads no running turn,
    // so every steer is refused target_gone, or a steer joins a later turn of
    // the session than the one that runs its target.
    #[test]
    fn running_sessions_are_the_sessions_with_an_active_turn_and_its_id() {
        let host = host();
        run(&host.inner, "a-chat", "main", "turn-1");
        assert_eq!(
            host.running_sessions(),
            HashMap::from([("a-chat".to_string(), "turn-1".to_string())])
        );
    }

    // Defect: a window that has not loaded a session routes a Mono message
    // into a second copy of it while another window holds or runs it.
    #[test]
    fn a_mono_message_route_runs_in_the_window_that_runs_or_holds_its_session() {
        let mut inner = Inner::default();
        assert_eq!(inner.route_owner("s", "caller"), "caller");
        inner
            .loaded
            .insert("other".into(), ["s".to_string()].into_iter().collect());
        assert_eq!(inner.route_owner("s", "caller"), "other");
        inner.active.insert(
            "s".into(),
            ActiveTurn {
                window: "runner".into(),
                cwd: "/repo".into(),
                app_allowed: true,
                turn_id: "turn-1".into(),
                started_by: StartedBy::MonoCode,
            },
        );
        assert_eq!(inner.route_owner("s", "caller"), "runner");
        inner.active.remove("s");
        inner.close_window("other");
        assert_eq!(inner.route_owner("s", "caller"), "caller");
    }

    // Defect (PR 5 review M1): each window that has an idle session loaded
    // counts itself as its owner, so each one reconciles it and queues the
    // same row or notice into its own copy.
    #[test]
    fn two_windows_with_a_session_loaded_agree_on_one_owner() {
        let mut inner = Inner::default();
        for label in ["second", "main"] {
            inner.loaded.insert(
                label.into(),
                ["giga-chat".to_string()].into_iter().collect(),
            );
        }
        let owners: Vec<&str> = ["main", "second", "third"]
            .into_iter()
            .filter(|window| inner.route_owner("giga-chat", window) == *window)
            .collect();
        assert_eq!(owners, ["main"]);
    }
    #[test]
    fn app_tokens_are_bound_to_active_turns_and_cannot_control_orchestration() {
        let mut inner = Inner::default();
        inner.app_grants.insert(
            "ordinary".into(),
            Grant {
                window: "main".into(),
                session: "ordinary".into(),
                cwd: "/repo".into(),
                token: "app-token".into(),
            },
        );
        assert_eq!(
            request_grant(&inner, "app", "app-token").err(),
            Some(Failure::Refused(Refusal::TurnInactive))
        );
        inner.active.insert(
            "ordinary".into(),
            ActiveTurn {
                window: "main".into(),
                cwd: "/repo".into(),
                app_allowed: true,
                turn_id: "turn-1".into(),
                started_by: StartedBy::MonoCode,
            },
        );
        assert!(request_grant(&inner, "app", "app-token").is_ok());
        assert!(request_grant(&inner, "control", "app-token").is_err());
        inner.active.get_mut("ordinary").unwrap().app_allowed = false;
        assert!(request_grant(&inner, "app", "app-token").is_err());
        inner.active.remove("ordinary");
        assert!(request_grant(&inner, "app", "app-token").is_err());
    }

    fn app_grant(inner: &mut Inner, session: &str) -> String {
        assert!(inner.prepare_app_grant(session, "main", "/repo"));
        inner.active.insert(
            session.into(),
            ActiveTurn {
                window: "main".into(),
                cwd: "/repo".into(),
                app_allowed: true,
                turn_id: "turn-1".into(),
                started_by: StartedBy::MonoCode,
            },
        );
        inner.app_grants[session].token.clone()
    }

    fn app_response(inner: &Inner, token: &str) -> Value {
        match request_grant(inner, "app", token) {
            Ok(grant) => json!({"ok": true, "session": grant.session}),
            Err(failure) => failure.response(),
        }
    }

    // PR 5 review m6 (R-e). Defect: the control event carries no turn, or
    // another turn than the one that authorized the call, so the web app
    // binds the call to the wrong handles.
    #[test]
    fn a_call_is_bound_to_the_turn_that_authorized_it() {
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let (_, events) = host_events(&inner, "main", "mono-chat", "r1", 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, "mono-chat");
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn an_unknown_token_is_refused_with_code_unknown_token() {
        let mut inner = Inner::default();
        app_grant(&mut inner, "mono-chat");
        assert_eq!(
            app_response(&inner, "not-a-token"),
            json!({
                "ok": false,
                "code": "unknown_token",
                "hint": UNKNOWN_TOKEN,
                "error": UNKNOWN_TOKEN,
                "retryable": false,
            })
        );
    }

    #[test]
    fn a_token_after_its_turn_finished_is_refused_with_code_turn_inactive() {
        let mut inner = Inner::default();
        let token = app_grant(&mut inner, "mono-chat");
        assert_eq!(app_response(&inner, &token)["session"], "mono-chat");
        inner.active.remove("mono-chat");
        assert_eq!(
            app_response(&inner, &token),
            json!({
                "ok": false,
                "code": "turn_inactive",
                "hint": APP_TURN_INACTIVE,
                "error": APP_TURN_INACTIVE,
                "retryable": false,
            })
        );
    }

    #[test]
    fn a_forgotten_session_token_is_unknown_and_other_sessions_keep_theirs() {
        let mut inner = Inner::default();
        let token = app_grant(&mut inner, "mono-chat");
        let other = app_grant(&mut inner, "other-chat");
        inner
            .scratch
            .insert("mono-chat".into(), PathBuf::from("/tmp/scratch"));
        inner.forget_session("mono-chat");
        assert_eq!(app_response(&inner, &token)["code"], "unknown_token");
        assert!(!inner.active.contains_key("mono-chat"));
        assert!(!inner.scratch.contains_key("mono-chat"));
        assert_eq!(app_response(&inner, &other)["session"], "other-chat");
        // The next turn of an unarchived session must mint a new token.
        let fresh = app_grant(&mut inner, "mono-chat");
        assert_ne!(fresh, token);
    }

    #[cfg(unix)]
    #[test]
    fn forgetting_a_session_kills_its_provider_and_numbered_children() {
        use crate::harness::tests::exits_soon;
        let inner = Mutex::new(Inner::default());
        let token = app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let harness = crate::harness::HarnessHost::new();
        let mut provider = harness.insert_test_child("mono-chat");
        let mut numbered = harness.insert_test_child("mono-chat#0");
        let mut other = harness.insert_test_child("other-chat");
        forget_session(&inner, &messaging_store(), &harness, "mono-chat");
        assert_eq!(
            app_response(&inner.lock().unwrap(), &token)["code"],
            "unknown_token"
        );
        assert!(exits_soon(&mut provider), "the provider kept running");
        assert!(
            exits_soon(&mut numbered),
            "the mono-chat#0 child kept running"
        );
        assert!(matches!(other.try_wait(), Ok(None)));
        let _ = other.kill();
        let _ = other.wait();
    }

    // Defect: closing a window kills a session's provider by exact key and
    // leaves its antigravity child `S#0` running with the session's token.
    #[cfg(unix)]
    #[test]
    fn closing_a_window_kills_its_sessions_numbered_children() {
        use crate::harness::tests::exits_soon;
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let harness = crate::harness::HarnessHost::new();
        let mut provider = harness.insert_test_child("mono-chat");
        let mut numbered = harness.insert_test_child("mono-chat#0");
        let mut other = harness.insert_test_child("other-chat");
        close_window(&inner, &messaging_store(), "main", &harness);
        assert!(exits_soon(&mut provider), "the provider kept running");
        assert!(
            exits_soon(&mut numbered),
            "the mono-chat#0 child kept running"
        );
        assert!(matches!(other.try_wait(), Ok(None)));
        let _ = other.kill();
        let _ = other.wait();
    }

    // PR 5 review n1, PR 3 review: Stop, kill and a reloaded window's boot
    // kill by exact key, so an antigravity `S#n` child keeps running with
    // the session's token.
    #[cfg(unix)]
    #[test]
    fn killing_or_booting_kills_the_sessions_numbered_children() {
        use crate::harness::tests::exits_soon;
        let store = messaging_store();
        let inner = Mutex::new(Inner::default());
        let harness = HarnessHost::new();
        let mut killed = harness.insert_test_child("a-chat#0");
        let mut other = harness.insert_test_child("other-chat");
        kill_session(&inner, &store, "a-chat", &harness);
        assert!(exits_soon(&mut killed), "kill left a-chat#0 running");
        run(&inner, "giga-chat", "reloaded", "turn-g");
        let mut booted = harness.insert_test_child("giga-chat#1");
        boot_window(&inner, &store, "reloaded", &harness);
        assert!(exits_soon(&mut booted), "boot left giga-chat#1 running");
        assert!(matches!(other.try_wait(), Ok(None)));
        let _ = other.kill();
        let _ = other.wait();
    }

    fn child_env(inner: &Inner, key: &str) -> HashMap<String, String> {
        let mut cmd = Command::new("true");
        configure_session_child(inner, "127.0.0.1:9", key, &mut cmd);
        cmd.get_envs()
            .filter_map(|(name, value)| {
                Some((name.to_str()?.to_string(), value?.to_str()?.to_string()))
            })
            .collect()
    }

    // Defect: antigravity spawns its child as `S#n`, the grant lookup uses
    // the whole key, and the child gets no token, so neither the CLI nor
    // `app-mcp` works there.
    #[test]
    fn a_numbered_child_gets_its_sessions_app_token_and_no_other() {
        let mut inner = Inner::default();
        let token = app_grant(&mut inner, "mono-chat");
        let env = child_env(&inner, "mono-chat#3");
        assert_eq!(env.get("MONOCODE_APP_TOKEN"), Some(&token));
        assert_eq!(
            env.get("MONOCODE_APP_ENDPOINT").map(String::as_str),
            Some("127.0.0.1:9")
        );
        assert!(child_env(&inner, "mono-chat-2#0").is_empty());
    }

    /// Run one host call; the window leaves the first `unanswered` attempts
    /// without a reply, as a window that answers too late does.
    fn host_events(
        inner: &Mutex<Inner>,
        window: &str,
        session: &str,
        request_id: &str,
        unanswered: usize,
    ) -> (Value, Vec<Event>) {
        let mut events = Vec::new();
        let response = host_call(
            inner,
            window,
            session,
            "messages.get_monos".into(),
            json!({}),
            request_id.into(),
            |label, event| {
                assert_eq!(label, "main");
                if events.len() >= unanswered {
                    let pending = inner.lock().unwrap().pending.remove(&event.id).unwrap();
                    pending
                        .reply
                        .send(json!({"ok": true, "monos": []}))
                        .unwrap();
                }
                events.push(event);
                true
            },
            Duration::from_millis(50),
        )
        .unwrap_or_else(Failure::response);
        (response, events)
    }

    // Defect: an omp host tool call runs for a session with no opted-in turn,
    // or from a window that does not run that turn.
    #[test]
    fn a_host_tool_call_without_an_active_turn_is_turn_inactive_and_emits_nothing() {
        let inner = Mutex::new(Inner::default());
        let (response, events) = host_events(&inner, "main", "mono-chat", "r1", 0);
        assert_eq!(response["code"], "turn_inactive");
        assert!(events.is_empty());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let (response, events) = host_events(&inner, "other", "mono-chat", "r1", 0);
        assert_eq!(response["code"], "turn_inactive");
        assert!(events.is_empty());
        inner.lock().unwrap().active.remove("mono-chat");
        let (response, events) = host_events(&inner, "main", "mono-chat", "r1", 0);
        assert_eq!(response["code"], "turn_inactive");
        assert!(events.is_empty());
    }

    // Defect: a host tool call reaches the window without its session, or
    // with a request id the `request_id` CHECK refuses (design 3.5).
    #[test]
    fn a_host_tool_call_in_an_active_turn_emits_one_event_for_its_session() {
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let (response, events) = host_events(&inner, "main", "mono-chat", "host_7-a", 0);
        assert_eq!(response, json!({"ok": true, "monos": []}));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, "mono-chat");
        assert_eq!(events[0].namespace, "app");
        assert_eq!(events[0].action, "messages.get_monos");
        assert_eq!(events[0].request_id, "host_7-a");
        let (response, events) = host_events(&inner, "main", "mono-chat", "toolu:01/x", 0);
        assert_eq!(response["ok"], false);
        assert!(events.is_empty());
        assert!(inner.lock().unwrap().pending.is_empty());
    }

    // PR I3. Defect: omp cannot call get_message, because the host tool
    // allowlist does not list messages.get; or the allowlist lets any app
    // action through as a host tool.
    #[test]
    fn a_host_tool_call_reaches_the_window_for_messages_get_and_not_for_other_actions() {
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let call = |action: &str| {
            let mut actions = Vec::new();
            let response = host_call(
                &inner,
                "main",
                "mono-chat",
                action.into(),
                json!({"handle": "#1"}),
                "r1".into(),
                |_, event| {
                    let pending = inner.lock().unwrap().pending.remove(&event.id).unwrap();
                    pending.reply.send(json!({"ok": true})).unwrap();
                    actions.push(event.action);
                    true
                },
                Duration::from_millis(50),
            )
            .unwrap_or_else(Failure::response);
            (response, actions)
        };
        let (response, actions) = call("messages.get");
        assert_eq!(response, json!({"ok": true}));
        assert_eq!(actions, ["messages.get"]);
        let (response, actions) = call("monos.list");
        assert_eq!(response["ok"], false);
        assert!(actions.is_empty());
    }

    // PR I3 review r1 m3. Defect: a timed-out get_message tells the model not
    // to call it again, so it goes on without the text; or a timed-out send
    // loses its "do not send it again" hint.
    #[test]
    fn a_timed_out_read_says_to_read_again_and_a_timed_out_send_does_not() {
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let timed_out = |action: &str, input: Value| {
            host_call(
                &inner,
                "main",
                "mono-chat",
                action.into(),
                input,
                action.replace('.', "-"),
                |_, _| true,
                Duration::from_millis(10),
            )
            .unwrap_or_else(Failure::response)
        };
        let read = timed_out("messages.get", json!({"handle": "#1"}));
        assert_eq!(read["code"], "outcome_unknown");
        assert_eq!(read["hint"], READ_OUTCOME_UNKNOWN);
        assert!(read["hint"]
            .as_str()
            .unwrap()
            .contains("call get_message again"));
        let send = timed_out(
            "messages.send",
            json!({"message": "Hi", "send_to": "b", "wanting_reply": false, "intent": "new"}),
        );
        assert_eq!(send["hint"], OUTCOME_UNKNOWN);
        assert!(inner.lock().unwrap().pending.is_empty());
    }
    // Defect: an omp host call that times out is not repeated, is repeated
    // under a new request id (a second delivery), or hands the model the
    // CLI's "Retry with the same request ID", which a tool caller cannot do.
    #[test]
    fn a_timed_out_host_call_is_repeated_with_its_request_id_then_reported_unknown() {
        let inner = Mutex::new(Inner::default());
        app_grant(&mut inner.lock().unwrap(), "mono-chat");
        let (response, events) = host_events(&inner, "main", "mono-chat", "req-1", 1);
        assert_eq!(response, json!({"ok": true, "monos": []}));
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.request_id == "req-1"));
        let (response, events) = host_events(&inner, "main", "mono-chat", "req-2", usize::MAX);
        assert_eq!(events.len(), 1 + TOOL_CALL_RETRIES);
        assert!(events.iter().all(|event| event.request_id == "req-2"));
        assert_eq!(response["code"], "outcome_unknown");
        assert!(!response
            .to_string()
            .contains("Retry with the same request ID"));
        assert!(inner.lock().unwrap().pending.is_empty());
    }

    // Defect: an antigravity child `S#n` of an orchestration lead gets the
    // lead's control token, and the stop paths that kill by exact key miss it.
    #[test]
    fn a_numbered_child_gets_no_orchestration_access() {
        let mut inner = Inner::default();
        inner.grants.insert(
            "lead".into(),
            Grant {
                window: "main".into(),
                session: "lead".into(),
                cwd: "/repo".into(),
                token: "control-token".into(),
            },
        );
        inner
            .scratch
            .insert("lead".into(), PathBuf::from("/tmp/scratch"));
        assert_eq!(
            child_env(&inner, "lead")
                .get("MONOCODE_CONTROL_TOKEN")
                .map(String::as_str),
            Some("control-token")
        );
        assert!(child_env(&inner, "lead#0").is_empty());
    }

    #[test]
    fn app_token_survives_normal_turns_but_only_works_when_opted_in() {
        let mut inner = Inner::default();
        assert!(inner.prepare_app_grant("ordinary", "main", "/repo"));
        let token = inner.app_grants["ordinary"].token.clone();
        inner.active.insert(
            "ordinary".into(),
            ActiveTurn {
                window: "main".into(),
                cwd: "/repo".into(),
                app_allowed: false,
                turn_id: "turn-1".into(),
                started_by: StartedBy::MonoCode,
            },
        );
        assert!(request_grant(&inner, "app", &token).is_err());
        assert!(inner.prepare_app_grant("ordinary", "main", "/repo"));
        assert_eq!(inner.app_grants["ordinary"].token, token);
        inner.grants.insert(
            "ordinary".into(),
            Grant {
                window: "main".into(),
                session: "ordinary".into(),
                cwd: "/repo".into(),
                token: "control-token".into(),
            },
        );
        assert!(!inner.prepare_app_grant("ordinary", "main", "/repo"));
        inner.active.get_mut("ordinary").unwrap().app_allowed = true;
        assert!(request_grant(&inner, "app", &token).is_err());
        inner.grants.remove("ordinary");
        assert!(inner.prepare_app_grant("ordinary", "main", "/repo"));
        assert_eq!(inner.app_grants["ordinary"].token, token);
        inner.active.get_mut("ordinary").unwrap().app_allowed = true;
        assert!(request_grant(&inner, "app", &token).is_ok());
        inner.active.remove("ordinary");
        assert!(request_grant(&inner, "app", &token).is_err());
    }
    #[cfg(not(windows))]
    #[test]
    fn comparison_keys_preserve_posix_case() {
        assert_eq!(comparison_path(Path::new("/tmp/Foo")), "/tmp/Foo");
        assert_ne!(
            comparison_path(Path::new("/tmp/Foo")),
            comparison_path(Path::new("/tmp/foo"))
        );
    }

    #[test]
    fn workers_get_distinct_private_scratch_and_matching_temp_environment() {
        let first = create_worker_scratch().unwrap();
        let second = create_worker_scratch().unwrap();
        assert_ne!(first, second);
        assert!(first.is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let mut cmd = Command::new("unused");
        configure_worker_scratch(&mut cmd, &first);
        for key in ["TMPDIR", "TMP", "TEMP"] {
            assert!(cmd
                .get_envs()
                .any(|(name, value)| name == key && value == Some(first.as_os_str())));
        }
        let new_file = first.join("new/helper.py");
        assert_eq!(
            control_write_path(new_file.to_string_lossy().into_owned()).unwrap(),
            new_file.to_string_lossy().replace('\\', "/")
        );
        assert!(control_write_path("relative.py".into()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&second, first.join("escape")).unwrap();
            let resolved = control_write_path(
                first
                    .join("escape/helper.py")
                    .to_string_lossy()
                    .into_owned(),
            )
            .unwrap();
            assert_eq!(resolved, second.join("helper.py").to_string_lossy());
            std::os::unix::fs::symlink(first.join("missing"), first.join("dangling")).unwrap();
            assert!(control_write_path(
                first
                    .join("dangling/helper.py")
                    .to_string_lossy()
                    .into_owned()
            )
            .is_err());
        }
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn closing_a_window_releases_ordinary_turns_and_owned_orchestration() {
        let mut inner = Inner::default();
        for (id, window) in [
            ("ordinary", "closing"),
            ("lead", "closing"),
            ("other", "open"),
        ] {
            inner.active.insert(
                id.into(),
                ActiveTurn {
                    window: window.into(),
                    cwd: format!("/{id}"),
                    app_allowed: false,
                    turn_id: "turn-1".into(),
                    started_by: StartedBy::MonoCode,
                },
            );
        }
        for (id, window) in [("lead", "closing"), ("other", "open")] {
            inner.grants.insert(
                id.into(),
                Grant {
                    window: window.into(),
                    session: id.into(),
                    cwd: format!("/{id}"),
                    token: id.into(),
                },
            );
        }
        inner.workers.insert("worker".into(), "lead".into());
        inner.workers.insert("other-worker".into(), "other".into());
        let (reply, response) = mpsc::channel();
        inner.pending.insert(
            "pending".into(),
            Pending {
                window: "closing".into(),
                reply,
            },
        );
        let (reply, other_response) = mpsc::channel();
        inner.pending.insert(
            "other-pending".into(),
            Pending {
                window: "open".into(),
                reply,
            },
        );

        let ended: Vec<String> = inner
            .close_window("closing")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ended, ["lead", "ordinary"]);
        assert_eq!(inner.active.len(), 1);
        assert_eq!(inner.active["other"].window, "open");
        assert_eq!(inner.grants.len(), 1);
        assert!(inner.grants.contains_key("other"));
        assert_eq!(inner.workers.len(), 1);
        assert_eq!(inner.workers["other-worker"], "other");
        assert_eq!(response.try_recv().unwrap()["ok"], false);
        assert!(other_response.try_recv().is_err());
        assert!(inner.pending.contains_key("other-pending"));
        assert!(inner.close_window("closing").is_empty());
    }

    #[test]
    fn scopes_reject_escape_and_resolve_new_files() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        assert!(resolve_scope(&root, "../escape").is_err());
        assert!(resolve_scope(&root, "/absolute").is_err());
        assert!(control_scopes(
            root.to_string_lossy().into_owned(),
            vec!["../escape".into()]
        )
        .unwrap_err()
        .contains("Invalid write scope \"../escape\""));
        assert!(resolve_scope(&root, "src/new.ts")
            .unwrap()
            .ends_with("/src/new.ts"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), root.join("outside")).unwrap();
            assert!(resolve_scope(&root, "outside/file").is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkout_reservations_include_nested_folders() {
        assert!(paths_overlap("/repo", "/repo/src"));
        assert!(paths_overlap("/repo/src", "/repo"));
        assert!(!paths_overlap("/repo", "/repo2"));
    }
}
