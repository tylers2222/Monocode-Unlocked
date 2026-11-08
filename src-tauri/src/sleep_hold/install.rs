//! The sleep hold in each session the app starts (alarms design 11): Claude
//! Code, Codex and omp run `<exe> hold` before every shell call. The hook goes
//! in through this spawn's own flags and env; no user file is written.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use super::Harness;

/// Above the client's own deadline, so the client always answers first:
/// Codex runs the command when its hook times out.
const HOOK_TIMEOUT_S: u64 = 150;
/// The omp extension finds the MonoCode binary here.
const EXE_ENV: &str = "MONOCODE_HOLD_EXE";
const OMP_EXTENSION: &str = include_str!("omp/monocode-hold.ts");
/// How long an omp session's extension has, from its spawn and into a turn,
/// to report in with `hold ping`.
pub(crate) const PING_GRACE_MS: i64 = 30_000;
const HOOKS_DISABLED: &str = "hooks disabled";
const NO_PING: &str = "the omp hold extension did not report in";

/// What a spawn's install came to, for `agent_activity.hold_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HoldState {
    /// The hook is in the spawn's flags.
    Installed,
    /// The omp extension reports in with `hold ping` inside a turn.
    AwaitingPing,
    Missing(String),
}

/// A spawn's args and env with the hold in, and what that came to.
#[derive(Debug, PartialEq)]
pub(crate) struct Install {
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(&'static str, String)>,
    pub(crate) state: HoldState,
}

/// `harness_spawn`'s install for a session (not a helper) of `harness`.
pub(crate) fn for_spawn<R: Runtime>(
    app: &AppHandle<R>,
    harness: Harness,
    args: Vec<String>,
) -> Install {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe.to_string_lossy().into_owned(),
        Err(error) => {
            let state = HoldState::Missing(format!("no MonoCode binary path: {error}"));
            return Install {
                args,
                env: Vec::new(),
                state,
            };
        }
    };
    let store = app.state::<crate::session_store::SessionStore>();
    let skip_s = store
        .lock_conn()
        .ok()
        .and_then(|conn| crate::alarms::Settings::load(&conn).ok())
        .unwrap_or_default()
        .short_sleep_skip_s;
    let omp_dir = app.path().app_data_dir().map(|dir| dir.join("omp"));
    install(
        harness,
        &exe,
        omp_dir.as_deref().map_err(ToString::to_string),
        skip_s,
        args,
    )
}

/// `args` with the hook for `harness` that runs `exe hold`. A hook that
/// cannot go in leaves `args` as they came, with the reason.
pub(crate) fn install(
    harness: Harness,
    exe: &str,
    omp_dir: Result<&Path, String>,
    skip_s: i64,
    args: Vec<String>,
) -> Install {
    let mut env = vec![(super::client::SHORT_SLEEP_SKIP, skip_s.to_string())];
    let installed = match harness {
        Harness::Claude => claude(&hook_command(exe, harness), args.clone()),
        Harness::Codex => codex(&hook_command(exe, harness), args.clone()),
        Harness::Omp => {
            env.push((EXE_ENV, exe.to_owned()));
            omp_dir.and_then(|dir| omp(dir, args.clone()))
        }
    };
    let (args, state) = installed.unwrap_or_else(|reason| (args, HoldState::Missing(reason)));
    Install { args, env, state }
}

/// `'<exe>' hold --harness <name>`, as the harness's shell reads it.
fn hook_command(exe: &str, harness: Harness) -> String {
    let name = match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
        Harness::Omp => "omp",
    };
    format!("'{}' hold --harness {name}", exe.replace('\'', r"'\''"))
}

/// The hook merged into Claude Code's `--settings` JSON. `disableAllHooks`
/// (the user's hooks toggle off) drops it, so the hold is missing.
fn claude(command: &str, mut args: Vec<String>) -> Result<(Vec<String>, HoldState), String> {
    let at = match args.iter().position(|arg| arg == "--settings") {
        Some(flag) => flag + 1,
        // claude refuses a root option after a subcommand (`claude auth login`).
        None => {
            args.splice(0..0, ["--settings".to_owned(), "{}".to_owned()]);
            1
        }
    };
    let Some(Ok(Value::Object(mut settings))) = args.get(at).map(|text| serde_json::from_str(text))
    else {
        return Err("Claude Code's --settings is not a JSON object".into());
    };
    if settings.get("disableAllHooks") == Some(&Value::Bool(true)) {
        return Ok((args, HoldState::Missing(HOOKS_DISABLED.into())));
    }
    let hook = json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": command, "timeout": HOOK_TIMEOUT_S, "onFailure": "block"}],
    });
    let pre = settings
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Claude Code's --settings hooks is not an object")?
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("Claude Code's --settings hooks.PreToolUse is not a list")?;
    if !pre.contains(&hook) {
        pre.push(hook);
    }
    args[at] = Value::Object(settings).to_string();
    Ok((args, HoldState::Installed))
}

/// The hook as a session-flag config layer. Codex runs a session-flag hook
/// only with its trust check bypassed (codex-cli 0.165.0).
fn codex(command: &str, args: Vec<String>) -> Result<(Vec<String>, HoldState), String> {
    let hooks = toml::Value::try_from(json!([{
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": command, "timeout": HOOK_TIMEOUT_S}],
    }]))
    .map_err(|error| format!("Codex hook config: {error}"))?;
    let flags = [
        "-c".to_owned(),
        format!("hooks.PreToolUse={hooks}"),
        "--dangerously-bypass-hook-trust".to_owned(),
    ];
    Ok((prepend(&flags, args), HoldState::Installed))
}

/// The extension and the handler timeout overlay, each in `dir`. omp takes
/// more than one `--config`, so this one stays apart from ADE-117's.
fn omp(dir: &Path, args: Vec<String>) -> Result<(Vec<String>, HoldState), String> {
    let extension = write_if_changed(&dir.join("monocode-hold.ts"), OMP_EXTENSION)?;
    let overlay = json!({"extensionHandlers": {"toolCallTimeoutMs": HOOK_TIMEOUT_S * 1000}});
    let overlay = crate::private_file::write_once(dir, "hold", &overlay.to_string())?;
    let path = |path: PathBuf| path.to_string_lossy().into_owned();
    let flags = [
        "-e".to_owned(),
        path(extension),
        "--config".to_owned(),
        path(overlay),
    ];
    Ok((prepend(&flags, args), HoldState::AwaitingPing))
}

fn prepend(flags: &[String], args: Vec<String>) -> Vec<String> {
    if args.starts_with(flags) {
        return args;
    }
    flags.iter().cloned().chain(args).collect()
}

/// `path` holds `contents` after this, written only when it differs. A
/// running omp read its copy at start, so a replace cannot change it.
fn write_if_changed(path: &Path, contents: &str) -> Result<PathBuf, String> {
    if std::fs::read_to_string(path).is_ok_and(|text| text == contents) {
        return Ok(path.to_path_buf());
    }
    let dir = path
        .parent()
        .ok_or("the omp extension path has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let temp = dir.join(format!(
        ".monocode-hold-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temp, contents)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("{}: {e}", path.display())
        })?;
    Ok(path.to_path_buf())
}

/// Each tick: an omp session running a turn whose extension has not
/// reported in by its grace is missing its hold.
pub(crate) fn mark_silent(conn: &Connection, active: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE agent_activity SET hold_state = 'missing', hold_missing_reason = ?2
         WHERE hold_state IS NULL AND hold_ping_due_active_ms IS NOT NULL
           AND turn_state = 'running'
           AND ?1 >= MAX(hold_ping_due_active_ms, turn_started_active_ms + ?3)",
        params![active, NO_PING, PING_GRACE_MS],
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    use super::*;
    use crate::sleep_hold::{parse_payload, ShellCall};

    const EXE: &str = "/Applications/MonoCode 2.app/Contents/MacOS/monocode";
    /// Claude Code 2.1.319's PreToolUse stdin for a Bash call (local probe).
    const CLAUDE_PAYLOAD: &str = r#"{"session_id":"7ee0e175-8709-4cfa-9792-76b148fe51c6","transcript_path":"/tmp/t.jsonl","cwd":"/private/tmp/p2-probe/claude","prompt_id":"d949b9ca","permission_mode":"default","effort":{"level":"medium"},"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo p2probe","description":"Echo the probe string"},"tool_use_id":"toolu_01UnFo5ikBfBDh7fFGRyXbU8"}"#;
    /// codex-cli 0.165.0's PreToolUse stdin for exec_command (local probe).
    const CODEX_PAYLOAD: &str = r#"{"session_id":"01a1b991-de44-7a70-9d6a-15a68f5c46cf","turn_id":"01a1b991-de5a-7430-b911-15a18c50af0b","transcript_path":null,"cwd":"/private/tmp/p2-probe/codex","hook_event_name":"PreToolUse","model":"mock-model","permission_mode":"bypassPermissions","tool_name":"Bash","tool_input":{"command":"echo p2probe"},"tool_use_id":"call_p2"}"#;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn folder() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("monocode-hold-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn installed(harness: Harness, list: Vec<String>) -> Install {
        install(harness, EXE, Err("no omp folder".into()), 10, list)
    }

    fn settings(args: &[String]) -> Value {
        let at = args.iter().position(|arg| arg == "--settings").unwrap();
        serde_json::from_str(&args[at + 1]).unwrap()
    }

    fn claude_hook() -> Value {
        json!({"matcher": "Bash", "hooks": [{
            "type": "command",
            "command": "'/Applications/MonoCode 2.app/Contents/MacOS/monocode' hold --harness claude",
            "timeout": 150, "onFailure": "block",
        }]})
    }

    /// The hook value of Codex's `-c hooks.PreToolUse=<toml>` flag.
    fn codex_hooks(args: &[String]) -> Value {
        let flag = args.iter().position(|arg| arg == "-c").unwrap();
        let value = args[flag + 1].strip_prefix("hooks.PreToolUse=").unwrap();
        let table: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        serde_json::to_value(&table["v"]).unwrap()
    }

    // Defect: the hold replaces the user's own hooks or other settings, or
    // moves the flags around it.
    #[test]
    fn the_claude_hook_joins_the_settings_and_hooks_already_there() {
        let user = json!({"matcher": "Edit", "hooks": [{"type": "command", "command": "lint"}]});
        let given = json!({
            "model": "opus",
            "hooks": {"PreToolUse": [user], "Stop": [{"hooks": [{"type": "command", "command": "bye"}]}]},
        });
        let install = installed(
            Harness::Claude,
            args(&["-p", "--settings", &given.to_string(), "--verbose"]),
        );
        assert_eq!(install.state, HoldState::Installed);
        assert_eq!(install.args[..2], args(&["-p", "--settings"])[..]);
        assert_eq!(install.args[3], "--verbose");
        let mut want = given.clone();
        want["hooks"]["PreToolUse"] = json!([user, claude_hook()]);
        assert_eq!(settings(&install.args), want);
    }

    // Defect: a spawn with no --settings (claude auth login) gets the flag
    // after its subcommand, which claude refuses.
    #[test]
    fn the_claude_hook_brings_its_own_settings_first() {
        let install = installed(Harness::Claude, args(&["auth", "login"]));
        assert_eq!(install.args[0], "--settings");
        assert_eq!(install.args[2..], args(&["auth", "login"])[..]);
        assert_eq!(
            settings(&install.args),
            json!({"hooks": {"PreToolUse": [claude_hook()]}})
        );
    }

    // Defect: a second install adds a second hook, so each Bash call asks twice.
    #[test]
    fn installing_twice_is_installing_once() {
        let given = args(&["--settings", r#"{"hooks":{}}"#, "app-server"]);
        for harness in [Harness::Claude, Harness::Codex] {
            let once = installed(harness, given.clone());
            let twice = installed(harness, once.args.clone());
            assert_eq!(twice, once, "{harness:?}");
        }
        let dir = folder();
        let once = install(Harness::Omp, EXE, Ok(&dir), 10, args(&["--mode", "rpc"]));
        let twice = install(Harness::Omp, EXE, Ok(&dir), 10, once.args.clone());
        assert_eq!(twice, once);
    }

    // Defect: with the user's hooks toggle off Claude Code drops every
    // command hook, ours too, and the hold reads as installed.
    #[test]
    fn hooks_turned_off_leave_the_hold_missing() {
        let given = args(&["--settings", r#"{"disableAllHooks":true}"#]);
        let install = installed(Harness::Claude, given.clone());
        assert_eq!(install.args, given);
        assert_eq!(install.state, HoldState::Missing("hooks disabled".into()));
    }

    // Defect: settings the merge cannot read stop the session from starting,
    // or start it with the hold silently absent.
    #[test]
    fn settings_that_are_not_an_object_start_the_session_with_the_hold_missing() {
        for settings in [
            "/Users/n/settings.json",
            r#"{"hooks":[]}"#,
            r#"{"hooks":{"PreToolUse":{}}}"#,
        ] {
            let given = args(&["--settings", settings]);
            let install = installed(Harness::Claude, given.clone());
            assert_eq!(install.args, given, "{settings}");
            assert!(matches!(install.state, HoldState::Missing(_)), "{settings}");
        }
        let given = args(&["--settings"]);
        assert!(matches!(
            installed(Harness::Claude, given).state,
            HoldState::Missing(_)
        ));
    }

    // Defect: Codex gets the hook after its subcommand, or without the trust
    // bypass, and never runs it (codex-cli 0.165.0 runs a session-flag hook
    // only with --dangerously-bypass-hook-trust).
    #[test]
    fn the_codex_hook_is_a_session_flag_before_the_subcommand() {
        let install = installed(Harness::Codex, args(&["app-server"]));
        assert_eq!(install.state, HoldState::Installed);
        assert_eq!(install.args[0], "-c");
        assert_eq!(
            install.args[2..],
            args(&["--dangerously-bypass-hook-trust", "app-server"])[..]
        );
        assert_eq!(
            codex_hooks(&install.args),
            json!([{"matcher": "Bash", "hooks": [{
                "type": "command",
                "command": "'/Applications/MonoCode 2.app/Contents/MacOS/monocode' hold --harness codex",
                "timeout": 150,
            }]}])
        );
    }

    // Defect: omp gets no extension, keeps its 30 s handler timeout (which
    // cuts a hold short), or never learns where MonoCode is.
    #[test]
    fn omp_gets_the_extension_the_timeout_and_the_binary() {
        let dir = folder();
        let install = install(Harness::Omp, EXE, Ok(&dir), 10, args(&["--mode", "rpc"]));
        assert_eq!(install.state, HoldState::AwaitingPing);
        let extension = dir.join("monocode-hold.ts");
        assert_eq!(
            install.args[..2],
            args(&["-e", extension.to_str().unwrap()])[..]
        );
        assert_eq!(install.args[2], "--config");
        assert_eq!(install.args[4..], args(&["--mode", "rpc"])[..]);
        assert_eq!(std::fs::read_to_string(&extension).unwrap(), OMP_EXTENSION);
        let overlay: Value =
            serde_json::from_str(&std::fs::read_to_string(&install.args[3]).unwrap()).unwrap();
        assert_eq!(
            overlay,
            json!({"extensionHandlers": {"toolCallTimeoutMs": 150_000}})
        );
        assert!(install.env.contains(&("MONOCODE_HOLD_EXE", EXE.to_owned())));
    }

    // Defect: each spawn rewrites the extension a running omp may be reading,
    // or a stale copy from an older build is kept.
    #[test]
    fn the_extension_is_written_only_when_its_text_differs() {
        let dir = folder();
        let extension = dir.join("monocode-hold.ts");
        std::fs::write(&extension, "// an older build's extension").unwrap();
        install(Harness::Omp, EXE, Ok(&dir), 10, Vec::new());
        assert_eq!(std::fs::read_to_string(&extension).unwrap(), OMP_EXTENSION);
        let file = std::fs::File::options()
            .write(true)
            .open(&extension)
            .unwrap();
        file.set_modified(std::time::UNIX_EPOCH).unwrap();
        install(Harness::Omp, EXE, Ok(&dir), 10, Vec::new());
        let modified = std::fs::metadata(&extension).unwrap().modified().unwrap();
        assert_eq!(
            modified,
            std::time::UNIX_EPOCH,
            "rewritten with the same text"
        );
    }

    // Defect: an omp spawn whose extension cannot be written fails, or
    // reads as installed.
    #[test]
    fn an_omp_folder_that_cannot_be_written_leaves_the_hold_missing() {
        let dir = folder();
        let blocker = dir.join("omp");
        std::fs::write(&blocker, "a file where the folder goes").unwrap();
        let given = args(&["--mode", "rpc"]);
        let install = install(Harness::Omp, EXE, Ok(&blocker), 10, given.clone());
        assert_eq!(install.args, given);
        assert!(
            matches!(install.state, HoldState::Missing(_)),
            "{:?}",
            install.state
        );
    }

    // Defect: the client never hears the short-sleep setting and asks for
    // every short sleep, or skips at a value the user did not set.
    #[test]
    fn every_harness_hears_the_short_sleep_setting() {
        let dir = folder();
        for harness in [Harness::Claude, Harness::Codex, Harness::Omp] {
            let install = install(harness, EXE, Ok(&dir), 7, Vec::new());
            assert!(
                install
                    .env
                    .contains(&("MONOCODE_SHORT_SLEEP_SKIP_S", "7".to_owned())),
                "{harness:?}: {:?}",
                install.env
            );
        }
    }

    /// A stand-in MonoCode binary in a folder whose name a shell would
    /// split, recording each call's argv and stdin in `dir`.
    fn stand_in(dir: &Path) -> String {
        let bin = dir.join("Mono Code's build");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("monocode");
        let log = dir.display();
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nn=$(ls '{log}' | grep -c '\\.argv$')\nprintf '%s\\0' \"$@\" > '{log}/call-'$n.argv\ncat > '{log}/call-'$n.stdin\n[ \"$2\" = ping ] && exit 0\necho 'held: no' >&2\nexit 2\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe.to_string_lossy().into_owned()
    }

    /// Each call the stand-in took: argv and stdin.
    fn calls(dir: &Path) -> Vec<(Vec<String>, String)> {
        (0..)
            .map_while(|n| {
                let argv = std::fs::read_to_string(dir.join(format!("call-{n}.argv"))).ok()?;
                let stdin = std::fs::read_to_string(dir.join(format!("call-{n}.stdin"))).unwrap();
                Some((
                    argv.split_terminator('\0').map(String::from).collect(),
                    stdin,
                ))
            })
            .collect()
    }

    /// Runs `command` as the harness's shell does, with `stdin`.
    fn run_hook(command: &str, stdin: &str) -> std::process::Output {
        let mut child = Command::new("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// The hold client's own reading of what one call got.
    fn client_reads(call: &(Vec<String>, String)) -> (Harness, Option<ShellCall>) {
        let (argv, stdin) = call;
        let [hold, flag, name] =
            <[&str; 3]>::try_from(argv.iter().map(String::as_str).collect::<Vec<_>>()).unwrap();
        assert_eq!((hold, flag), ("hold", "--harness"));
        let harness = Harness::parse(name).unwrap();
        (harness, parse_payload(harness, stdin).unwrap())
    }

    // Defect: the hook line the harness runs does not reach `<exe> hold`
    // (a path with a space or a quote splits) or names the wrong harness,
    // so the client misreads the payload or never runs.
    #[test]
    fn the_claude_and_codex_hooks_run_the_client_on_the_harness_payload() {
        let cases = [
            (
                Harness::Claude,
                CLAUDE_PAYLOAD,
                "toolu_01UnFo5ikBfBDh7fFGRyXbU8",
                Some("Echo the probe string"),
            ),
            (Harness::Codex, CODEX_PAYLOAD, "call_p2", None),
        ];
        for (harness, payload, tool_use_id, reason) in cases {
            let dir = folder();
            let exe = stand_in(&dir);
            let install = install(harness, &exe, Err("no omp folder".into()), 10, Vec::new());
            let command = match harness {
                Harness::Claude => {
                    settings(&install.args)["hooks"]["PreToolUse"][0]["hooks"][0]["command"].clone()
                }
                _ => codex_hooks(&install.args)[0]["hooks"][0]["command"].clone(),
            };
            let output = run_hook(command.as_str().unwrap(), payload);
            assert_eq!(output.status.code(), Some(2), "{harness:?}");
            assert_eq!(String::from_utf8_lossy(&output.stderr), "held: no\n");
            let [call] = <[_; 1]>::try_from(calls(&dir)).unwrap();
            let (read_as, shell) = client_reads(&call);
            assert_eq!(read_as, harness);
            let shell = shell.unwrap();
            assert_eq!(
                (
                    shell.tool_use_id.as_str(),
                    shell.command.as_str(),
                    shell.background,
                    shell.reason.as_deref()
                ),
                (tool_use_id, "echo p2probe", false, reason),
                "{harness:?}"
            );
        }
    }

    /// Loads the extension in Node with a stand-in `pi`, fires `session_start`,
    /// a foreground bash call, an `async` one and a read, and prints what each
    /// handler returned.
    const DRIVE_EXTENSION: &str = r#"
const { default: extension } = await import(process.argv[1]);
const handlers = {};
extension({ on: (name, handler) => (handlers[name] ??= []).push(handler) });
const fire = async (name, event) => {
  let result = null;
  for (const handler of handlers[name] ?? []) result = (await handler(event, {})) ?? result;
  return result;
};
const results = [
  await fire("session_start", { type: "session_start" }),
  await fire("tool_call", { type: "tool_call", toolCallId: "call_1", toolName: "bash", input: { command: "sleep 30" } }),
  await fire("tool_call", { type: "tool_call", toolCallId: "call_2", toolName: "bash", input: { command: "sleep 30", async: true } }),
  await fire("tool_call", { type: "tool_call", toolCallId: "call_3", toolName: "read", input: { path: "a" } }),
];
console.log(JSON.stringify(results));
"#;

    // Defect: the omp extension does not ping, sends a payload the client
    // cannot read, holds an `async` call, or lets a refused call run.
    #[test]
    fn the_omp_extension_pings_then_runs_the_client_on_each_foreground_bash_call() {
        let dir = folder();
        let exe = stand_in(&dir);
        let install = install(Harness::Omp, &exe, Ok(&dir.join("omp")), 10, Vec::new());
        // As omp loads a `.ts`; Node needs `.mjs` to load it as a module.
        let module = dir.join("monocode-hold.mjs");
        std::fs::copy(&install.args[1], &module).unwrap();
        let output = Command::new("node")
            .args([
                "--input-type=module",
                "-e",
                DRIVE_EXTENSION,
                module.to_str().unwrap(),
            ])
            .envs(install.env.iter().cloned())
            .output()
            .expect("node runs the omp extension test");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let results: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            results,
            json!([null, {"block": true, "reason": "held: no"}, null, null])
        );
        let calls = calls(&dir);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(
            (calls[0].0.clone(), calls[0].1.as_str()),
            (args(&["hold", "ping"]), "")
        );
        let (read_as, shell) = client_reads(&calls[1]);
        assert_eq!(read_as, Harness::Omp);
        let shell = shell.unwrap();
        assert_eq!(
            (
                shell.tool_use_id.as_str(),
                shell.command.as_str(),
                shell.background
            ),
            ("call_1", "sleep 30", false)
        );
    }
}
