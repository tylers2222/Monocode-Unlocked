//! Claude Code deny rules (ADE-44). They are off unless
//! `<app data>/claude-deny-rules-on` exists. Off, the args stay exactly as the
//! front end sent them. On, the `--settings` object gets `permissions.deny` and
//! `sandbox` rules on every MonoCode data folder (`com.monocode.*` beside this
//! app's own, so another instance's database is covered too), and Edit rules
//! on the files Claude Code loads hooks and MCP servers from, which run outside
//! its sandbox. Claude Code's sandbox already keeps Bash off those files.
//!
//! The rules are defense in depth only. The Read rule does not stop a script
//! that opens the file itself, the OS sandbox covers Bash only, and Claude Code
//! runs Bash unsandboxed where the sandbox cannot start (`failIfUnavailable`
//! stays off). Inside D1's srt (ADE-41) Claude Code's sandbox cannot start,
//! and srt is the only Bash control. This module always adds the `sandbox`
//! block; dropping it under srt belongs to the code that wraps the spawn.

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
    args_in(&data, args)
}

/// A marker that cannot be read counts as on: an IO error must not lower
/// protection.
pub(crate) fn args_in(data: &Path, args: Vec<String>) -> Result<Vec<String>, String> {
    if matches!(std::fs::exists(data.join(ON_MARKER)), Ok(false)) {
        return Ok(args);
    }
    with_deny_rules(args, data)
}

fn with_deny_rules(mut args: Vec<String>, data: &Path) -> Result<Vec<String>, String> {
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
    settings.insert(
        "permissions".into(),
        json!({ "deny": [
            format!("Read(/{folders}/**)"),
            format!("Edit(/{folders}/**)"),
            "Edit(//**/.claude/**)",
            "Edit(//**/.claude.json)",
            "Edit(//**/.mcp.json)",
        ] }),
    );
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
