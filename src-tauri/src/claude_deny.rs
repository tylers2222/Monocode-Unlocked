//! Claude Code deny rules (ADE-44), off unless `<app data>/claude-deny-rules-on`
//! exists. Off, the args stay as the front end sent them. On, `--settings` gets
//! `permissions.deny` and `sandbox` rules on every `com.monocode.*` data folder
//! beside this app's own, and Edit rules on what Claude Code loads commands
//! from (`.claude`, `.claude.json`, `.mcp.json`, the config folder of the
//! spawn's account, `CLAUDE_CONFIG_DIR` or `$HOME/.claude`, each file under a
//! `.claude` folder that a command key names, and each other spelling of any
//! of them), which run outside its sandbox. A partial guard: `COMMAND_KEYS`
//! names what it does not cover.
//!
//! Defense in depth only: the Read rule does not stop a script that opens the
//! file itself, the sandbox covers Bash only, and Bash runs unsandboxed where
//! the sandbox cannot start (`failIfUnavailable` stays off). Inside D1's srt
//! (ADE-41) the sandbox cannot start, and with the block present the session's
//! first Bash call fails trying, so a spawn that `agent_sandbox::command` wraps
//! in srt gets no `sandbox` block, and srt denies writes to the command sources
//! instead (`srt_deny`). Not to `.claude.json`: Claude Code writes it, so a
//! script inside srt can still add an MCP server there.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind::{InvalidInput, NotADirectory, NotFound};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

const ON_MARKER: &str = "claude-deny-rules-on";

pub(crate) fn args<R: Runtime>(
    app: &AppHandle<R>,
    workdir: &Path,
    account_id: Option<&str>,
    in_srt: bool,
    args: Vec<String>,
) -> Result<Vec<String>, String> {
    let (data, config_dir, home) = child_env(app, account_id)?;
    args_in(&data, config_dir.as_deref(), &home, workdir, in_srt, args)
}

/// What D1's srt must deny writes to for this spawn: inside srt, Claude Code's
/// own sandbox, which refuses these to Bash, does not run.
pub(crate) fn srt_deny<R: Runtime>(
    app: &AppHandle<R>,
    workdir: &Path,
    account_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let (data, config_dir, home) = child_env(app, account_id)?;
    srt_deny_in(&data, config_dir.as_deref(), &home, workdir)
}

/// The app data folder, the config folder the child loads (`None`:
/// `$HOME/.claude`), and its HOME. `account_id` is the spawn's Claude account
/// other than the default one.
fn child_env<R: Runtime>(
    app: &AppHandle<R>,
    account_id: Option<&str>,
) -> Result<(PathBuf, Option<OsString>, PathBuf), String> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Claude Code deny rules: no app data folder: {e}"))?;
    // HOME as `apply_gui_env` sets it.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| crate::passwd_identity().map(|id| id.home.into()))
        .unwrap_or_default();
    // The folder `apply_provider_account` gives the child, as `fs.rs` reads it.
    let config = match account_id {
        Some(id) => Some(crate::harness::provider_account_path(app, "claude", id)?.into()),
        None => std::env::var_os("CLAUDE_CONFIG_DIR"),
    };
    Ok((data, config, home))
}

/// The command sources Claude Code never writes itself, each with every link on the
/// way, spelled as the kernel sees them: srt matches that path, and a deny on a
/// link is not one on its target. Empty while the marker is absent.
pub(crate) fn srt_deny_in(
    data: &Path,
    config_dir: Option<&OsStr>,
    home: &Path,
    workdir: &Path,
) -> Result<Vec<String>, String> {
    if matches!(std::fs::exists(data.join(ON_MARKER)), Ok(false)) {
        return Ok(Vec::new());
    }
    let config = config_dir.map_or_else(|| home.join(".claude"), PathBuf::from);
    let mut deny = Vec::new();
    for (source, _) in command_sources(&config, home, workdir) {
        // As written too: srt (macOS) then blocks moving any folder on its way.
        deny.extend(source.to_str().map(str::to_owned));
        deny.extend(link_chain(&source)?);
    }
    deny.dedup();
    Ok(deny)
}

/// A marker that cannot be read counts as on: an IO error must not lower
/// protection. `config_dir` is the config folder the child loads (`None`:
/// `$HOME/.claude`), `home` its HOME, `workdir` its working folder, and
/// `in_srt` whether D1 runs it inside srt.
pub(crate) fn args_in(
    data: &Path,
    config_dir: Option<&OsStr>,
    home: &Path,
    workdir: &Path,
    in_srt: bool,
    mut args: Vec<String>,
) -> Result<Vec<String>, String> {
    if matches!(std::fs::exists(data.join(ON_MARKER)), Ok(false)) {
        return Ok(args);
    }
    let config = config_rules(config_dir, home, workdir)?;
    let Some(parent) = data.parent().and_then(Path::to_str) else {
        return Err("Claude Code deny rules: the app data path has no UTF-8 parent folder.".into());
    };
    if !parent.starts_with('/') {
        return Err("Claude Code deny rules: they need a macOS or Linux app data path. Remove claude-deny-rules-on from the app data folder to start Claude Code.".into());
    }
    let name = data.file_name().and_then(OsStr::to_str);
    if !name.is_some_and(|name| name.starts_with("com.monocode.")) {
        return Err(
            "Claude Code deny rules: the app data folder is not named com.monocode.*.".into(),
        );
    }
    let at = match args.iter().position(|arg| arg == "--settings") {
        Some(flag) => flag + 1,
        // `claude auth login` (authSupport.ts) has no --settings, and claude
        // refuses a root option after the subcommand.
        None => {
            args.insert(0, "--settings".into());
            args.insert(1, "{}".into());
            1
        }
    };
    let Some(Ok(Value::Object(mut settings))) =
        args.get(at).map(|text| serde_json::from_str::<Value>(text))
    else {
        return Err("Claude Code deny rules: --settings is not a JSON object.".into());
    };
    for key in ["permissions", "sandbox"] {
        if settings.contains_key(key) {
            return Err(format!(
                "Claude Code deny rules: --settings already has a {key} key."
            ));
        }
    }
    let folders = format!("{parent}/com.monocode.*");
    // Read and Edit rules mark an absolute path with `//`. Sandbox paths are plain.
    let mut deny = vec![
        format!("Read(/{folders}/**)"),
        format!("Edit(/{folders}/**)"),
        "Edit(//**/.claude/**)".to_owned(),
        "Edit(//**/.claude.json)".to_owned(),
        "Edit(//**/.mcp.json)".to_owned(),
    ];
    deny.extend(config);
    settings.insert("permissions".into(), json!({ "deny": deny }));
    if !in_srt {
        settings.insert(
            "sandbox".into(),
            json!({
                "enabled": true,
                "allowUnsandboxedCommands": false,
                "filesystem": { "denyRead": [folders], "denyWrite": [folders] },
            }),
        );
    }
    args[at] = Value::Object(settings).to_string();
    Ok(args)
}

/// Edit rules on the config folder, and on each link and real path of a command
/// source that the fixed rules miss (one generic rule for every source).
fn config_rules(
    config_dir: Option<&OsStr>,
    home: &Path,
    workdir: &Path,
) -> Result<Vec<String>, String> {
    let config = config_dir.map_or_else(|| home.join(".claude"), PathBuf::from);
    // Claude Code matches a rule's path as written, so `.` or `..` would match nothing.
    let Some(text) = config.to_str().filter(|text| {
        text.starts_with('/') && !text.split('/').any(|part| part == "." || part == "..")
    }) else {
        return Err("Claude Code deny rules: CLAUDE_CONFIG_DIR (or $HOME/.claude) is not an absolute path without . or .. segments.".into());
    };
    let mut rules = vec![format!("Edit(/{text}/**)")];
    let roots = [home, workdir].map(|dir| (dir.join(".claude"), "/**"));
    let files = [home, config.as_path()].map(|dir| (dir.join(".claude.json"), ""));
    let sources = [(config.clone(), "/**")]
        .into_iter()
        .chain(roots)
        .chain(command_sources(&config, home, workdir))
        .chain(files);
    for (path, tail) in sources {
        let mut chain = link_chain(&path)?;
        if let Some(real) = chain.last_mut() {
            real.push_str(tail);
        }
        for at in chain {
            if !covered(&rules, &at) {
                rules.push(format!("Edit(/{at})"));
            }
        }
    }
    Ok(rules)
}

/// Whether `rules` or the fixed `.claude/**`, `.claude.json` and `.mcp.json`
/// rules match `path`.
fn covered(rules: &[String], path: &str) -> bool {
    let (folders, name) = path.rsplit_once('/').unwrap_or_default();
    folders.split('/').any(|folder| folder == ".claude")
        || [".claude.json", ".mcp.json"].contains(&name)
        || rules
            .iter()
            .filter_map(|rule| rule.strip_prefix("Edit(/")?.strip_suffix(')'))
            .any(|rule| {
                rule == path || rule.strip_suffix("**").is_some_and(|d| path.starts_with(d))
            })
}

/// What Claude Code loads commands from and never writes itself, a folder with
/// `/**`: the settings and MCP files, each `.claude` path a command in them
/// names (`$CLAUDE_PROJECT_DIR`, `$HOME` and `~` expanded, a relative path from
/// `workdir`), and the agent, skill and command folders (hooks, `!` commands).
fn command_sources(config: &Path, home: &Path, workdir: &Path) -> Vec<(PathBuf, &'static str)> {
    let project = workdir.join(".claude");
    let mut sources = vec![
        (config.join("settings.json"), ""),
        (home.join(".claude/settings.json"), ""),
        (project.join("settings.json"), ""),
        (project.join("settings.local.json"), ""),
        (workdir.join(".mcp.json"), ""),
    ];
    let vars = [
        ("$CLAUDE_PROJECT_DIR", workdir),
        ("$HOME", home),
        ("~", home),
    ];
    for command in sources
        .iter()
        .flat_map(|(file, _)| setting_commands(file))
        .collect::<Vec<_>>()
    {
        for word in command.split(|c: char| c.is_whitespace() || ";&|()<>`".contains(c)) {
            let (base, rest) = vars
                .iter()
                .find_map(|(var, base)| {
                    Some((*base, word.strip_prefix(var)?.trim_start_matches('/')))
                })
                .unwrap_or((workdir, word));
            if rest.split('/').any(|name| name == ".claude") {
                sources.push((base.join(rest), ""));
            }
        }
    }
    let folders = ["agents", "commands", "skills"];
    for dir in [config.to_owned(), home.join(".claude"), project] {
        sources.extend(folders.map(|name| (dir.join(name), "/**")));
    }
    sources
}

/// Where Claude Code 2.1.309 keeps a command it runs: in a settings file, in
/// each hook, and in each MCP server. D4 is a partial guard. It covers the four
/// settings files, `.mcp.json`, each `.claude` path these keys name there, the
/// `agents`, `skills` and `commands` folders, and the links to any of them. Not
/// covered (ADE-106): `.claude.json` MCP servers, the app's `--mcp-config`
/// file, plugins, managed settings, scripts that agent or skill files name, and
/// keys a newer CLI adds: re-check this list on each CLI upgrade.
pub(crate) const COMMAND_KEYS: &str = "/command /args /headersHelper /apiKeyHelper \
    /awsAuthRefresh /awsCredentialExport /gcpAuthRefresh /otelHeadersHelper /processWrapper \
    /proxyAuthHelper /fileSuggestion/command /statusLine/command /subagentStatusLine/command";

/// Each command `file` has Claude Code run (`COMMAND_KEYS` in the file, each hook
/// and each MCP server), quotes and braces dropped; none if it does not parse.
fn setting_commands(file: &Path) -> Vec<String> {
    let Some(settings) = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Vec::new();
    };
    let hooks = settings["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|events| events.values().filter_map(Value::as_array).flatten())
        .filter_map(|group| group["hooks"].as_array())
        .flatten();
    let servers = settings["mcpServers"].as_object().into_iter().flatten();
    std::iter::once(&settings)
        .chain(hooks)
        .chain(servers.map(|(_, server)| server))
        .flat_map(|at| COMMAND_KEYS.split(' ').filter_map(|k| at.pointer(k)))
        .flat_map(|v| v.as_array().map_or(std::slice::from_ref(v), Vec::as_slice))
        .filter_map(Value::as_str)
        .map(|command| command.replace(['"', '\'', '{', '}'], ""))
        .collect()
}

/// `path` where its name lives (its own folders resolved) if its last name is
/// a link, every link the walk meets after the first one, and its real path.
/// Past a name that is not there, the rest is spelled as written with `..`
/// folded: Claude Code matches a rule's path as written.
fn link_chain(path: &Path) -> Result<Vec<String>, String> {
    let failed = |why: &dyn std::fmt::Display| {
        format!(
            "Claude Code deny rules: cannot resolve {}: {why}",
            path.display()
        )
    };
    let utf8 = |at: &Path| {
        at.to_str()
            .map(str::to_owned)
            .ok_or_else(|| failed(&"not UTF-8"))
    };
    let names = |at: &Path| -> Vec<OsString> {
        at.components()
            .rev()
            .map(|name| name.as_os_str().to_owned())
            .collect()
    };
    let mut todo = names(&std::path::absolute(path).map_err(|e| failed(&e))?);
    let (mut at, mut chain, mut links) = (PathBuf::new(), Vec::new(), 0);
    while let Some(name) = todo.pop() {
        if name == "." || name == ".." {
            if name == ".." {
                at.pop();
            }
            continue;
        }
        let here = at.join(&name);
        let target = match here.read_link() {
            Ok(target) => target,
            Err(e) if matches!(e.kind(), NotFound | NotADirectory | InvalidInput) => {
                at = here;
                continue;
            }
            Err(e) => return Err(failed(&e)),
        };
        links += 1;
        // The kernel gives up after 40 links too.
        if links > 40 {
            return Err(failed(&"too many links"));
        }
        // srt (macOS) pins `path`'s own folders through `path` as written only up
        // to its first link, one in `/` too. Only root can replace a link in `/`.
        if (todo.is_empty() || links > 1) && at != Path::new("/") {
            chain.push(utf8(&here)?);
        }
        todo.extend(names(&target));
    }
    chain.push(utf8(&at)?);
    Ok(chain)
}
