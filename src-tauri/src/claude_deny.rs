//! Claude Code deny rules (ADE-44). They are off unless
//! `<app data>/claude-deny-rules-on` exists. Off, the args stay exactly as the
//! front end sent them. On, the `--settings` object gets `permissions.deny` and
//! `sandbox` rules on the app data folder. The rules are defense in depth only.
//! The Read rule does not stop a script that opens the file itself, the OS
//! sandbox covers Bash only, and Claude Code runs Bash unsandboxed where the
//! sandbox cannot start (`failIfUnavailable` stays off).
//!
//! With the D1 sandbox on (`agent-sandbox-on`, ADE-41), only `permissions.deny`
//! is added. Inside srt, Claude Code's own sandbox fails to start ("Failed to
//! initialize sandbox"), and srt already denies the folder to every process.

use std::path::Path;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

const ON_MARKER: &str = "claude-deny-rules-on";
const D1_MARKER: &str = "agent-sandbox-on";

pub(crate) fn args(app: &AppHandle, args: Vec<String>) -> Result<Vec<String>, String> {
    let Some(data) = app
        .path()
        .app_data_dir()
        .ok()
        .filter(|data| data.join(ON_MARKER).exists())
    else {
        return Ok(args);
    };
    with_deny_rules(args, &data)
}

fn with_deny_rules(mut args: Vec<String>, data: &Path) -> Result<Vec<String>, String> {
    let in_d1_sandbox = data.join(D1_MARKER).exists();
    let data = data
        .to_str()
        .ok_or("Claude Code deny rules: the app data path is not UTF-8.")?;
    if !data.starts_with('/') {
        return Err("Claude Code deny rules: they need a macOS or Linux app data path. Remove claude-deny-rules-on from the app data folder to start Claude Code.".into());
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
    // Read and Edit rules mark an absolute path with `//`. Sandbox paths are plain.
    let pattern = format!("/{data}/**");
    settings.insert(
        "permissions".into(),
        json!({ "deny": [format!("Read({pattern})"), format!("Edit({pattern})")] }),
    );
    if !in_d1_sandbox {
        settings.insert(
            "sandbox".into(),
            json!({
                "enabled": true,
                "allowUnsandboxedCommands": false,
                "filesystem": { "denyRead": [data], "denyWrite": [data] },
            }),
        );
    }
    args[at] = Value::Object(settings).to_string();
    Ok(args)
}
