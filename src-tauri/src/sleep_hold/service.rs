//! The app side of the sleep hold (alarms design 6, 7, 11). The hook
//! client's `hold` namespace calls (`request`, `wait`, `ping`) and the
//! provisioner's `holds.allow` / `holds.deny` run here, in Rust, on the
//! store. A held wait reaches its provisioner as a stage `hold` outbox
//! wake, or Tyler as an OS banner; no decision by the deadline is a
//! refusal the log records.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};

use super::store::{self, Decision, HoldRow, NewHold, CLIENT_STOPPED, CLOCK_STOPPED, NO_ANSWER};
use super::{Harness, HoldWaitReply, WaitKind, MAX_WAIT};
use crate::active_clock::Active;
use crate::alarms::act::{self as alarm_act, Refusal};
use crate::alarms::{self, Provisioner, Settings};
use crate::ask_gate::Actor;
use crate::mono_messages::Notices;
use crate::session_store::SessionStore;

/// The time a hold call runs at. With the active clock stopped no hold
/// can be timed, so each hold a call touches times out (fails closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HoldTime {
    pub active: Active,
    pub wall: i64,
}

impl HoldTime {
    /// `(active, wall)` to time a hold by, while the clock runs.
    pub(crate) fn running(self) -> Option<(i64, i64)> {
        match self.active {
            Active::Running(active) => Some((active, self.wall)),
            Active::Stopped(_) => None,
        }
    }

    /// `(active, wall)` a hold closes at, the clock running or not.
    pub(crate) fn closing(self) -> (i64, i64) {
        match self.active {
            Active::Running(active) | Active::Stopped(active) => (active, self.wall),
        }
    }
}

#[cfg(test)]
impl From<(i64, i64)> for HoldTime {
    fn from((active, wall): (i64, i64)) -> Self {
        Self {
            active: Active::Running(active),
            wall,
        }
    }
}

/// `at` moved back by `by`.
fn before((active, wall): (i64, i64), by: Duration) -> (i64, i64) {
    let by = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
    (active.saturating_sub(by), wall.saturating_sub(by))
}

/// The store's hold time, as [`now`] reads it.
pub(crate) type Now<'a> = &'a dyn Fn(&Connection) -> rusqlite::Result<HoldTime>;

/// A waiting call reads its hold again this often even with no signal.
const RECHECK: Duration = Duration::from_secs(1);
/// Tyler's banner thread looks for holds this often even with no signal.
const BANNER_POLL: Duration = Duration::from_secs(5);
const MAX_COMMAND: usize = 100_000;
const MAX_TOOL_USE_ID: usize = 200;
const MAX_REASON: usize = 2_000;
const SHOWN_COMMAND: usize = 300;
const BANNER_COMMAND: usize = 120;
#[cfg(any(target_os = "macos", test))]
const DENIED_FROM_BANNER: &str = "denied from notification";

/// Moves on every new hold and every decision, so a waiting `hold.wait`
/// and Tyler's banner thread wake at once.
struct Signal {
    version: Mutex<u64>,
    changed: Condvar,
}

static CHANGED: Signal = Signal {
    version: Mutex::new(0),
    changed: Condvar::new(),
};

fn changed() {
    let mut version = CHANGED
        .version
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    *version += 1;
    CHANGED.changed.notify_all();
}

fn version() -> u64 {
    *CHANGED
        .version
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Blocks until something changed after `seen`, or `timeout` passed.
fn wait_change(seen: u64, timeout: Duration) {
    let version = CHANGED
        .version
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let _ = CHANGED
        .changed
        .wait_timeout_while(version, timeout, |version| *version == seen);
}

/// The client connection a `hold` call serves.
pub(crate) trait HoldCaller {
    /// How long since the app took the connection.
    fn elapsed(&self) -> Duration;
    /// Blocks up to `timeout`; a new hold or a decision ends it early.
    fn block(&self, seen: u64, timeout: Duration);
    /// The client closed its end, so it hears no answer.
    fn hung_up(&self) -> bool;
    /// `false` past the app's cap on running waits: a wait answers at once.
    fn may_block(&self) -> bool;
}

/// A call the app took off `peer` at `at`.
pub(crate) struct Received {
    pub at: Instant,
    pub peer: Option<TcpStream>,
    pub may_block: bool,
}

impl HoldCaller for Received {
    fn elapsed(&self) -> Duration {
        self.at.elapsed()
    }

    fn block(&self, seen: u64, timeout: Duration) {
        wait_change(seen, timeout);
    }

    fn hung_up(&self) -> bool {
        let Some(peer) = &self.peer else {
            return false;
        };
        if peer.set_nonblocking(true).is_err() {
            return false;
        }
        let gone = match peer.peek(&mut [0; 1]) {
            Ok(read) => read == 0,
            Err(error) => !matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted),
        };
        let _ = peer.set_nonblocking(false);
        gone
    }

    fn may_block(&self) -> bool {
        self.may_block
    }
}

/// The time of a hold call, on the live clock in every process, so a
/// process without the lease cuts on time too. The lease holder reads it
/// with no tick lag, as the tick sweeps holds by its caught-up clock.
pub(crate) fn now(store: &SessionStore, conn: &Connection) -> rusqlite::Result<HoldTime> {
    let wall = crate::session_store::now_millis();
    let live = store.alarms.now_at(Instant::now());
    Ok(HoldTime {
        active: crate::active_clock::active_at(live, conn, wall)?,
        wall,
    })
}

/// `hold.request` as the hook client sends it (`super::HoldRequest`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestInput {
    harness: Harness,
    tool_use_id: String,
    command: String,
    #[serde(default)]
    reason: Option<String>,
    wait: WireWait,
}

#[derive(Deserialize)]
struct WireWait {
    kind: WaitKind,
    seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitInput {
    hold: String,
    timeout_seconds: WaitFor,
    #[serde(default)]
    last: bool,
}

/// A wait's `timeoutSeconds`, at most [`MAX_WAIT`]. The client never asks
/// for more, so a longer one is refused before any time is added to it.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "u64")]
struct WaitFor(Duration);

impl TryFrom<u64> for WaitFor {
    type Error = String;

    fn try_from(seconds: u64) -> Result<Self, String> {
        let wait = Duration::from_secs(seconds);
        if wait > MAX_WAIT {
            return Err(format!(
                "timeoutSeconds must be 0 to {}",
                MAX_WAIT.as_secs()
            ));
        }
        Ok(Self(wait))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideInput {
    hold: String,
    #[serde(default)]
    reason: Option<String>,
}

fn parsed<T: serde::de::DeserializeOwned>(input: &Value, what: &str) -> Result<T, Refusal> {
    serde_json::from_value(input.clone())
        .map_err(|error| Refusal::InvalidInput(format!("Not a valid {what}: {error}.")))
}

fn harness_name(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
        Harness::Omp => "omp",
    }
}

fn kind_name(kind: WaitKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn kind_of(name: &str) -> Option<WaitKind> {
    serde_json::from_value(Value::String(name.into())).ok()
}

/// How long a wait runs, as the hold text says it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HowLong {
    text: String,
    unbounded: bool,
}

fn bounded(text: String) -> HowLong {
    HowLong {
        text,
        unbounded: false,
    }
}

fn unbounded(text: &str) -> HowLong {
    HowLong {
        text: text.into(),
        unbounded: true,
    }
}

fn how_long(kind: Option<WaitKind>, seconds: Option<f64>) -> HowLong {
    let time = seconds.map(duration);
    match (kind, time) {
        (Some(WaitKind::Timeout | WaitKind::ClusterWait), Some(time)) => {
            bounded(format!("up to {time}"))
        }
        (Some(WaitKind::PollLoop), Some(time)) => bounded(format!("{time} (a polling loop)")),
        (_, Some(time)) => bounded(time),
        (Some(WaitKind::WaitBuiltin), None) => unbounded("until its background jobs end"),
        (Some(WaitKind::PollLoop), None) => unbounded("until the polling loop ends"),
        (Some(WaitKind::GhWatch), None) => unbounded("until checks finish"),
        (Some(WaitKind::Watch | WaitKind::Follow), None) => unbounded("until stopped"),
        (Some(WaitKind::ClusterWait), None) => unbounded("until the resource is ready"),
        (Some(WaitKind::Timeout), None) => bounded("up to its timeout".into()),
        (Some(WaitKind::TooDeep), None) => {
            bounded("unknown (the command nests too deep to read)".into())
        }
        (Some(WaitKind::Sleep | WaitKind::Interpreter) | None, None) => {
            bounded("a length set when it runs".into())
        }
    }
}

fn duration(seconds: f64) -> String {
    if seconds < 60.0 {
        return if seconds.fract() == 0.0 {
            format!("{seconds:.0} s")
        } else {
            format!("{seconds:.1} s")
        };
    }
    let whole = seconds.round() as u64;
    let (hours, minutes, rest) = (whole / 3600, whole % 3600 / 60, whole % 60);
    match (hours, minutes, rest) {
        (0, minutes, 0) => format!("{minutes} min"),
        (0, minutes, rest) => format!("{minutes} min {rest} s"),
        (hours, 0, _) => format!("{hours} h"),
        (hours, minutes, _) => format!("{hours} h {minutes} min"),
    }
}

/// The parts of a hold its texts show.
struct Shown<'a> {
    id: &'a str,
    label: &'a str,
    actor: &'a str,
    command: &'a str,
    how_long: HowLong,
    reason: Option<&'a str>,
}

/// Design 6: the provisioner's wake for a held wait.
fn prompt(hold: &Shown) -> String {
    let id = hold.id;
    let how = if hold.how_long.unbounded {
        format!("{} (no bound)", hold.how_long.text)
    } else {
        hold.how_long.text.clone()
    };
    let reason = hold.reason.map_or_else(
        || "(none given)".to_string(),
        |reason| format!("\"{}\"", alarms::short(reason, SHOWN_COMMAND)),
    );
    format!(
        "<mono_sleep_hold hold=\"{id}\" expires_in=\"2m\">\n{} ({}) wants to wait in the foreground.\nCommand: {}\nHow long: {how}\nReason: {reason}\nAnswer: holds.allow {id} | holds.deny {id} --reason ...  No answer in 2 min = refused.\n</mono_sleep_hold>",
        hold.label,
        hold.actor,
        alarms::short(hold.command, SHOWN_COMMAND),
    )
}

/// Tyler's banner for a held wait (approved frame 3): subtitle and body.
fn banner_text(hold: &Shown) -> (String, String) {
    let mut body = vec![
        alarms::short(hold.command, BANNER_COMMAND),
        hold.how_long.text.clone(),
    ];
    if let Some(reason) = hold.reason {
        body.push(format!("\"{}\"", alarms::short(reason, BANNER_COMMAND)));
    }
    body.push("2 min to answer".into());
    (format!("{} wants to wait", hold.label), body.join(" · "))
}

/// Why this agent's waits are not held, if they are not (design 9).
fn skip_reason(settings: &Settings, provisioner: &Provisioner) -> Option<&'static str> {
    if !settings.sleep_hold {
        return Some("sleep holds are off");
    }
    match provisioner {
        Provisioner::Tyler if !settings.tyler_sleep_hold => Some("Tyler's sleep holds are off"),
        _ => None,
    }
}

fn immediate(conn: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
}

/// `hold.request` from `session`: one hold per tool call, for the agent
/// the session speaks for, asked of that agent's provisioner.
pub(crate) fn request(
    conn: &Connection,
    session: &str,
    input: &Value,
    now: (i64, i64),
) -> Result<(String, Notices), Refusal> {
    let input: RequestInput = parsed(input, "hold request")?;
    if input.tool_use_id.is_empty() || input.tool_use_id.len() > MAX_TOOL_USE_ID {
        return Err(Refusal::InvalidInput(format!(
            "toolUseId must be 1 to {MAX_TOOL_USE_ID} bytes."
        )));
    }
    if input.command.trim().is_empty() || input.command.len() > MAX_COMMAND {
        return Err(Refusal::InvalidInput(format!(
            "command must be 1 to {MAX_COMMAND} bytes."
        )));
    }
    if input
        .wait
        .seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(Refusal::InvalidInput(
            "wait.seconds must be a number of seconds, 0 or more.".into(),
        ));
    }
    let reason = input
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| alarms::short(reason, MAX_REASON));
    let tx = immediate(conn)?;
    let agent = alarm_act::caller(&tx, session)?.ok_or(Refusal::UnknownCaller)?;
    let settings = Settings::load(&tx)?;
    let provisioner = alarms::provisioner_of(&tx, &agent, settings.root_giga.as_deref())?;
    let skipped = skip_reason(&settings, &provisioner);
    let who = alarms::who(&tx, &agent)?;
    let id = store::new_id();
    let kind = kind_name(input.wait.kind);
    let shown = Shown {
        id: &id,
        label: &who.label,
        actor: &who.actor,
        command: &input.command,
        how_long: how_long(Some(input.wait.kind), input.wait.seconds),
        reason: reason.as_deref(),
    };
    let text = matches!(provisioner, Provisioner::Agent(_)).then(|| prompt(&shown));
    let (id, inserted) = store::insert(
        &tx,
        &NewHold {
            id: &id,
            actor: &agent.to_string(),
            session,
            provisioner: &provisioner.stored(),
            harness: harness_name(input.harness),
            tool_use_id: &input.tool_use_id,
            command: &input.command,
            wait_kind: &kind,
            wait_seconds: input.wait.seconds,
            reason: reason.as_deref(),
            prompt: text.as_deref(),
            skipped,
        },
        now,
    )?;
    let mut notices = Notices::default();
    if let (true, None, Provisioner::Agent(to)) = (inserted, skipped, &provisioner) {
        let to = to.to_string();
        store::enqueue(&tx, &id, &to, now.0)?;
        crate::ask_scheduler::name_chat(&tx, &mut notices, &to)?;
    }
    tx.commit()?;
    if inserted {
        changed();
    }
    Ok((id, notices))
}

fn reply(row: &HoldRow) -> HoldWaitReply {
    match row.decision {
        None => HoldWaitReply::Pending,
        Some(Decision::Allow | Decision::Skipped) => HoldWaitReply::Allow,
        Some(Decision::Deny) => HoldWaitReply::Deny {
            reason: row.decision_reason.clone(),
        },
        Some(Decision::Timeout) => HoldWaitReply::Timeout,
    }
}

/// Holds past their deadline with no decision become `timeout`. The
/// active clock's tick runs it; a read or an answer that finds its hold
/// due runs it first, so no one sees a decision the log does not have.
pub(crate) fn sweep(conn: &Connection, now: (i64, i64)) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let expired = store::expire(&tx, now)?;
    tx.commit()?;
    if expired > 0 {
        changed();
    }
    Ok(())
}

/// Hold `hold` of `session` as it stands at `now`.
pub(crate) fn status(
    conn: &Connection,
    session: &str,
    hold: &str,
    now: (i64, i64),
) -> Result<HoldWaitReply, Refusal> {
    let mine = |row: Option<HoldRow>| row.filter(|row| row.session == session);
    let mut row = mine(store::get(conn, hold)?).ok_or(Refusal::UnknownHold)?;
    if row.decision.is_none() && row.lapsed(now.0) {
        sweep(conn, now)?;
        row = mine(store::get(conn, hold)?).ok_or(Refusal::UnknownHold)?;
    }
    Ok(reply(&row))
}

/// `hold.wait`: blocks up to `timeoutSeconds` (at most 30) for a decision.
/// The hold times out when no answer can reach the client any more: at the
/// end of its `last` wait, once it hung up, when it is served after the
/// client's read timeout, or with the active clock stopped. It first reads
/// the hold as it stood when the app took the call, so a wait for a free
/// reader does not make a client that sent it on time look
/// [idle](HoldRow::lapsed), unless a tick swept the hold while the wait
/// queued (fails closed). Past the cap on running waits a `last` wait
/// closes at once, before the client's cut (fails closed). No store lock
/// is held while it blocks.
fn wait(
    store: &SessionStore,
    session: &str,
    input: &Value,
    now: Now,
    caller: &dyn HoldCaller,
) -> Result<Value, Refusal> {
    let input: WaitInput = parsed(input, "hold wait")?;
    let WaitFor(asked) = input.timeout_seconds;
    let slice = if caller.may_block() {
        asked
    } else {
        Duration::ZERO
    };
    let read_timeout = asked + super::client::MARGIN;
    let mut marked = false;
    loop {
        let seen = version();
        let spent = caller.elapsed();
        let left = slice.saturating_sub(spent);
        let running = {
            let conn = store.lock_conn()?;
            let time = now(&conn)?;
            let read = if marked {
                time.closing()
            } else {
                before(time.closing(), spent)
            };
            let status = status(&conn, session, &input.hold, read)?;
            if status != HoldWaitReply::Pending {
                return Ok(answered(&status));
            }
            let running = time.running();
            if let Some((active, _)) = running.filter(|_| !marked) {
                let left = i64::try_from(left.as_millis()).unwrap_or(i64::MAX);
                store::waiting(&conn, &input.hold, active.saturating_add(left))?;
                marked = true;
            }
            running.is_some()
        };
        let why = if !running {
            Some(CLOCK_STOPPED)
        } else if spent >= read_timeout || caller.hung_up() {
            Some(CLIENT_STOPPED)
        } else if left.is_zero() && input.last {
            Some(NO_ANSWER)
        } else {
            None
        };
        if let Some(why) = why {
            return close(store, session, &input.hold, now, why).map(|status| answered(&status));
        }
        if left.is_zero() {
            return Ok(answered(&HoldWaitReply::Pending));
        }
        caller.block(seen, left.min(RECHECK));
    }
}

/// No answer can reach the client: the hold times out now, for `why`,
/// whatever this process's clock says, unless a decision just came first.
fn close(
    store: &SessionStore,
    session: &str,
    hold: &str,
    now: Now,
    why: &str,
) -> Result<HoldWaitReply, Refusal> {
    let conn = store.lock_conn()?;
    let at = now(&conn)?.closing();
    let tx = immediate(&conn)?;
    let closed = store::time_out(&tx, hold, at, why)?;
    tx.commit()?;
    if closed {
        changed();
    }
    status(&conn, session, hold, at)
}

fn answered(status: &HoldWaitReply) -> Value {
    let mut reply = serde_json::to_value(status).unwrap_or_else(|_| json!({}));
    reply["ok"] = json!(true);
    reply
}

/// One `hold` namespace call of `session`: the reply, and the chats it
/// names, which the caller emits after every lock is let go. A request is
/// stamped when the app took its connection, as the client counts its cut
/// from before it sent it, so a wait for a free reader does not move the
/// app's cut past the client's. Its first wait is due at once.
pub(crate) fn call(
    store: &SessionStore,
    session: &str,
    action: &str,
    input: &Value,
    now: Now,
    caller: &dyn HoldCaller,
) -> (Value, Notices) {
    let result = match action {
        "request" => store
            .lock_conn()
            .map_err(Refusal::from)
            .and_then(|conn| {
                let time = now(&conn)?.running().ok_or(Refusal::ClockStale)?;
                let (hold, notices) =
                    request(&conn, session, input, before(time, caller.elapsed()))?;
                store::waiting(&conn, &hold, time.0)?;
                Ok((hold, notices))
            })
            .map(|(hold, notices)| (json!({"ok": true, "hold": hold}), notices)),
        "wait" => wait(store, session, input, now, caller).map(|reply| (reply, Notices::default())),
        // Through the taps' queue, behind the spawn that resets the hold.
        "ping" => Ok((
            json!({"ok": true, "recorded": store.alarms.hold_pinged(session)}),
            Notices::default(),
        )),
        _ => Err(Refusal::InvalidInput(
            "Unknown hold action. The hold namespace takes request, wait and ping.".into(),
        )),
    };
    result.unwrap_or_else(|refusal| (refusal.response(), Notices::default()))
}

/// `chain[0]` is the agent, `chain[1]` the hold's provisioner as stored,
/// then each provisioner above it up to Tyler.
fn authorize(conn: &Connection, row: &HoldRow, caller: &Actor) -> Result<(), Refusal> {
    let agent = Actor::parse(&row.actor)
        .ok_or_else(|| Refusal::from(format!("hold {} has no agent", row.id)))?;
    let settings = Settings::load(conn)?;
    let provisioner = Actor::parse(&row.provisioner).map_or(Provisioner::Tyler, Provisioner::Agent);
    let mut chain = vec![Provisioner::Agent(agent)];
    chain.extend(alarms::climb(provisioner, |actor| {
        alarms::provisioner_of(conn, actor, settings.root_giga.as_deref())
    })?);
    match alarms::standing(&chain, caller) {
        Some(_) => Ok(()),
        None => Err(Refusal::NotYourHold),
    }
}

/// `holds.allow` / `holds.deny` from `session`. The first decision wins;
/// the same call repeated with its request id gets the same answer.
pub(crate) fn decide(
    conn: &Connection,
    session: &str,
    action: &str,
    input: &Value,
    request_id: &str,
    now: HoldTime,
) -> Result<Value, Refusal> {
    let decision = match action {
        "holds.allow" => Decision::Allow,
        "holds.deny" => Decision::Deny,
        _ => {
            return Err(Refusal::InvalidInput(
                "Unknown holds action. Use holds.allow or holds.deny.".into(),
            ))
        }
    };
    let input: DecideInput = parsed(input, action)?;
    let reason = match (decision, input.reason.as_deref().map(str::trim)) {
        (Decision::Allow, Some(_)) => {
            return Err(Refusal::InvalidInput("holds.allow takes only hold.".into()))
        }
        (_, Some(reason)) if reason.is_empty() || reason.chars().count() > MAX_REASON => {
            return Err(Refusal::InvalidInput(format!(
                "reason must be 1 to {MAX_REASON} characters."
            )))
        }
        (_, reason) => reason,
    };
    let tx = immediate(conn)?;
    let caller = alarm_act::caller(&tx, session)?.ok_or(Refusal::UnknownCaller)?;
    let row = store::get(&tx, &input.hold)?.ok_or(Refusal::UnknownHold)?;
    authorize(&tx, &row, &caller)?;
    let result = json!({"hold": row.id, "decision": decision.as_str()});
    let by = caller.to_string();
    match row.decision {
        Some(done)
            if done == decision
                && row.decided_by.as_deref() == Some(by.as_str())
                && row.decided_request_id.as_deref() == Some(request_id) =>
        {
            return Ok(result)
        }
        Some(done) => return Err(Refusal::HoldDecided(done)),
        None => {}
    }
    let at = match reachable(&tx, &row, now)? {
        Ok(at) => at,
        Err(refusal) => {
            tx.commit()?;
            changed();
            return Err(refusal);
        }
    };
    store::decide(&tx, &row.id, decision, &by, reason, Some(request_id), at)?;
    tx.commit()?;
    changed();
    Ok(result)
}

/// The time to decide undecided hold `row` at. When no answer can reach
/// its client at `now`, the hold times out and the answer that found it
/// so is refused.
fn reachable(
    tx: &Connection,
    row: &HoldRow,
    now: HoldTime,
) -> rusqlite::Result<Result<(i64, i64), Refusal>> {
    let Some(at) = now.running() else {
        store::time_out(tx, &row.id, now.closing(), CLOCK_STOPPED)?;
        return Ok(Err(Refusal::ClockStale));
    };
    if row.lapsed(at.0) {
        store::expire(tx, at)?;
        return Ok(Err(Refusal::HoldDecided(Decision::Timeout)));
    }
    Ok(Ok(at))
}

/// `holds.*` as an app action: `{ok, result}` or the refusal.
pub(crate) fn decide_call(
    store: &SessionStore,
    session: &str,
    action: &str,
    input: &Value,
    request_id: &str,
    now: Now,
) -> Value {
    let decided = store.lock_conn().map_err(Refusal::from).and_then(|conn| {
        let at = now(&conn)?;
        decide(&conn, session, action, input, request_id, at)
    });
    match decided {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(refusal) => refusal.response(),
    }
}

/// Tyler answered hold `hold` from its banner. `false` when it was
/// decided or expired already, or is not Tyler's.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn tyler_decides(
    conn: &Connection,
    hold: &str,
    allow: bool,
    now: HoldTime,
) -> rusqlite::Result<bool> {
    let tx = immediate(conn)?;
    let Some(row) = store::get(&tx, hold)?.filter(|row| row.provisioner == "tyler") else {
        return Ok(false);
    };
    if row.decision.is_some() {
        return Ok(false);
    }
    let Ok(at) = reachable(&tx, &row, now)? else {
        tx.commit()?;
        changed();
        return Ok(false);
    };
    let (decision, reason) = if allow {
        (Decision::Allow, None)
    } else {
        (Decision::Deny, Some(DENIED_FROM_BANNER))
    };
    let decided = store::decide(&tx, hold, decision, "tyler", reason, None, at)?;
    tx.commit()?;
    if decided {
        changed();
    }
    Ok(decided)
}

/// A banner Tyler gets for a held wait, identifier `hold:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Banner {
    pub hold: String,
    pub subtitle: String,
    pub body: String,
}

fn banner(conn: &Connection, row: &HoldRow) -> rusqlite::Result<Banner> {
    let label = match Actor::parse(&row.actor) {
        Some(actor) => alarms::who(conn, &actor)?.label,
        None => row.actor.clone(),
    };
    let (subtitle, body) = banner_text(&Shown {
        id: &row.id,
        label: &label,
        actor: &row.actor,
        command: &row.command,
        how_long: how_long(kind_of(&row.wait_kind), row.wait_seconds),
        reason: row.reason.as_deref(),
    });
    Ok(Banner {
        hold: row.id.clone(),
        subtitle,
        body,
    })
}

/// One pass of Tyler's banners: each new Tyler hold is shown once, and
/// each shown hold that is decided or expired is withdrawn once, by the
/// pass that claims it first in any process. The store lock is let go
/// while `show` and `withdraw` run. A banner that failed stays claimed, so
/// a denied OS permission does not retry every pass.
pub(crate) fn notify_tyler(
    store: &SessionStore,
    wall: i64,
    mut show: impl FnMut(&Banner) -> Result<(), String>,
    mut withdraw: impl FnMut(&str),
) -> Result<(), String> {
    let (banners, settled) = {
        let conn = store.lock_conn()?;
        let banners = store::tyler_unshown(&conn)
            .and_then(|rows| {
                rows.iter()
                    .map(|row| banner(&conn, row))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|error| error.to_string())?;
        let settled = store::tyler_settled(&conn).map_err(|error| error.to_string())?;
        (banners, settled)
    };
    for banner in &banners {
        if !store::claim_shown(&*store.lock_conn()?, &banner.hold, wall)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        if let Err(error) = show(banner) {
            eprintln!("Could not show the sleep hold {}: {error}", banner.hold);
        }
    }
    for hold in &settled {
        if store::claim_withdrawn(&*store.lock_conn()?, hold, wall)
            .map_err(|error| error.to_string())?
        {
            withdraw(hold);
        }
    }
    Ok(())
}

/// Starts the thread that puts Tyler's sleep holds up as OS banners and
/// takes them down once decided. Only the process that holds the clock
/// lease shows them, as `alarms::init`.
pub(crate) fn init(app: &tauri::AppHandle) {
    use tauri::Manager;
    let app = app.clone();
    std::thread::spawn(move || loop {
        let seen = version();
        if app.state::<SessionStore>().alarms.now().is_none() {
            wait_change(seen, BANNER_POLL);
            continue;
        }
        let shown = notify_tyler(
            &app.state::<SessionStore>(),
            crate::session_store::now_millis(),
            |banner| {
                tauri::async_runtime::block_on(crate::notifications::show_hold(
                    &app,
                    &banner.hold,
                    &banner.subtitle,
                    &banner.body,
                ))
            },
            crate::notifications::withdraw_hold,
        );
        if let Err(error) = shown {
            eprintln!("Could not show Tyler's sleep holds: {error}");
        }
        wait_change(seen, BANNER_POLL);
    });
}

/// Tyler pressed Allow or Deny on hold `hold`'s banner.
#[cfg(target_os = "macos")]
pub(crate) fn banner_answer<R: tauri::Runtime>(app: &tauri::AppHandle<R>, hold: &str, allow: bool) {
    use tauri::Manager;
    let store = app.state::<SessionStore>();
    let decided = store.lock_conn().and_then(|conn| {
        let now = now(&store, &conn).map_err(|error| error.to_string())?;
        tyler_decides(&conn, hold, allow, now).map_err(|error| error.to_string())
    });
    if let Err(error) = decided {
        eprintln!("Could not record Tyler's answer to {hold}: {error}");
    }
}

/// The session a click on hold `hold`'s banner opens: the held agent's.
pub(crate) fn hold_session<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    hold: &str,
) -> Option<String> {
    use tauri::Manager;
    let store = app.state::<SessionStore>();
    let conn = store.lock_conn().ok()?;
    store::get(&conn, hold).ok()?.map(|row| row.session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mono_messages::{MonoId, SessionId};

    fn shown<'a>(how_long: HowLong, reason: Option<&'a str>) -> Shown<'a> {
        Shown {
            id: "sh_K1",
            label: "PR S1 builder",
            actor: "session:9f3c",
            command: "gh pr checks 912 --watch",
            how_long,
            reason,
        }
    }

    #[test]
    fn the_hold_wake_reads_as_design_six() {
        // Defect: the provisioner's wake drifts from design 6, so it does
        // not name the command, the bound or the two answers it can give.
        let gh = how_long(Some(WaitKind::GhWatch), None);
        assert_eq!(
            prompt(&shown(gh.clone(), Some("Wait for CI"))),
            "<mono_sleep_hold hold=\"sh_K1\" expires_in=\"2m\">\n\
             PR S1 builder (session:9f3c) wants to wait in the foreground.\n\
             Command: gh pr checks 912 --watch\n\
             How long: until checks finish (no bound)\n\
             Reason: \"Wait for CI\"\n\
             Answer: holds.allow sh_K1 | holds.deny sh_K1 --reason ...  No answer in 2 min = refused.\n\
             </mono_sleep_hold>"
        );
        assert!(prompt(&shown(gh.clone(), None)).contains("\nReason: (none given)\n"));
        assert_eq!(
            banner_text(&shown(gh, Some("Wait for CI"))),
            (
                "PR S1 builder wants to wait".to_string(),
                "gh pr checks 912 --watch · until checks finish · \"Wait for CI\" · 2 min to answer"
                    .to_string()
            )
        );
    }

    #[test]
    fn how_long_names_each_bound() {
        // Defect: a bounded wait reads as unbounded, or the reverse, so the
        // provisioner answers a ten-minute sleep as if it never ended.
        let text = |kind, seconds| {
            let how = how_long(Some(kind), seconds);
            (how.text, how.unbounded)
        };
        let rows = [
            (WaitKind::Sleep, Some(30.0), "30 s", false),
            (WaitKind::Sleep, Some(9.9), "9.9 s", false),
            (WaitKind::Sleep, Some(600.0), "10 min", false),
            (WaitKind::Sleep, Some(90.0), "1 min 30 s", false),
            (WaitKind::Sleep, Some(5400.0), "1 h 30 min", false),
            (WaitKind::Sleep, None, "a length set when it runs", false),
            (WaitKind::Timeout, Some(600.0), "up to 10 min", false),
            (WaitKind::ClusterWait, Some(120.0), "up to 2 min", false),
            (
                WaitKind::ClusterWait,
                None,
                "until the resource is ready",
                true,
            ),
            (
                WaitKind::PollLoop,
                Some(300.0),
                "5 min (a polling loop)",
                false,
            ),
            (
                WaitKind::PollLoop,
                None,
                "until the polling loop ends",
                true,
            ),
            (
                WaitKind::WaitBuiltin,
                None,
                "until its background jobs end",
                true,
            ),
            (WaitKind::Follow, None, "until stopped", true),
            (WaitKind::Watch, None, "until stopped", true),
            (WaitKind::GhWatch, None, "until checks finish", true),
            (
                WaitKind::TooDeep,
                None,
                "unknown (the command nests too deep to read)",
                false,
            ),
        ];
        for (kind, seconds, words, open) in rows {
            assert_eq!(
                text(kind, seconds),
                (words.to_string(), open),
                "{kind:?} {seconds:?}"
            );
        }
    }

    #[test]
    fn each_decision_reaches_the_client_as_its_typed_reply() {
        // Defect: skipped reads as pending (the agent waits 2 min for
        // nothing), or a deny loses its reason on the wire.
        let row = |decision, reason: Option<&str>| HoldRow {
            id: "sh_1".into(),
            actor: "session:w".into(),
            session: "w".into(),
            provisioner: "session:l".into(),
            command: "sleep 30".into(),
            wait_kind: "sleep".into(),
            wait_seconds: Some(30.0),
            reason: None,
            deadline_active: 120_000,
            decision,
            decided_by: None,
            decision_reason: reason.map(String::from),
            decided_request_id: None,
            waiting_until: None,
        };
        let wire = |decision, reason| {
            let reply = answered(&super::reply(&row(decision, reason)));
            serde_json::from_value::<HoldWaitReply>(reply.clone())
                .map(|typed| (reply["ok"].clone(), typed))
                .unwrap()
        };
        let ok = json!(true);
        assert_eq!(wire(None, None), (ok.clone(), HoldWaitReply::Pending));
        assert_eq!(
            wire(Some(Decision::Allow), None),
            (ok.clone(), HoldWaitReply::Allow)
        );
        assert_eq!(
            wire(Some(Decision::Skipped), Some("Tyler's sleep holds are off")),
            (ok.clone(), HoldWaitReply::Allow)
        );
        assert_eq!(
            wire(Some(Decision::Deny), Some("use CI")),
            (
                ok.clone(),
                HoldWaitReply::Deny {
                    reason: Some("use CI".into())
                }
            )
        );
        assert_eq!(
            wire(Some(Decision::Timeout), Some("no answer in 2 min")),
            (ok, HoldWaitReply::Timeout)
        );
    }

    #[test]
    fn only_the_provisioner_or_one_above_answers_a_hold() {
        // Defect: the held agent allows its own sleep, a sibling or a
        // stranger answers it, or a Giga above the lead is refused.
        let session = |id: &str| Actor::Session(SessionId::parse(id).unwrap());
        let mono = |id: &str| Actor::Mono(MonoId::parse(id).unwrap());
        let chain = [
            Provisioner::Agent(session("worker")),
            Provisioner::Agent(session("lead")),
            Provisioner::Agent(mono("mono")),
            Provisioner::Agent(mono("giga")),
            Provisioner::Tyler,
        ];
        let allowed = |caller: &Actor| alarms::standing(&chain, caller).is_some();
        assert!(!allowed(&session("worker")), "the agent itself");
        assert!(allowed(&session("lead")), "its provisioner");
        assert!(allowed(&mono("mono")), "one above");
        assert!(allowed(&mono("giga")), "the top agent");
        assert!(!allowed(&session("other-worker")), "a stranger");
        let tyler_only = [Provisioner::Agent(session("solo")), Provisioner::Tyler];
        assert_eq!(alarms::standing(&tyler_only, &mono("giga")), None);
    }

    #[test]
    fn a_refusal_has_a_code_and_a_hint() {
        // Defect: a late answer gets a bare error, so the provisioner cannot
        // tell that the first decision stands.
        let refused = Refusal::HoldDecided(Decision::Allow).response();
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["code"], "hold_decided");
        assert_eq!(refused["retryable"], false);
        assert!(refused["hint"].as_str().unwrap().contains("allow"));
        assert_eq!(
            Refusal::HoldDecided(Decision::Timeout).response()["code"],
            "hold_decided"
        );
    }
}
