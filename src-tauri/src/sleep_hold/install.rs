//! The sleep hold in each session the app starts (alarms design 11): Claude
//! Code, Codex and omp run `<exe> hold` before every shell call. The hook goes
//! in through this spawn's own flags, env and app-owned files; no user file
//! is written.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use super::claude_settings::{hooks_off, ClaudeFiles, MANAGED};
use super::Harness;
use crate::harness::LoginDir;

/// Above the client's own deadline, so the client always answers first:
/// Codex runs the command when its hook times out.
const HOOK_TIMEOUT_S: u64 = 150;
/// The omp extension finds the MonoCode binary here.
const EXE_ENV: &str = "MONOCODE_HOLD_EXE";
const OMP_EXTENSION: &str = include_str!("omp/monocode-hold.ts");
/// How long an omp session's extension has, from its spawn and into a turn,
/// to report in with `hold ping`.
pub(crate) const PING_GRACE_MS: i64 = 30_000;
const NO_PING: &str = "the omp hold extension did not report in";

/// What a spawn's install came to, for `agent_activity.hold_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HoldState {
    /// The hook is in, where the harness runs it.
    Installed,
    /// The omp extension reports in with `hold ping` inside a turn.
    AwaitingPing,
    Missing(String),
}

/// A spawn's args and env with the hold in, what that came to, and what the
/// agent sandbox must open for it.
#[derive(Debug, PartialEq)]
pub(crate) struct Install {
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(&'static str, String)>,
    pub(crate) state: HoldState,
    /// The hold's Codex home: the spawn reads and writes it.
    pub(crate) codex_home: Option<PathBuf>,
    /// Folders the spawn reads in the app's denied folders.
    pub(crate) reads: Vec<PathBuf>,
}

/// Where a spawn's harness finds what its hold depends on.
pub(crate) struct Places<'a> {
    pub(crate) exe: &'a str,
    /// `<app data>/omp/hold`: the omp extension and its timeout overlay,
    /// alone in their folder.
    pub(crate) omp: Result<&'a Path, String>,
    /// The Codex home the spawn runs on, and the folder its overlay goes in.
    pub(crate) codex: Result<(&'a Path, &'a Path), String>,
    pub(crate) claude: ClaudeFiles<'a>,
}

/// The spawn `harness_spawn` makes, as far as its hold depends on it.
pub(crate) struct Spawn<'a> {
    pub(crate) workdir: &'a Path,
    pub(crate) login: Option<&'a LoginDir>,
    /// A Mono's private Codex home, when the spawn runs on one.
    pub(crate) codex_store: Option<&'a Path>,
}

/// `harness_spawn`'s install for a session (not a helper) of `harness`.
pub(crate) fn for_spawn<R: Runtime>(
    app: &AppHandle<R>,
    harness: Harness,
    spawn: Spawn,
    args: Vec<String>,
) -> Install {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe.to_string_lossy().into_owned(),
        Err(error) => {
            let reason = format!("no MonoCode binary path: {error}");
            return missing(args, Vec::new(), reason);
        }
    };
    let store = app.state::<crate::session_store::SessionStore>();
    // Read once: a session hears a changed setting at its next spawn.
    let settings = store
        .lock_conn()
        .map_err(|error| error.to_string())
        .and_then(|conn| crate::alarms::Settings::load(&conn).map_err(|e| e.to_string()));
    let skip_s = settings
        .unwrap_or_else(|error| {
            eprintln!("Sleep hold: the alarm settings did not load ({error}); using the default");
            crate::alarms::Settings::default()
        })
        .short_sleep_skip_s;
    let data = app.path().app_data_dir().map_err(|e| e.to_string());
    let omp = data
        .as_ref()
        .map(|dir| dir.join("omp").join("hold"))
        .map_err(String::clone);
    let home = crate::dirs_home().map(PathBuf::from).unwrap_or_default();
    let codex_real = codex_home(&spawn, &home);
    let overlays = data.as_deref().map_err(String::clone).and_then(|data| {
        let name = data.file_name().ok_or("The app data folder has no name")?;
        let homes = crate::codex_mono_store::homes(data)
            .ok_or("The app data folder has no parent folder")?;
        Ok(homes.join(name).join(".hold"))
    });
    let claude_config = match spawn.login {
        Some(LoginDir::Claude(dir)) => dir.clone(),
        _ => std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.join(".claude"), PathBuf::from),
    };
    let places = Places {
        exe: &exe,
        omp: omp.as_deref().map_err(String::clone),
        codex: overlays
            .as_deref()
            .map(|root| (codex_real.as_path(), root))
            .map_err(String::clone),
        claude: ClaudeFiles {
            config: &claude_config,
            workdir: spawn.workdir,
            managed: Path::new(MANAGED),
        },
    };
    let mut install = install(harness, &places, skip_s, args);
    // Codex keeps its databases in the home, so they stay in the real one.
    let sqlite_set = std::env::var_os("CODEX_SQLITE_HOME").is_some_and(|dir| !dir.is_empty());
    if install.codex_home.is_some() && !sqlite_set {
        install.env.push((
            "CODEX_SQLITE_HOME",
            codex_real.to_string_lossy().into_owned(),
        ));
    }
    install
}

/// The Codex home the spawn would run on, as `harness_spawn` sets it.
fn codex_home(spawn: &Spawn, home: &Path) -> PathBuf {
    match (spawn.codex_store, spawn.login) {
        (Some(store), _) => store.to_path_buf(),
        (None, Some(LoginDir::Codex(dir))) => dir.clone(),
        _ => std::env::var_os("CODEX_HOME")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.join(".codex"), PathBuf::from),
    }
}

impl Install {
    /// `args` and `state` alone: no env, home or reads.
    fn plain(args: Vec<String>, state: HoldState) -> Self {
        Install {
            args,
            env: Vec::new(),
            state,
            codex_home: None,
            reads: Vec::new(),
        }
    }
}

fn missing(args: Vec<String>, env: Vec<(&'static str, String)>, reason: String) -> Install {
    Install {
        env,
        ..Install::plain(args, HoldState::Missing(reason))
    }
}

/// `args` with the hook for `harness` that runs `places.exe hold`. A hook
/// that cannot go in leaves `args` as they came, with the reason.
pub(crate) fn install(
    harness: Harness,
    places: &Places,
    skip_s: i64,
    args: Vec<String>,
) -> Install {
    let mut env = vec![(super::client::SHORT_SLEEP_SKIP, skip_s.to_string())];
    let command = hook_command(places.exe, harness);
    let installed = match harness {
        Harness::Claude => {
            claude(&command, &places.claude, &args).map(|(args, state)| Install::plain(args, state))
        }
        Harness::Codex => places.codex.clone().and_then(|(real, root)| {
            let home = super::codex_home::overlay(real, root, &command, HOOK_TIMEOUT_S)?;
            Ok(Install {
                env: vec![("CODEX_HOME", home.to_string_lossy().into_owned())],
                codex_home: Some(home),
                ..Install::plain(args.clone(), HoldState::Installed)
            })
        }),
        Harness::Omp => {
            env.push((EXE_ENV, places.exe.to_owned()));
            places.omp.clone().and_then(|dir| omp(dir, &args))
        }
    };
    match installed {
        Ok(mut install) => {
            env.append(&mut install.env);
            Install { env, ..install }
        }
        Err(reason) => missing(args, env, reason),
    }
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
/// in any settings source the spawn loads (the app's hooks toggle off sets
/// it in `--settings`) drops it, so the hold is missing.
fn claude(
    command: &str,
    files: &ClaudeFiles,
    args: &[String],
) -> Result<(Vec<String>, HoldState), String> {
    let mut args = args.to_vec();
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
    if let Some(source) = hooks_off(&settings, files, &args) {
        return Ok((
            args,
            HoldState::Missing(format!("hooks disabled in {source}")),
        ));
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

/// The extension and the handler timeout overlay, each in `dir`. omp takes
/// more than one `--config`, so this one stays apart from ADE-117's.
fn omp(dir: &Path, args: &[String]) -> Result<Install, String> {
    let extension = write_if_changed(&dir.join("monocode-hold.ts"), OMP_EXTENSION)?;
    let overlay = json!({"extensionHandlers": {"toolCallTimeoutMs": HOOK_TIMEOUT_S * 1000}});
    let overlay = crate::private_file::write_once(dir, "hold", &overlay.to_string())?;
    let path = |path: &Path| path.to_string_lossy().into_owned();
    let flags = [
        "-e".to_owned(),
        path(&extension),
        "--config".to_owned(),
        path(&overlay),
    ];
    Ok(Install {
        // omp stops at an overlay it cannot read, and loads an extension
        // only from a folder it may read (srt 0.0.81, omp 18.2.11).
        reads: vec![dir.to_path_buf()],
        ..Install::plain(prepend(&flags, args), HoldState::AwaitingPing)
    })
}

fn prepend(flags: &[String], args: &[String]) -> Vec<String> {
    if args.starts_with(flags) {
        return args.to_vec();
    }
    [flags, args].concat()
}

/// `path` holds `contents` after this, owner-only, written only when it
/// differs. A running harness read its copy at start, so a replace cannot
/// change it.
pub(super) fn write_if_changed(path: &Path, contents: &str) -> Result<PathBuf, String> {
    if std::fs::read_to_string(path).is_ok_and(|text| text == contents) {
        return Ok(path.to_path_buf());
    }
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let temp = dir.join(format!(
        ".monocode-hold-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(&temp)
        .and_then(|mut file| std::io::Write::write_all(&mut file, contents.as_bytes()))
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("{}: {e}", path.display())
        })?;
    Ok(path.to_path_buf())
}

/// Each detector pass, after the pass applied the pings it took: an omp
/// session running a turn whose extension has not reported in by its grace
/// is missing its hold.
pub(crate) fn mark_silent(conn: &Connection, active: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE agent_activity SET hold_state = 'missing', hold_missing_reason = ?2
         WHERE hold_state IS NULL AND hold_ping_due_active_ms IS NOT NULL
           AND turn_state = 'running'
           AND ?1 >= MAX(hold_ping_due_active_ms, turn_started_active_ms + ?3)",
        params![active, NO_PING, PING_GRACE_MS],
    )
}

/// The extension pings at the start of every turn, before any tool call,
/// so a turn of this spawn that ended with no ping had no hold.
/// Runs before the end of running turn `turn_id` lands.
pub(crate) fn mark_unreported(
    conn: &Connection,
    session: &str,
    turn_id: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE agent_activity SET hold_state = 'missing', hold_missing_reason = ?2
         WHERE session_id = ?1 AND turn_state = 'running' AND turn_id = ?4
           AND hold_state IS NULL AND hold_ping_due_active_ms IS NOT NULL
           AND turn_started_active_ms >= hold_ping_due_active_ms - ?3",
        params![session, NO_PING, PING_GRACE_MS, turn_id],
    )
}

#[cfg(all(test, unix))]
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

    /// Places with no omp folder or Codex home, and Claude Code settings
    /// files in `claude` (none there unless a test writes them).
    fn places<'a>(exe: &'a str, omp: Result<&'a Path, String>, claude: &'a Path) -> Places<'a> {
        Places {
            exe,
            omp,
            codex: Err("no Codex home".into()),
            claude: ClaudeFiles {
                config: claude,
                workdir: claude,
                managed: Path::new("/nonexistent/managed-settings.json"),
            },
        }
    }

    fn installed(harness: Harness, list: Vec<String>) -> Install {
        install(
            harness,
            &places(EXE, Err("no omp folder".into()), Path::new("/nonexistent")),
            10,
            list,
        )
    }

    /// A Codex install on the home `real`, its overlay in `root`.
    fn codex_at(exe: &str, real: &Path, root: &Path, list: Vec<String>) -> Install {
        let places = Places {
            codex: Ok((real, root)),
            ..places(exe, Err("no omp folder".into()), Path::new("/nonexistent"))
        };
        install(Harness::Codex, &places, 10, list)
    }

    fn omp_in(dir: &Path, exe: &str, list: Vec<String>) -> Install {
        install(
            Harness::Omp,
            &places(exe, Ok(dir), Path::new("/nonexistent")),
            10,
            list,
        )
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

    /// The config.toml of the Codex home `install` points the spawn at.
    fn codex_config(install: &Install) -> toml::Table {
        let home = install
            .env
            .iter()
            .find(|(key, _)| *key == "CODEX_HOME")
            .unwrap();
        assert_eq!(Some(Path::new(&home.1)), install.codex_home.as_deref());
        std::fs::read_to_string(Path::new(&home.1).join("config.toml"))
            .unwrap()
            .parse()
            .unwrap()
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
    // after its subcommand, which claude refuses. A login spawn takes the
    // hook too, harmlessly: it runs no Bash.
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
        let given = args(&["--settings", r#"{"hooks":{}}"#]);
        let once = installed(Harness::Claude, given);
        assert_eq!(installed(Harness::Claude, once.args.clone()), once);
        let dir = folder();
        let once = omp_in(&dir, EXE, args(&["--mode", "rpc"]));
        assert_eq!(omp_in(&dir, EXE, once.args.clone()), once);
        let (real, root) = (dir.join("codex"), dir.join("overlays"));
        let once = codex_at(EXE, &real, &root, args(&["app-server"]));
        assert_eq!(codex_at(EXE, &real, &root, once.args.clone()), once);
    }

    // Defect: with the user's hooks toggle off Claude Code drops every
    // command hook, ours too, and the hold reads as installed.
    #[test]
    fn hooks_turned_off_leave_the_hold_missing() {
        let given = args(&["--settings", r#"{"disableAllHooks":true}"#]);
        let install = installed(Harness::Claude, given.clone());
        assert_eq!(install.args, given);
        assert_eq!(
            install.state,
            HoldState::Missing("hooks disabled in --settings".into())
        );
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

    // Defect: Codex app-server gets the hook as a session flag, which it
    // lists untrusted and never runs (codex-cli 0.165.0), or the spawn's own
    // Codex home is written.
    #[test]
    fn codex_runs_the_hook_from_a_trusted_home_of_its_own() {
        let dir = folder();
        let real = dir.join("codex");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let install = codex_at(EXE, &real, &dir.join("overlays"), args(&["app-server"]));
        assert_eq!(install.state, HoldState::Installed);
        assert_eq!(install.args, args(&["app-server"]), "no session-flag hook");
        let config = codex_config(&install);
        assert_eq!(config["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            serde_json::to_value(&config["hooks"]["PreToolUse"]).unwrap(),
            json!([{"matcher": "Bash", "hooks": [{
                "type": "command",
                "command": "'/Applications/MonoCode 2.app/Contents/MacOS/monocode' hold --harness codex",
                "timeout": 150,
            }]}])
        );
        assert_eq!(
            std::fs::read_to_string(real.join("config.toml")).unwrap(),
            "model = \"gpt-5\"\n"
        );
    }

    // Defect: a Codex hook that Codex will not run (no home of the hold's
    // own, or a config the hook cannot join) is recorded as installed.
    #[test]
    fn a_codex_hook_codex_will_not_run_is_never_installed() {
        let dir = folder();
        let real = dir.join("codex");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("config.toml"), "hooks = 1").unwrap();
        let unresolved = installed(Harness::Codex, args(&["app-server"]));
        let unjoinable = codex_at(EXE, &real, &dir.join("overlays"), args(&["app-server"]));
        for install in [unresolved, unjoinable] {
            assert!(
                matches!(install.state, HoldState::Missing(_)),
                "{install:?}"
            );
            assert_eq!(install.args, args(&["app-server"]));
            assert!(
                install.env.iter().all(|(key, _)| *key != "CODEX_HOME"),
                "{install:?}"
            );
            assert_eq!(install.codex_home, None);
        }
    }

    // Defect: omp gets no extension, keeps its 30 s handler timeout (which
    // cuts a hold short), or never learns where MonoCode is.
    #[test]
    fn omp_gets_the_extension_the_timeout_and_the_binary() {
        let dir = folder();
        let install = omp_in(&dir, EXE, args(&["--mode", "rpc"]));
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
        assert_eq!(
            install.reads,
            std::slice::from_ref(&dir),
            "what srt must let omp read"
        );
        let listed: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            listed.len(),
            2,
            "the folder holds the hold's files alone: {listed:?}"
        );
    }

    // Defect: each spawn rewrites the extension a running omp may be reading,
    // or a stale copy from an older build is kept.
    #[test]
    fn the_extension_is_written_only_when_its_text_differs() {
        let dir = folder();
        let extension = dir.join("monocode-hold.ts");
        std::fs::write(&extension, "// an older build's extension").unwrap();
        omp_in(&dir, EXE, Vec::new());
        assert_eq!(std::fs::read_to_string(&extension).unwrap(), OMP_EXTENSION);
        let file = std::fs::File::options()
            .write(true)
            .open(&extension)
            .unwrap();
        file.set_modified(std::time::UNIX_EPOCH).unwrap();
        omp_in(&dir, EXE, Vec::new());
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
        let install = omp_in(&blocker, EXE, given.clone());
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
        let real = dir.join("codex");
        let places = Places {
            codex: Ok((&real, &dir)),
            ..places(EXE, Ok(&dir), &dir)
        };
        for harness in [Harness::Claude, Harness::Codex, Harness::Omp] {
            let install = install(harness, &places, 7, Vec::new());
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
            let command = match harness {
                Harness::Claude => {
                    let install = install(
                        harness,
                        &places(&exe, Err("none".into()), &dir),
                        10,
                        Vec::new(),
                    );
                    settings(&install.args)["hooks"]["PreToolUse"][0]["hooks"][0]["command"].clone()
                }
                _ => {
                    let real = dir.join("codex");
                    let install = codex_at(&exe, &real, &dir.join("overlays"), Vec::new());
                    serde_json::to_value(
                        &codex_config(&install)["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
                    )
                    .unwrap()
                }
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
        let install = omp_in(&dir.join("omp"), &exe, Vec::new());
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
