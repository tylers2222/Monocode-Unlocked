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
//! stays off). Inside D1's srt (ADE-41) Claude Code's sandbox cannot start, so
//! dropping the `sandbox` block there is decided where the spawn is wrapped.

use std::path::Path;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

const ON_MARKER: &str = "claude-deny-rules-on";
const CLAUDE_CONFIG: [&str; 3] = [
    "Edit(//**/.claude/**)",
    "Edit(//**/.claude.json)",
    "Edit(//**/.mcp.json)",
];

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
    let (Some(parent), Some(name)) = (data.parent().and_then(Path::to_str), data.file_name())
    else {
        return Err("Claude Code deny rules: the app data path is not UTF-8.".into());
    };
    if !parent.starts_with('/') {
        return Err("Claude Code deny rules: they need a macOS or Linux app data path. Remove claude-deny-rules-on from the app data folder to start Claude Code.".into());
    }
    if !name.to_string_lossy().starts_with("com.monocode.") {
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
    ];
    deny.extend(CLAUDE_CONFIG.map(String::from));
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
