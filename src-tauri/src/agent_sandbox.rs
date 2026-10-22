//! The agent sandbox (ADE-41). It is off unless `<app data>/agent-sandbox-on`
//! exists, and off, a harness child starts exactly as it always did. On, the
//! child starts as `srt --settings <app data>/agent-sandbox.json -- <cli>
//! <args>`. srt runs it, and every process it starts, under a Seatbelt profile
//! that allows writes only to `allowWrite`, network only to `allowedDomains`,
//! and reads everywhere except `denyRead`. Every MonoCode data folder is in
//! both deny lists, so the agent can neither read a database nor change this setting.
//!
//! The user adds paths and domains under `extraAllowWrite` and
//! `extraAllowedDomains` in the settings file. The app rewrites every other key
//! from its defaults before each spawn, so a later fix to the defaults reaches
//! every machine. srt lets `denyWrite` win over `allowWrite`, so an addition
//! cannot reopen a denied path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

/// The settings format is a research preview, so no other version runs.
const SRT_VERSION: &str = "0.0.81";
const ON_MARKER: &str = "agent-sandbox-on";
const SETTINGS_FILE: &str = "agent-sandbox.json";
/// Every MonoCode build's data folder sits next to this one (`com.monocode.desktop`,
/// `.second`, `.desktop.gigaeval`, dev builds), and each holds a database.
const ALL_INSTANCES: &str = "com.monocode.*";

/// Under HOME. A write to any of these runs code outside the sandbox later,
/// and no agent needs to read them: login items and shell startup files.
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

/// Under HOME. Config that programs outside the sandbox load later: git's
/// global config (with `<XDG config>/git`; the app's own git calls honour
/// `core.fsmonitor` and `core.hooksPath`), Claude Code's user hooks and Codex's
/// `notify` command. The agent's own git, Claude Code and Codex read them, so
/// only writes are denied.
const OTHERS_CONFIG: [&str; 3] = [".gitconfig", ".claude/settings.json", ".codex/config.toml"];

/// Each harness's own state folder. Under the XDG config folder only OpenCode
/// (`opencode`) and Devin (`devin`) write, so it is not allowed whole.
const DEFAULT_WRITE: [&str; 9] = [
    ".",
    "/tmp",
    "~/.omp",
    "~/.claude",
    "~/.claude.json",
    "~/.codex",
    "~/.cache",
    "~/.npm",
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

/// The command a harness child starts with, and whether srt runs it.
pub(crate) struct Spawn {
    pub(crate) cmd: Command,
    pub(crate) sandboxed: bool,
}

pub(crate) fn command<R: Runtime>(app: &AppHandle<R>, program: &str) -> Result<Spawn, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    command_in(&data, program, || crate::harness::resolve_gui_binary("srt"))
}

/// `command` for the app data folder `data`; `find_srt` runs only when the
/// sandbox is on.
pub(crate) fn command_in(
    data: &Path,
    program: &str,
    find_srt: impl FnOnce() -> Option<PathBuf>,
) -> Result<Spawn, String> {
    let sandboxed =
        std::fs::exists(data.join(ON_MARKER)).map_err(|e| format!("{}: {e}", data.display()))?;
    if !sandboxed {
        let cmd = Command::new(program);
        return Ok(Spawn { cmd, sandboxed });
    }
    let srt = pinned_srt(find_srt())?;
    let settings = write_settings(data, &srt)?;
    let mut cmd = Command::new(srt);
    cmd.arg("--settings").arg(settings).arg("--").arg(program);
    Ok(Spawn { cmd, sandboxed })
}

fn pinned_srt(srt: Option<PathBuf>) -> Result<PathBuf, String> {
    let install = format!("npm install -g @anthropic-ai/sandbox-runtime@{SRT_VERSION}");
    let srt = srt.ok_or_else(|| {
        format!("The agent sandbox is on, but srt is not installed. Run `{install}`.")
    })?;
    let output = crate::harness::exec_output(
        &srt.to_string_lossy(),
        &["--version".to_string()],
        None,
        Duration::from_secs(15),
    )?;
    let needs = format!(
        "The agent sandbox needs srt {SRT_VERSION}, but {}",
        srt.display()
    );
    match String::from_utf8_lossy(&output.stdout).trim() {
        SRT_VERSION => Ok(srt),
        "" => Err(format!(
            "{needs} did not answer `--version` ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        found => Err(format!("{needs} is {found}. Run `{install}`.")),
    }
}

/// srt itself runs outside the sandbox, so the folder its code and modules
/// load from must not be writable from inside it.
fn srt_modules(srt: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(srt).ok()?;
    let modules = real
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "node_modules"));
    modules.or_else(|| real.parent()).map(Path::to_path_buf)
}

fn write_settings(data: &Path, srt: &Path) -> Result<PathBuf, String> {
    let home =
        PathBuf::from(crate::dirs_home().ok_or("The agent sandbox needs a home directory.")?);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(&exe);
    let startup: Vec<PathBuf> = STARTUP_FILES.iter().map(|file| home.join(file)).collect();
    let instances = data.parent().map(|dir| dir.join(ALL_INSTANCES));
    let mut deny_read: Vec<String> = std::iter::once(data)
        .chain(instances.as_deref())
        .chain(startup.iter().map(PathBuf::as_path))
        .map(path_entry)
        .collect();
    let mut deny_write = deny_read.clone();
    // `$XDG_CONFIG_HOME`, as git, OpenCode and Devin find it (devin_config.rs).
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from);
    deny_write.extend(
        OTHERS_CONFIG
            .iter()
            .map(|file| path_entry(&home.join(file))),
    );
    deny_write.push(path_entry(&config.join("git")));
    deny_write.push(path_entry(bundle));
    deny_write.extend(srt_modules(srt).as_deref().map(path_entry));
    // The app runs what it finds on its search PATH outside the sandbox
    // (`--help` probes, the next spawn), so an agent must not plant a binary there.
    deny_write.extend(
        std::env::split_paths(&crate::harness::gui_search_path())
            .filter(|dir| dir.is_absolute())
            .map(|dir| path_entry(&dir)),
    );
    let mut default_write = DEFAULT_WRITE.map(String::from).to_vec();
    default_write.push(path_entry(&std::env::temp_dir()));
    default_write.extend(["opencode", "devin"].map(|tool| path_entry(&config.join(tool))));
    for list in [&mut deny_read, &mut deny_write, &mut default_write] {
        add_resolved(list);
    }

    let path = data.join(SETTINGS_FILE);
    crate::mcp::write_json_config(&path, |saved| {
        let extra_write = user_list(saved, "extraAllowWrite")?;
        let extra_domains = user_list(saved, "extraAllowedDomains")?;
        let allow_write = [default_write.as_slice(), &extra_write].concat();
        let domains = [DEFAULT_DOMAINS.map(String::from).as_slice(), &extra_domains].concat();
        *saved = json!({
            "extraAllowWrite": extra_write,
            "extraAllowedDomains": extra_domains,
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

/// The user's additions under `key`. srt refuses the whole file for one entry
/// that is not a string, so the app refuses it here with the key named.
fn user_list(saved: &Value, key: &str) -> Result<Vec<String>, String> {
    match saved.get(key) {
        None => Ok(Vec::new()),
        Some(list) => serde_json::from_value(list.clone())
            .map_err(|_| format!("{key} must be a list of strings")),
    }
}

/// srt rejects a deny entry that ends in a separator, as a PATH dir may.
fn path_entry(path: &Path) -> String {
    path.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// srt matches a path as the kernel sees it, and resolves only paths that
/// exist: a deny of a missing file under a link (`/tmp` is `/private/tmp` on
/// macOS) would never match. So each absolute entry also gets its resolved
/// form, and the list loses its duplicates.
fn add_resolved(list: &mut Vec<String>) {
    let real: Vec<String> = list
        .iter()
        .map(Path::new)
        .filter(|entry| entry.is_absolute())
        .filter_map(resolved)
        .map(|real| path_entry(&real))
        .collect();
    list.extend(real);
    let mut seen = HashSet::new();
    list.retain(|entry| seen.insert(entry.clone()));
}

/// `path` with its deepest existing ancestor resolved.
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut dir = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(dir) {
            return Some(
                missing
                    .iter()
                    .rev()
                    .fold(real, |real, part| real.join(part)),
            );
        }
        missing.push(dir.file_name()?);
        dir = dir.parent()?;
    }
}
