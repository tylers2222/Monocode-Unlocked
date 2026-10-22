//! The agent sandbox (ADE-41). It is off unless `<app data>/agent-sandbox-on`
//! exists, and off, a harness child starts exactly as it always did. On, the
//! child starts as `srt --settings <app data>/agent-sandbox.json -- <cli>
//! <args>`, under a deny-default Seatbelt profile that every process it starts
//! inherits. The profile keeps the agent out of the app data folder, so it can
//! read neither the database nor this setting, and cannot change either.
//!
//! The user may edit `filesystem.allowWrite` and `network.allowedDomains` in
//! the settings file. The app rewrites every other key before each spawn, and
//! srt lets `denyWrite` win over `allowWrite`, so an edit cannot reopen a
//! denied path.

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

/// The command a harness child starts with.
pub(crate) fn command(app: &AppHandle, program: &str) -> Result<Command, String> {
    let Some(data) = app
        .path()
        .app_data_dir()
        .ok()
        .filter(|data| data.join(ON_MARKER).exists())
    else {
        return Ok(Command::new(program));
    };
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
    let path = data.join(SETTINGS_FILE);
    let saved = match std::fs::read_to_string(&path) {
        Ok(text) => Some(
            serde_json::from_str::<Value>(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let home = crate::dirs_home().ok_or("The agent sandbox needs a home directory.")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(&exe);
    let search_path = crate::harness::gui_search_path();
    let protected = Protected {
        data,
        bundle,
        home: Path::new(&home),
        search_path: std::env::split_paths(&search_path).collect(),
        temp: std::env::temp_dir(),
    };
    let temp = data.join(format!(".{SETTINGS_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, settings(&protected, saved.as_ref()).to_string())
        .and_then(|()| std::fs::rename(&temp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("{}: {e}", path.display())
        })?;
    Ok(path)
}

struct Protected<'a> {
    data: &'a Path,
    bundle: &'a Path,
    home: &'a Path,
    /// The app runs what it finds here outside the sandbox (`--help` probes,
    /// the next spawn), so an agent must not be able to plant a binary there.
    search_path: Vec<PathBuf>,
    temp: PathBuf,
}

fn settings(protected: &Protected, saved: Option<&Value>) -> Value {
    let user_list = |pointer: &str, default: Value| {
        saved
            .and_then(|saved| saved.pointer(pointer))
            .filter(|list| list.is_array())
            .cloned()
            .unwrap_or(default)
    };
    let mut default_write = DEFAULT_WRITE.map(String::from).to_vec();
    default_write.push(path_entry(&protected.temp));
    let startup: Vec<PathBuf> = STARTUP_FILES
        .iter()
        .map(|file| protected.home.join(file))
        .collect();
    let deny_read = std::iter::once(protected.data).chain(startup.iter().map(PathBuf::as_path));
    let deny_write = deny_read.clone().chain([protected.bundle]).chain(
        protected
            .search_path
            .iter()
            .filter(|dir| dir.is_absolute())
            .map(PathBuf::as_path),
    );
    let deny_read: Vec<String> = deny_read.map(path_entry).collect();
    let deny_write: Vec<String> = deny_write.map(path_entry).collect();
    json!({
        "filesystem": {
            "denyRead": deny_read,
            "allowRead": [],
            "allowWrite": user_list("/filesystem/allowWrite", json!(default_write)),
            "denyWrite": deny_write,
        },
        "network": {
            "allowedDomains": user_list("/network/allowedDomains", json!(DEFAULT_DOMAINS)),
            "deniedDomains": [],
            // The app's tool endpoint is loopback TCP on a port picked at launch.
            "allowLocalBinding": true,
        },
    })
}

/// srt rejects a deny entry that ends in a separator, as a PATH dir may.
fn path_entry(path: &Path) -> String {
    path.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}
