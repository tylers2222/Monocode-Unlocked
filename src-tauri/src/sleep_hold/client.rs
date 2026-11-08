//! `<exe> hold --harness claude|codex|omp` (alarms design 11), the pre-tool
//! hook. A shell call that does not wait in the foreground exits 0 at once
//! with no output and no network. A wait asks the app (`hold.request`, then
//! `hold.wait` long-polls) and runs only on allow; deny, no answer by the
//! client's own deadline, and every error exit 2 with a refusal on stderr.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::{
    classify, parse_payload, Harness, HoldRequest, HoldRequested, HoldWait, HoldWaitReply,
};

/// The client's own deadline. The hook timeout is 150 s and Codex fails
/// open when a hook times out, so the client must give up first.
const DEADLINE: Duration = Duration::from_secs(120);
/// The longest `hold.wait`, inside the app's reply window.
const SLICE: Duration = Duration::from_secs(30);
/// How long the app may take to answer any call (control.rs REPLY_WAIT).
const REPLY_WAIT: Duration = Duration::from_secs(35);
/// Time left for a reply to arrive after the app's wait ends.
const MARGIN: Duration = Duration::from_secs(5);
/// The least time between two `hold.wait` calls, so an app that answers
/// `pending` at once is not polled in a tight loop.
const POLL_FLOOR: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// A larger payload is cut, fails to parse, and is refused (fail closed).
const MAX_STDIN: u64 = 4_000_000;
const MAX_REPLY: u64 = 64_000;
const MAX_SHOWN: usize = 300;
const SHORT_SLEEP_SKIP: &str = "MONOCODE_SHORT_SLEEP_SKIP_S";
/// Alarms design 12 (card #87): a single wait under 10 s runs unasked.
const DEFAULT_SHORT_SLEEP_SKIP: Duration = Duration::from_secs(10);

/// Time since the client started.
pub trait Clock {
    fn elapsed(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

struct SystemClock(Instant);

impl Clock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub trait Transport {
    fn call(
        &mut self,
        action: &str,
        input: Value,
        request_id: &str,
        read_timeout: Duration,
    ) -> Result<Value, String>;
}

/// The app's loopback endpoint, one JSON line each way per call, as
/// control_cli.rs frames it, in the `hold` namespace.
pub(crate) struct Tcp {
    address: SocketAddr,
    token: String,
}

impl Tcp {
    pub(crate) fn from_env(var: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |key: &str| var(key).filter(|value| !value.is_empty());
        let (Some(endpoint), Some(token)) =
            (var("MONOCODE_APP_ENDPOINT"), var("MONOCODE_APP_TOKEN"))
        else {
            return Err("MONOCODE_APP_ENDPOINT or MONOCODE_APP_TOKEN is not set".into());
        };
        let address: SocketAddr = endpoint
            .parse()
            .map_err(|_| format!("invalid MonoCode endpoint {endpoint}"))?;
        if !address.ip().is_loopback() {
            return Err("the hold client only connects to localhost".into());
        }
        Ok(Self { address, token })
    }
}

impl Transport for Tcp {
    fn call(
        &mut self,
        action: &str,
        input: Value,
        request_id: &str,
        read_timeout: Duration,
    ) -> Result<Value, String> {
        let mut stream = TcpStream::connect_timeout(&self.address, CONNECT_TIMEOUT)
            .map_err(|error| format!("cannot connect to MonoCode at {}: {error}", self.address))?;
        stream
            .set_read_timeout(Some(read_timeout))
            .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
            .map_err(|error| error.to_string())?;
        let frame = json!({"token": self.token, "action": action, "input": input, "requestId": request_id, "namespace": "hold"});
        writeln!(stream, "{frame}").map_err(|error| format!("cannot send to MonoCode: {error}"))?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(MAX_REPLY + 1)
            .read_line(&mut line)
            .map_err(|error| format!("no reply from MonoCode: {error}"))?;
        if line.len() as u64 > MAX_REPLY {
            return Err("MonoCode's reply is too large".into());
        }
        serde_json::from_str(&line).map_err(|_| "MonoCode returned an invalid reply".to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Not a wait, or allowed: the harness's normal flow continues.
    Run,
    Refused {
        command: Option<String>,
        cause: Cause,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    Denied(Option<String>),
    NoAnswer,
    Failed(String),
}

/// The held command, for the panic hook's refusal.
static COMMAND: OnceLock<String> = OnceLock::new();

pub fn run(args: Vec<String>) -> i32 {
    let clock = SystemClock(Instant::now());
    install_panic_refusal();
    let var = |key: &str| std::env::var(key).ok();
    let outcome = match harness_arg(&args) {
        None => failed(None, "usage: hold --harness claude|codex|omp"),
        Some(harness) => match read_stdin() {
            Ok(stdin) => hold(harness, &stdin, &var, &clock, || Tcp::from_env(&var)),
            Err(error) => failed(None, &error),
        },
    };
    finish(outcome, &mut io::stderr())
}

/// Codex fails open when a hook crashes, so a panic writes the refusal and
/// exits 2 itself instead of unwinding.
fn install_panic_refusal() {
    std::panic::set_hook(Box::new(|_| {
        let cause = Cause::Failed("the hold client crashed".into());
        let text = refusal(COMMAND.get().map(String::as_str), &cause);
        let _ = writeln!(io::stderr(), "{text}");
        std::process::exit(2);
    }));
}

fn harness_arg(args: &[String]) -> Option<Harness> {
    match args {
        [flag, name] if flag == "--harness" => Harness::parse(name),
        [arg] => arg.strip_prefix("--harness=").and_then(Harness::parse),
        _ => None,
    }
}

fn read_stdin() -> Result<String, String> {
    let mut stdin = String::new();
    io::stdin()
        .take(MAX_STDIN)
        .read_to_string(&mut stdin)
        .map_err(|error| format!("cannot read the hook payload: {error}"))?;
    Ok(stdin)
}

fn failed(command: Option<String>, error: &str) -> Outcome {
    Outcome::Refused {
        command,
        cause: Cause::Failed(error.to_string()),
    }
}

pub(crate) fn hold<T: Transport>(
    harness: Harness,
    stdin: &str,
    var: &dyn Fn(&str) -> Option<String>,
    clock: &dyn Clock,
    connect: impl FnOnce() -> Result<T, String>,
) -> Outcome {
    let call = match parse_payload(harness, stdin) {
        Ok(Some(call)) => call,
        Ok(None) => return Outcome::Run,
        Err(error) => return failed(None, &error),
    };
    let Some(wait) = classify(&call.command, call.background) else {
        return Outcome::Run;
    };
    let skip_below = var(SHORT_SLEEP_SKIP)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .unwrap_or(DEFAULT_SHORT_SLEEP_SKIP);
    if wait.is_short(skip_below) {
        return Outcome::Run;
    }
    let _ = COMMAND.set(call.command.clone());
    let command = call.command.clone();
    let request = HoldRequest {
        harness,
        session_hint: call.session_hint,
        tool_use_id: call.tool_use_id,
        command: call.command,
        background: call.background,
        reason: call.reason,
        wait,
    };
    let cause = match connect().and_then(|mut transport| ask(&mut transport, clock, &request)) {
        Ok(HoldWaitReply::Allow) => return Outcome::Run,
        Ok(HoldWaitReply::Deny { reason }) => Cause::Denied(reason),
        Ok(HoldWaitReply::Timeout | HoldWaitReply::Pending) => Cause::NoAnswer,
        Err(error) => Cause::Failed(error),
    };
    Outcome::Refused {
        command: Some(command),
        cause,
    }
}

/// Asks once, then long-polls in slices that all end before the deadline.
fn ask(
    transport: &mut impl Transport,
    clock: &dyn Clock,
    request: &HoldRequest,
) -> Result<HoldWaitReply, String> {
    let id = &request.tool_use_id;
    let input = serde_json::to_value(request).map_err(|error| error.to_string())?;
    let requested: HoldRequested = read_reply(transport.call(
        "request",
        input,
        &format!("hold-{id}"),
        REPLY_WAIT + MARGIN,
    )?)?;
    let mut slice = 0u32;
    loop {
        let started = clock.elapsed();
        let Some(wait) = DEADLINE
            .checked_sub(started + MARGIN)
            .map(|left| Duration::from_secs(left.min(SLICE).as_secs()))
            .filter(|wait| !wait.is_zero())
        else {
            return Ok(HoldWaitReply::Timeout);
        };
        let input = json!(HoldWait {
            hold: &requested.hold,
            timeout_seconds: wait.as_secs(),
        });
        let request_id = format!("hold-{id}-wait-{slice}");
        match read_reply(transport.call("wait", input, &request_id, wait + MARGIN)?)? {
            HoldWaitReply::Pending => {
                slice += 1;
                let spent = clock.elapsed().saturating_sub(started);
                clock.sleep(POLL_FLOOR.saturating_sub(spent));
            }
            decided => return Ok(decided),
        }
    }
}

fn read_reply<T: DeserializeOwned>(reply: Value) -> Result<T, String> {
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = reply.get("error").and_then(Value::as_str);
        return Err(error.unwrap_or("MonoCode refused the hold").to_string());
    }
    serde_json::from_value(reply).map_err(|_| "MonoCode returned an invalid hold reply".into())
}

fn finish(outcome: Outcome, stderr: &mut impl Write) -> i32 {
    match outcome {
        Outcome::Run => 0,
        Outcome::Refused { command, cause } => {
            let _ = writeln!(stderr, "{}", refusal(command.as_deref(), &cause));
            2
        }
    }
}

/// Design 6 refusal text. Never empty: Codex reads an empty stderr as allow.
fn refusal(command: Option<&str>, cause: &Cause) -> String {
    let what = command.map_or_else(
        || "foreground wait".to_string(),
        |command| format!("foreground wait \"{}\"", shown(command)),
    );
    let why = match cause {
        Cause::NoAnswer => "no answer in 2 min".to_string(),
        Cause::Denied(Some(reason)) => format!("denied: {reason}"),
        Cause::Denied(None) => "denied".to_string(),
        Cause::Failed(error) => format!("the hold check failed: {error}"),
    };
    format!(
        "Refused: {what} was not allowed ({why}). Start it in the background (Bash run_in_background:true, omp async, or \"cmd > log 2>&1 &\"), end your turn, and let the completion notice wake you."
    )
}

fn shown(command: &str) -> String {
    if command.chars().count() <= MAX_SHOWN {
        return command.to_string();
    }
    let mut short: String = command.chars().take(MAX_SHOWN).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "hold-token";

    #[derive(Clone, Default)]
    struct FakeClock(Arc<AtomicU64>);

    impl FakeClock {
        fn advance(&self, by: Duration) {
            let millis = u64::try_from(by.as_millis()).unwrap();
            self.0.fetch_add(millis, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn elapsed(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }

        fn sleep(&self, duration: Duration) {
            self.advance(duration);
        }
    }

    struct FakeApp {
        endpoint: String,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    impl FakeApp {
        /// A loopback app that answers each request line with `reply`. It
        /// records a request before answering it.
        fn start(reply: impl Fn(&Value) -> String + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = listener.local_addr().unwrap().to_string();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut line = String::new();
                    if BufReader::new(&stream).read_line(&mut line).is_err() {
                        continue;
                    }
                    let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let answer = reply(&request);
                    seen.lock().unwrap().push(request);
                    let _ = writeln!(&stream, "{answer}");
                }
            });
            Self { endpoint, requests }
        }

        /// Grants the hold, then answers every wait with `decision`.
        fn deciding(decision: Value) -> Self {
            Self::start(move |request| match request["action"].as_str() {
                Some("request") => json!({"ok": true, "hold": "sh_1"}).to_string(),
                _ => decision.to_string(),
            })
        }

        fn requests(&self) -> Vec<Value> {
            self.requests.lock().unwrap().clone()
        }

        fn env(&self) -> Vec<(&'static str, String)> {
            vec![
                ("MONOCODE_APP_ENDPOINT", self.endpoint.clone()),
                ("MONOCODE_APP_TOKEN", TOKEN.to_string()),
            ]
        }
    }

    fn claude_bash(command: &str, background: bool) -> String {
        json!({
            "session_id": "s-1", "tool_use_id": "tu-1", "tool_name": "Bash",
            "hook_event_name": "PreToolUse",
            "tool_input": {"command": command, "description": "Wait for CI", "run_in_background": background},
        })
        .to_string()
    }

    /// Runs the client over TCP and returns its exit code and stderr.
    fn run_hold(
        harness: Harness,
        stdin: &str,
        env: &[(&str, String)],
        clock: &FakeClock,
    ) -> (i32, String) {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        let var = |key: &str| env.get(key).cloned();
        let outcome = hold(harness, stdin, &var, clock, || Tcp::from_env(&var));
        let mut stderr = Vec::new();
        let code = finish(outcome, &mut stderr);
        (code, String::from_utf8(stderr).unwrap())
    }

    /// Runs the client with a transport that must never be reached.
    fn run_offline(harness: Harness, stdin: &str, env: &[(&str, &str)]) -> (Outcome, bool) {
        let connected = Cell::new(false);
        let var = |key: &str| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        };
        let outcome = hold(harness, stdin, &var, &FakeClock::default(), || {
            connected.set(true);
            Err::<Tcp, _>("offline".to_string())
        });
        (outcome, connected.get())
    }

    #[test]
    fn allow_exits_zero_silently_after_one_request_and_one_wait() {
        // Defect: an allowed wait prints to stderr or exits non-zero, so the
        // harness blocks a command the provisioner allowed.
        let app = FakeApp::deciding(json!({"ok": true, "status": "allow"}));
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 30", false),
            &app.env(),
            &FakeClock::default(),
        );
        assert_eq!((code, stderr.as_str()), (0, ""));
        let requests = app.requests();
        assert_eq!(
            requests[0],
            json!({
                "token": TOKEN, "action": "request", "namespace": "hold", "requestId": "hold-tu-1",
                "input": {
                    "harness": "claude", "sessionHint": "s-1", "toolUseId": "tu-1",
                    "command": "sleep 30", "background": false, "reason": "Wait for CI",
                    "wait": {"kind": "sleep", "seconds": 30},
                },
            })
        );
        assert_eq!(requests[1]["action"], "wait");
        assert_eq!(requests[1]["namespace"], "hold");
        assert_eq!(
            requests[1]["input"],
            json!({"hold": "sh_1", "timeoutSeconds": 30})
        );
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn deny_exits_two_with_the_reason_in_the_refusal() {
        // Defect: a deny reads as allow, or the agent never sees why.
        let app = FakeApp::deciding(
            json!({"ok": true, "status": "deny", "reason": "poll in the background"}),
        );
        let (code, stderr) = run_hold(
            Harness::Codex,
            &claude_bash("gh run watch 9", false),
            &app.env(),
            &FakeClock::default(),
        );
        assert_eq!(code, 2);
        assert_eq!(
            stderr.trim_end(),
            refusal(
                Some("gh run watch 9"),
                &Cause::Denied(Some("poll in the background".into()))
            )
        );
        assert_eq!(app.requests()[0]["input"]["harness"], "codex");
        assert_eq!(app.requests()[0]["input"]["reason"], Value::Null);
    }

    #[test]
    fn an_app_that_never_answers_is_refused_at_the_deadline_in_slices() {
        // Defect: the client waits past its own deadline (Codex then fails
        // open), or one wait outlasts the app's 35 s reply window.
        let clock = FakeClock::default();
        let waited = clock.clone();
        let app = FakeApp::start(move |request| {
            if request["action"] == "request" {
                return json!({"ok": true, "hold": "sh_1"}).to_string();
            }
            waited.advance(Duration::from_secs(
                request["input"]["timeoutSeconds"].as_u64().unwrap(),
            ));
            json!({"ok": true, "status": "pending"}).to_string()
        });
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 600", false),
            &app.env(),
            &clock,
        );
        assert_eq!(code, 2);
        assert_eq!(
            stderr.trim_end(),
            refusal(Some("sleep 600"), &Cause::NoAnswer)
        );
        let slices: Vec<u64> = app.requests()[1..]
            .iter()
            .map(|request| request["input"]["timeoutSeconds"].as_u64().unwrap())
            .collect();
        assert!(slices.iter().all(|&slice| slice <= 30), "{slices:?}");
        assert!(clock.elapsed() + MARGIN <= DEADLINE);
        assert!(
            clock.elapsed() + MARGIN + POLL_FLOOR > DEADLINE,
            "gave up early"
        );
    }

    #[test]
    fn an_app_that_answers_pending_at_once_is_polled_at_most_once_a_second() {
        // Defect: an app that answers `pending` without waiting makes the
        // client reconnect in a tight loop until its deadline.
        let clock = FakeClock::default();
        let seen = clock.clone();
        let polled_at = Arc::new(Mutex::new(Vec::new()));
        let polls = Arc::clone(&polled_at);
        let app = FakeApp::start(move |request| {
            if request["action"] == "request" {
                return json!({"ok": true, "hold": "sh_1"}).to_string();
            }
            polls.lock().unwrap().push(seen.elapsed());
            json!({"ok": true, "status": "pending"}).to_string()
        });
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 600", false),
            &app.env(),
            &clock,
        );
        assert_eq!(
            (code, stderr.trim_end()),
            (2, refusal(Some("sleep 600"), &Cause::NoAnswer).as_str())
        );
        let polled_at = polled_at.lock().unwrap();
        assert!(
            polled_at
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= POLL_FLOOR),
            "{polled_at:?}"
        );
        let most = (DEADLINE - MARGIN).as_secs_f64() / POLL_FLOOR.as_secs_f64();
        assert!(polled_at.len() as f64 <= most, "{}", polled_at.len());
    }

    /// An app that takes `request_delay` to grant the hold, then answers
    /// each wait with `status` after the slice it was given. A delay past
    /// the client's read timeout is a socket timeout.
    struct SlowApp {
        clock: FakeClock,
        request_delay: Duration,
        status: &'static str,
    }

    impl Transport for SlowApp {
        fn call(
            &mut self,
            action: &str,
            input: Value,
            _request_id: &str,
            read_timeout: Duration,
        ) -> Result<Value, String> {
            let delay = match action {
                "request" => self.request_delay,
                _ => Duration::from_secs(input["timeoutSeconds"].as_u64().unwrap()),
            };
            if delay > read_timeout {
                self.clock.advance(read_timeout);
                return Err("no reply from MonoCode: timed out".into());
            }
            self.clock.advance(delay);
            Ok(match action {
                "request" => json!({"ok": true, "hold": "sh_1"}),
                _ => json!({"ok": true, "status": self.status}),
            })
        }
    }

    #[test]
    fn a_hold_request_answered_late_in_the_apps_reply_window_still_counts() {
        // Defect: the request waits only 5 s for a reply, so an app busy for
        // longer inside its 35 s reply window gets a wait refused that the
        // provisioner would allow.
        let ask = |status| {
            let clock = FakeClock::default();
            let app = SlowApp {
                clock: clock.clone(),
                request_delay: REPLY_WAIT - Duration::from_millis(500),
                status,
            };
            let outcome = hold(
                Harness::Claude,
                &claude_bash("sleep 30", false),
                &|_| None,
                &clock,
                || Ok(app),
            );
            (outcome, clock.elapsed())
        };
        assert_eq!(ask("allow").0, Outcome::Run);
        let (outcome, elapsed) = ask("pending");
        assert_eq!(
            outcome,
            Outcome::Refused {
                command: Some("sleep 30".into()),
                cause: Cause::NoAnswer
            }
        );
        assert!(elapsed + MARGIN <= DEADLINE, "{elapsed:?}");
    }

    #[test]
    fn an_app_timeout_is_refused_as_no_answer() {
        // Defect: the app's own timeout status is misread as allow.
        let app = FakeApp::deciding(json!({"ok": true, "status": "timeout"}));
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 30", false),
            &app.env(),
            &FakeClock::default(),
        );
        assert_eq!(
            (code, stderr.trim_end()),
            (2, refusal(Some("sleep 30"), &Cause::NoAnswer).as_str())
        );
    }

    #[test]
    fn a_server_that_is_down_is_refused() {
        // Defect: an unreachable app lets the wait run (fail open).
        let endpoint = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string();
        let env = [
            ("MONOCODE_APP_ENDPOINT", endpoint),
            ("MONOCODE_APP_TOKEN", TOKEN.to_string()),
        ];
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 30", false),
            &env,
            &FakeClock::default(),
        );
        assert_eq!(code, 2);
        assert!(
            stderr.starts_with("Refused: foreground wait \"sleep 30\""),
            "{stderr}"
        );
        assert!(stderr.contains("cannot connect to MonoCode"), "{stderr}");
    }

    #[test]
    fn garbage_and_refused_replies_are_refused() {
        // Defect: an unreadable or ok:false reply is treated as allow.
        let cases = [
            ("not json".to_string(), "invalid reply"),
            (json!({"ok": true}).to_string(), "invalid hold reply"),
            (
                json!({"ok": false, "error": "This agent's turn is not active"}).to_string(),
                "turn is not active",
            ),
        ];
        for (reply, expected) in cases {
            let app = FakeApp::start(move |_| reply.clone());
            let (code, stderr) = run_hold(
                Harness::Claude,
                &claude_bash("sleep 30", false),
                &app.env(),
                &FakeClock::default(),
            );
            assert_eq!(code, 2, "{expected}");
            assert!(stderr.contains(expected), "{stderr}");
        }
    }

    #[test]
    fn a_garbage_wait_reply_is_refused() {
        // Defect: an unknown status on the wait slice is read as allow.
        let app = FakeApp::deciding(json!({"ok": true, "status": "maybe"}));
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 30", false),
            &app.env(),
            &FakeClock::default(),
        );
        assert_eq!(code, 2);
        assert!(stderr.contains("invalid hold reply"), "{stderr}");
    }

    #[test]
    fn a_wait_without_an_app_connection_is_refused() {
        // Defect: a session MonoCode did not start runs waits unasked.
        let (code, stderr) = run_hold(
            Harness::Claude,
            &claude_bash("sleep 30", false),
            &[],
            &FakeClock::default(),
        );
        assert_eq!(code, 2);
        assert!(stderr.contains("MONOCODE_APP_ENDPOINT"), "{stderr}");
    }

    #[test]
    fn calls_that_do_not_wait_run_without_network() {
        // Defect: a background start, a non-Bash tool, or plain work opens
        // a connection or blocks.
        let omp_async =
            json!({"toolCallId": "tu-1", "command": "sleep 30", "async": true}).to_string();
        let read = json!({"session_id": "s-1", "tool_use_id": "tu-1", "tool_name": "Read", "tool_input": {"file_path": "/a"}}).to_string();
        let cases = [
            (Harness::Claude, claude_bash("sleep 30", true)),
            (Harness::Omp, omp_async),
            (Harness::Claude, read),
            (Harness::Claude, claude_bash("cargo test", false)),
        ];
        for (harness, stdin) in cases {
            assert_eq!(
                run_offline(harness, &stdin, &[]),
                (Outcome::Run, false),
                "{stdin}"
            );
        }
    }

    #[test]
    fn only_a_single_wait_under_ten_seconds_runs_unasked() {
        // Defect (card #87): a short sleep in a loop or repeated in one
        // command runs unasked, a 10 s sleep skips, or an unbounded wait
        // slips through the short-sleep skip.
        let held = |command: &str, env: &[(&str, &str)]| {
            run_offline(Harness::Claude, &claude_bash(command, false), env).1
        };
        assert!(!held("sleep 9.9", &[]));
        assert!(held("sleep 10", &[]));
        assert!(held("for i in 1 2 3; do sleep 1; done", &[]));
        assert!(held("sleep 2; sleep 2", &[]));
        assert!(held("gh pr checks 1 --watch", &[]));
        assert!(!held("sleep 5 &", &[]));
        assert!(held("sleep 1", &[(SHORT_SLEEP_SKIP, "0")]));
        assert!(!held("sleep 20", &[(SHORT_SLEEP_SKIP, "30")]));
    }

    #[test]
    fn an_unreadable_payload_is_refused_without_network() {
        // Defect: garbage stdin fails open.
        let (outcome, connected) = run_offline(Harness::Claude, "{", &[]);
        assert!(!connected);
        let mut stderr = Vec::new();
        assert_eq!(finish(outcome, &mut stderr), 2);
        assert!(String::from_utf8(stderr).unwrap().starts_with("Refused: foreground wait was not allowed (the hold check failed: unreadable hook payload"));
    }

    #[test]
    fn harness_argument_names_one_of_three_harnesses() {
        // Defect: a typo in the installed hook command silently picks a harness.
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            harness_arg(&args(&["--harness", "claude"])),
            Some(Harness::Claude)
        );
        assert_eq!(harness_arg(&args(&["--harness=omp"])), Some(Harness::Omp));
        assert_eq!(harness_arg(&args(&["--harness", "cursor"])), None);
        assert_eq!(harness_arg(&args(&[])), None);
    }

    #[test]
    fn refusal_text_matches_design_6() {
        // Defect: the refusal drifts from the text agents are told to expect.
        assert_eq!(
            refusal(Some("gh pr checks 912 --watch"), &Cause::NoAnswer),
            "Refused: foreground wait \"gh pr checks 912 --watch\" was not allowed (no answer in 2 min). Start it in the background (Bash run_in_background:true, omp async, or \"cmd > log 2>&1 &\"), end your turn, and let the completion notice wake you."
        );
        let long = "x".repeat(MAX_SHOWN + 50);
        assert!(refusal(Some(&long), &Cause::NoAnswer)
            .contains(&format!("{}…\"", "x".repeat(MAX_SHOWN))));
    }

    const PANIC_CHILD: &str = "MONOCODE_HOLD_PANIC_CHILD";

    #[test]
    fn a_panic_writes_the_refusal_and_exits_two() {
        // Defect: a panic in the hook unwinds to a crash, which Codex reads
        // as allow.
        if std::env::var_os(PANIC_CHILD).is_some() {
            install_panic_refusal();
            let _ = COMMAND.set("sleep 30".into());
            panic!("boom");
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sleep_hold::client::tests::a_panic_writes_the_refusal_and_exits_two",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(PANIC_CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains(&refusal(
                Some("sleep 30"),
                &Cause::Failed("the hold client crashed".into())
            )),
            "{stderr}"
        );
    }
}
