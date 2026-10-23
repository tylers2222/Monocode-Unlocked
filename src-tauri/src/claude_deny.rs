//! Claude Code deny rules (ADE-44), off unless `<app data>/claude-deny-rules-on`
//! exists. Off, the args stay as the front end sent them. On, `--settings` gets
//! `permissions.deny` and `sandbox` rules on every `com.monocode.*` data folder
//! beside this app's own, and Edit rules on the files Claude Code loads hooks
//! and MCP servers from (`.claude`, `.claude.json`, `.mcp.json`, the config
//! folder `CLAUDE_CONFIG_DIR` or `$HOME/.claude`, and the real path behind any
//! of them that is a symlink), which run outside its sandbox.
//!
//! Defense in depth only: the Read rule does not stop a script that opens the
//! file itself, the sandbox covers Bash only, and Bash runs unsandboxed where
//! the sandbox cannot start (`failIfUnavailable` stays off). Inside D1's srt
//! (ADE-41) the sandbox cannot start and srt is the only Bash control; dropping
//! the block there belongs to the code that wraps the spawn.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

const ON_MARKER: &str = "claude-deny-rules-on";

pub(crate) fn args<R: Runtime>(
    app: &AppHandle<R>,
    workdir: &Path,
    args: Vec<String>,
) -> Result<Vec<String>, String> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Claude Code deny rules: no app data folder: {e}"))?;
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
    // The child's HOME, as `apply_gui_env` sets it.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| crate::passwd_identity().map(|id| id.home.into()))
        .unwrap_or_default();
    args_in(&data, config_dir.as_deref(), &home, workdir, args)
}

/// A marker that cannot be read counts as on: an IO error must not lower
/// protection. `config_dir` is the `CLAUDE_CONFIG_DIR` the child inherits,
/// `home` its HOME and `workdir` its working folder.
pub(crate) fn args_in(
    data: &Path,
    config_dir: Option<&OsStr>,
    home: &Path,
    workdir: &Path,
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
    settings.insert(
        "sandbox".into(),
        json!({
            "enabled": true,
            "allowUnsandboxedCommands": false,
            "filesystem": { "denyRead": [folders], "denyWrite": [folders] },
        }),
    );
    args[at] = Value::Object(settings).to_string();
    Ok(args)
}

/// Edit rules on the config folder, and on the real path behind each hook or
/// MCP source that is a symlink: Claude Code loads the file through the link,
/// and neither a glob nor a literal rule on the link path covers its target.
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
    let (user, project) = (home.join(".claude"), workdir.join(".claude"));
    let sources = [
        (config.join("settings.json"), ""),
        (config.join(".claude.json"), ""),
        (user.join("settings.json"), ""),
        (home.join(".claude.json"), ""),
        (project.join("settings.json"), ""),
        (project.join("settings.local.json"), ""),
        (workdir.join(".mcp.json"), ""),
        (config, "/**"),
        (user, "/**"),
        (project, "/**"),
    ];
    for (path, tail) in sources {
        if let Some(real) = link_target(&path)? {
            let rule = format!("Edit(/{real}{tail})");
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    }
    Ok(rules)
}

fn link_target(path: &Path) -> Result<Option<String>, String> {
    let failed = |why: String| {
        format!(
            "Claude Code deny rules: cannot resolve {}: {why}",
            path.display()
        )
    };
    match path.symlink_metadata() {
        Ok(meta) if meta.is_symlink() => {
            let real = path.canonicalize().map_err(|e| failed(e.to_string()))?;
            real.to_str()
                .map(|real| Some(real.to_owned()))
                .ok_or_else(|| failed("not UTF-8".into()))
        }
        Ok(_) => Ok(None),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(e) => Err(failed(e.to_string())),
    }
}
