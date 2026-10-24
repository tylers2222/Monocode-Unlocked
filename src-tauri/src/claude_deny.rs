//! Claude Code deny rules (ADE-44), off unless `<app data>/claude-deny-rules-on`
//! exists. Off, the args stay as the front end sent them. On, `--settings` gets
//! `permissions.deny` and `sandbox` rules on every `com.monocode.*` data folder
//! beside this app's own, and Edit rules on the files Claude Code loads hooks
//! and MCP servers from (`.claude`, `.claude.json`, `.mcp.json`, the config
//! folder of the spawn's account, `CLAUDE_CONFIG_DIR` or `$HOME/.claude`, each
//! file under a `.claude` folder that a hook, helper key or MCP server command
//! names, and each link on the way to any of them that is a symlink), which run
//! outside its sandbox.
//!
//! Defense in depth only: the Read rule does not stop a script that opens the
//! file itself, the sandbox covers Bash only, and Bash runs unsandboxed where
//! the sandbox cannot start (`failIfUnavailable` stays off). Inside D1's srt
//! (ADE-41) the sandbox cannot start, and with the block present the session's
//! first Bash call fails trying, so a spawn that `agent_sandbox::command` wraps
//! in srt gets no `sandbox` block, and srt denies writes to the hook files
//! instead (`srt_deny`). Not to `.claude.json`: Claude Code writes it, so a
//! script inside srt can still add an MCP server there.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
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

/// The hook files Claude Code never writes itself, each with every link on the
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
    for file in hook_files(&config, home, workdir) {
        deny.extend(link_chain(&file)?);
    }
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

/// Edit rules on the config folder, and on each link and the real path behind
/// every hook or MCP source that is a symlink: Claude Code loads the file
/// through them, and a rule on a link covers neither its target nor the links
/// after it.
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
    // The rule above misses the real spelling when a folder link leads there.
    let resolved = link_chain(&config)?.swap_remove(0);
    if resolved != text {
        rules.push(format!("Edit(/{resolved}/**)"));
    }
    let files = hook_files(&config, home, workdir)
        .into_iter()
        .chain([config.join(".claude.json"), home.join(".claude.json")]);
    let folders = [config, home.join(".claude"), workdir.join(".claude")];
    let sources = files
        .map(|file| (file, ""))
        .chain(folders.map(|folder| (folder, "/**")));
    for (path, tail) in sources {
        if let [_, links @ .., real] = link_chain(&path)?.as_slice() {
            let real = format!("Edit(/{real}{tail})");
            for rule in links
                .iter()
                .map(|link| format!("Edit(/{link})"))
                .chain([real])
            {
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
        }
    }
    Ok(rules)
}

/// The settings and MCP files Claude Code loads hooks and servers from and
/// never writes itself, and each path through a `.claude` folder that a
/// command in those files names (`$CLAUDE_PROJECT_DIR`, `$HOME` and `~`
/// expanded, a relative path taken from `workdir`, where hooks run).
fn hook_files(config: &Path, home: &Path, workdir: &Path) -> Vec<PathBuf> {
    let project = workdir.join(".claude");
    let mut files = vec![
        config.join("settings.json"),
        home.join(".claude/settings.json"),
        project.join("settings.json"),
        project.join("settings.local.json"),
        workdir.join(".mcp.json"),
    ];
    let vars = [
        ("$CLAUDE_PROJECT_DIR", workdir),
        ("$HOME", home),
        ("~", home),
    ];
    for command in files
        .iter()
        .flat_map(|file| setting_commands(file))
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
                files.push(base.join(rest));
            }
        }
    }
    files
}

/// Settings keys besides `hooks` that hold a command Claude Code runs.
const COMMAND_KEYS: [&str; 5] = [
    "/apiKeyHelper",
    "/awsAuthRefresh",
    "/awsCredentialExport",
    "/otelHeadersHelper",
    "/statusLine/command",
];

/// Each command `file` has Claude Code run (hooks, the `COMMAND_KEYS`, and
/// each MCP server's command and args), quotes and braces dropped. A file
/// Claude Code cannot read or parse holds no command it runs.
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
        .flatten()
        .map(|hook| &hook["command"]);
    let servers = settings["mcpServers"]
        .as_object()
        .into_iter()
        .flat_map(|servers| servers.values())
        .flat_map(|server| {
            let args = server["args"].as_array().into_iter().flatten();
            args.chain([&server["command"]])
        });
    hooks
        .chain(COMMAND_KEYS.iter().filter_map(|key| settings.pointer(key)))
        .chain(servers)
        .filter_map(Value::as_str)
        .map(|command| command.replace(['"', '\'', '{', '}'], ""))
        .collect()
}

/// `path` where its name lives (its folders resolved), then each link it
/// resolves through, a folder on the way included, ending at its real path.
/// Past a name that is not there, the rest is spelled as written with `..`
/// folded: Claude Code matches a rule's path as written.
fn link_chain(path: &Path) -> Result<Vec<String>, String> {
    let failed = |why: &dyn std::fmt::Display| {
        format!(
            "Claude Code deny rules: cannot resolve {}: {why}",
            path.display()
        )
    };
    let missing =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory);
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
            Err(e) if missing(&e) || e.kind() == ErrorKind::InvalidInput => {
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
        // The folders of `path` are followed silently: they are not on the way
        // to a link target. Only root can replace a link in `/`.
        if (todo.is_empty() || !chain.is_empty()) && at != Path::new("/") {
            chain.push(utf8(&here)?);
        }
        todo.extend(names(&target));
    }
    chain.push(utf8(&at)?);
    Ok(chain)
}
