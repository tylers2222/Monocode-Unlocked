//! The agent sandbox (ADE-41). It is off unless `<app data>/agent-sandbox-on`
//! exists, and off, a harness child starts exactly as it always did. On, the
//! child starts as `srt --settings <app data>/agent-sandbox.json -- <cli>
//! <args>`. srt runs it, and every process it starts, under a Seatbelt profile
//! that allows writes only to `allowWrite`, network only to `allowedDomains`,
//! and reads everywhere except `denyRead`. The app data folder is in both deny
//! lists, so the agent can neither read the database nor change this setting.
//!
//! The user may edit `filesystem.allowWrite` and `network.allowedDomains` in
//! the settings file. The app rewrites every other key before each spawn, and
//! srt lets `denyWrite` win over `allowWrite`, so an edit cannot reopen a
//! denied path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// The settings format is a research preview, so no other version runs.
const SRT_VERSION: &str = "0.0.81";
const ON_MARKER: &str = "agent-sandbox-on";
const SETTINGS_FILE: &str = "agent-sandbox.json";

/// Under HOME. A write to any of these runs code outside the sandbox later.
const STARTUP_FILES: [&str; 11] = [
    "Library/LaunchAgents",
    ".zshenv",
    ".zprofile",
    ".zshrc",
    ".zlogin",
    ".zlogout",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".config/fish",
];

const DEFAULT_WRITE: [&str; 10] = [
    ".",
    "/tmp",
    "~/.omp",
    "~/.claude",
    "~/.claude.json",
    "~/.codex",
    "~/.cache",
    "~/.npm",
    "~/.config",
    "~/Library/Caches",
];

const DEFAULT_DOMAINS: [&str; 16] = [
    "*.anthropic.com",
    "claude.ai",
    "*.openai.com",
    "chatgpt.com",
    "*.chatgpt.com",
    "generativelanguage.googleapis.com",
    "openrouter.ai",
    "api.x.ai",
    "ai-gateway.vercel.sh",
    "github.com",
    "*.github.com",
    "*.githubusercontent.com",
    "registry.npmjs.org",
    "*.npmjs.org",
    "crates.io",
    "*.crates.io",
];

pub(crate) fn command(app: &AppHandle, program: &str) -> Result<Command, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let on =
        std::fs::exists(data.join(ON_MARKER)).map_err(|e| format!("{}: {e}", data.display()))?;
    if !on {
        return Ok(Command::new(program));
    }
    let srt = pinned_srt()?;
    let settings = write_settings(&data)?;
    let mut cmd = Command::new(srt);
    cmd.arg("--settings").arg(settings).arg("--").arg(program);
    Ok(cmd)
}

fn pinned_srt() -> Result<PathBuf, String> {
    let install = format!("npm install -g @anthropic-ai/sandbox-runtime@{SRT_VERSION}");
    let srt = crate::harness::resolve_gui_binary("srt").ok_or_else(|| {
        format!("The agent sandbox is on, but srt is not installed. Run `{install}`.")
    })?;
    let output = crate::harness::exec_output(
        &srt.to_string_lossy(),
        &["--version".to_string()],
        None,
        Duration::from_secs(15),
    )?;
    let found = String::from_utf8_lossy(&output.stdout);
    if found.trim() != SRT_VERSION {
        return Err(format!(
            "The agent sandbox needs srt {SRT_VERSION}, but {} is {}. Run `{install}`.",
            srt.display(),
            found.trim()
        ));
    }
    Ok(srt)
}

fn write_settings(data: &Path) -> Result<PathBuf, String> {
    let home =
        PathBuf::from(crate::dirs_home().ok_or("The agent sandbox needs a home directory.")?);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(&exe);
    let startup: Vec<PathBuf> = STARTUP_FILES.iter().map(|file| home.join(file)).collect();
    let deny_read: Vec<String> = std::iter::once(data)
        .chain(startup.iter().map(PathBuf::as_path))
        .map(path_entry)
        .collect();
    let mut deny_write = deny_read.clone();
    deny_write.push(path_entry(bundle));
    // The app runs what it finds on its search PATH outside the sandbox
    // (`--help` probes, the next spawn), so an agent must not plant a binary there.
    deny_write.extend(
        std::env::split_paths(&crate::harness::gui_search_path())
            .filter(|dir| dir.is_absolute())
            .map(|dir| path_entry(&dir)),
    );
    let mut seen = HashSet::new();
    deny_write.retain(|entry| seen.insert(entry.clone()));
    let mut default_write = DEFAULT_WRITE.map(String::from).to_vec();
    default_write.push(path_entry(&std::env::temp_dir()));

    let path = data.join(SETTINGS_FILE);
    crate::mcp::write_json_config(&path, |saved| {
        let allow_write = user_list(saved, "/filesystem/allowWrite", json!(default_write))?;
        let domains = user_list(saved, "/network/allowedDomains", json!(DEFAULT_DOMAINS))?;
        *saved = json!({
            "filesystem": {
                "denyRead": deny_read,
                "allowRead": [],
                "allowWrite": allow_write,
                "denyWrite": deny_write,
            },
            "network": {
                "allowedDomains": domains,
                "deniedDomains": [],
                // The app's tool endpoint is loopback TCP on a port picked at launch.
                "allowLocalBinding": true,
            },
        });
        Ok(())
    })
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn user_list(saved: &Value, pointer: &str, default: Value) -> Result<Value, String> {
    match saved.pointer(pointer) {
        None => Ok(default),
        Some(list) if list.is_array() => Ok(list.clone()),
        Some(_) => Err(format!("{pointer} must be a list")),
    }
}

/// srt rejects a deny entry that ends in a separator, as a PATH dir may.
fn path_entry(path: &Path) -> String {
    path.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}
