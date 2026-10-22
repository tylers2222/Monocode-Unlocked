//! Claude Code deny rules (ADE-44), off unless `<app data>/claude-deny-rules-on`
//! exists. Off, the args stay as the front end sent them. On, `--settings` gets
//! `permissions.deny` and `sandbox` rules on every `com.monocode.*` data folder
//! beside this app's own, and Edit rules on the files Claude Code loads hooks
//! and MCP servers from (`.claude`, `.claude.json`, `.mcp.json`,
//! `CLAUDE_CONFIG_DIR`), which run outside its sandbox.
//!
//! Defense in depth only: the Read rule does not stop a script that opens the
//! file itself, the sandbox covers Bash only, and Bash runs unsandboxed where
//! the sandbox cannot start (`failIfUnavailable` stays off). Inside D1's srt
//! (ADE-41) the sandbox cannot start and srt is the only Bash control; dropping
//! the block there belongs to the code that wraps the spawn.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

const ON_MARKER: &str = "claude-deny-rules-on";

pub(crate) fn args(app: &AppHandle, args: Vec<String>) -> Result<Vec<String>, String> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Claude Code deny rules: no app data folder: {e}"))?;
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
    args_in(&data, config_dir.as_deref(), args)
}

/// A marker that cannot be read counts as on: an IO error must not lower
/// protection. `config_dir` is the `CLAUDE_CONFIG_DIR` the child inherits.
pub(crate) fn args_in(
    data: &Path,
    config_dir: Option<&OsStr>,
    mut args: Vec<String>,
) -> Result<Vec<String>, String> {
    if matches!(std::fs::exists(data.join(ON_MARKER)), Ok(false)) {
        return Ok(args);
    }
    let config = match config_dir.map(OsStr::to_str) {
        None => None,
        Some(Some(dir)) if dir.starts_with('/') => Some(format!("Edit(/{dir}/**)")),
        Some(_) => return Err("Claude Code deny rules: CLAUDE_CONFIG_DIR is not absolute.".into()),
    };
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
