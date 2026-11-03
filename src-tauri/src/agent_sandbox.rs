//! The agent sandbox (ADE-41), off unless `<app data>/agent-sandbox-on` exists.
//! Off, a harness child starts as it always did. On, it starts as `srt
//! --settings <app data>/agent-sandbox.json -- <cli> <args>`: writes only to
//! `allowWrite`, network only to `allowedDomains`, reads all but `denyRead`.
//! Every MonoCode data folder is in both deny lists. The user's `extraAllowWrite`
//! and `extraAllowedDomains` are added to the defaults on every spawn, and srt
//! lets `denyWrite` win, so an addition cannot reopen a denied path.
//! A spawn with write rules of its own (ADE-44's hook files, the Space fence)
//! gets a copy with them added, `agent-sandbox-<hash of its content>.json`.
//!
//! The Space write fence (ADE-177): once a second Space exists the sandbox is
//! on for every spawn, marker or not, and a spawn may write to its own
//! checkout but to no checkout another Space holds. Windows has no srt, so
//! there the fence is not available and spawns start unfenced.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use crate::session_store::SessionStore;
use crate::spaces::leases::{lease_on, leases};

/// The settings format is a research preview, so no other version runs.
const SRT_VERSION: &str = "0.0.81";
const ON_MARKER: &str = "agent-sandbox-on";
const SETTINGS_FILE: &str = "agent-sandbox.json";
/// Every MonoCode build's data folder (`.second`, `.desktop.gigaeval`, dev builds).
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
/// only writes are denied. Inside the repo srt denies only `.git/hooks` and
/// `.git/config`; a linked worktree's `.git` file stays writable (ADE-101).
const OTHERS_CONFIG: [&str; 3] = [".gitconfig", ".claude/settings.json", ".codex/config.toml"];

/// Each harness's own state folder. Under the XDG config folder only OpenCode
/// (`opencode`) and Devin (`devin`) write, so it is not allowed whole.
/// `~/MonoCode` and DigitalOcean's `doctl` (its macOS folder here, its XDG one
/// with gh's) are shared by every Space (ADE-177).
const DEFAULT_WRITE: [&str; 11] = [
    ".",
    "/tmp",
    "~/.omp",
    "~/.claude",
    "~/.claude.json",
    "~/.codex",
    "~/.cache",
    "~/.npm",
    "~/Library/Caches",
    "~/MonoCode",
    "~/Library/Application Support/doctl",
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

/// srt write rules one spawn adds to the shared settings.
#[derive(Default)]
struct SpawnRules {
    allow_write: Vec<String>,
    deny_write: Vec<String>,
}

/// What the Space write fence lets one spawn write, and what it keeps it out of.
#[derive(Debug)]
pub(crate) struct Fence {
    /// The spawn's own checkout, `None` outside every checkout its Space holds.
    checkout: Option<PathBuf>,
    /// The main checkout's `.git`, which a linked copy commits to.
    git: Option<PathBuf>,
    /// Every checkout another Space holds.
    pub(crate) others: Vec<PathBuf>,
}

impl Fence {
    fn own(&self) -> impl Iterator<Item = &PathBuf> {
        self.checkout.iter().chain(&self.git)
    }
}

/// The fence for `session` starting in `workdir`, `None` while the user has
/// one Space. Its own checkout is the one its Space leases around `workdir`,
/// where the spawn hook placed it (`spaces::leases::place`).
pub(crate) fn space_fence(
    store: &SessionStore,
    session: &str,
    workdir: &Path,
) -> Result<Option<Fence>, String> {
    if crate::spaces::list(store)?.len() < 2 {
        return Ok(None);
    }
    let space = {
        let conn = store.lock_conn()?;
        let session = crate::harness::child_session(session);
        crate::spaces::membership::space_for_row(&conn, session, None).map_err(|e| e.to_string())?
    };
    let leases = leases(store)?;
    let real = std::fs::canonicalize(workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    let key = crate::mono_gate::path_key(&crate::fs::path_to_js(&real));
    let checkout = lease_on(&leases, &key, None)
        .filter(|lease| lease.space_id == space)
        .map(|lease| PathBuf::from(&lease.checkout_path));
    Ok(Some(Fence {
        git: checkout.as_deref().and_then(git_common_dir),
        checkout,
        others: leases
            .iter()
            .filter(|lease| lease.space_id != space)
            .map(|lease| PathBuf::from(&lease.checkout_path))
            .collect(),
    }))
}

/// The folder a linked worktree's commits write to: the main checkout's
/// `.git` (`gitdir:` in its `.git` file, then that folder's `commondir`).
fn git_common_dir(top: &Path) -> Option<PathBuf> {
    let link = std::fs::read_to_string(top.join(".git")).ok()?;
    let gitdir = top.join(link.strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .map_or_else(|_| gitdir.clone(), |dir| gitdir.join(dir.trim()));
    std::fs::canonicalize(common).ok()
}

pub(crate) fn command<R: Runtime>(
    app: &AppHandle<R>,
    program: &str,
    fence: Option<&Fence>,
    spawn_deny: &[String],
) -> Result<Spawn, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    command_in(&data, program, fence, spawn_deny, find_srt)
}

#[cfg(test)]
thread_local! {
    /// The srt spawns on this thread find in place of the search PATH's,
    /// `Some(None)` for none: a scenario with two Spaces needs one to spawn.
    pub(crate) static SRT_STAND_IN: std::cell::RefCell<Option<Option<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

fn find_srt() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(srt) = SRT_STAND_IN.with(|srt| srt.borrow().clone()) {
            return srt;
        }
    }
    crate::harness::resolve_gui_binary("srt")
}

/// `command` for the app data folder `data`, with `fence` and `spawn_deny`
/// added to srt's write rules for this spawn only; `find_srt` runs only when
/// the sandbox is on. A fence turns it on without the marker, and a fence
/// without a working srt refuses the spawn, except on Windows, where srt does
/// not run and the spawn starts unfenced.
pub(crate) fn command_in(
    data: &Path,
    program: &str,
    fence: Option<&Fence>,
    spawn_deny: &[String],
    find_srt: impl FnOnce() -> Option<PathBuf>,
) -> Result<Spawn, String> {
    let marker =
        std::fs::exists(data.join(ON_MARKER)).map_err(|e| format!("{}: {e}", data.display()))?;
    let fenced = fence.is_some() && !cfg!(windows);
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        if fence.is_some() {
            ONCE.call_once(|| {
                eprintln!("The write fence between Spaces is unavailable on Windows: agents start unfenced.");
            });
        }
    }
    let sandboxed = marker || fenced;
    if !sandboxed {
        let cmd = Command::new(program);
        return Ok(Spawn { cmd, sandboxed });
    }
    let why = if marker {
        ""
    } else {
        " to fence each Space's writes"
    };
    let srt = pinned_srt(find_srt(), why)?;
    let mut rules = fence.map(fence_rules).transpose()?.unwrap_or_default();
    rules.deny_write.extend_from_slice(spawn_deny);
    let settings = write_settings(data, &srt, rules)?;
    let mut cmd = Command::new(srt);
    cmd.arg("--settings").arg(settings).arg("--").arg(program);
    Ok(Spawn { cmd, sandboxed })
}

/// srt's write allows and denies for `fence`. srt lets a deny win, and a
/// linked copy commits into its main checkout's `.git`, so a checkout that
/// holds a folder of the spawn's own is denied around it: each entry beside
/// the way down. The hooks there run in that other checkout, so they stay denied.
fn fence_rules(fence: &Fence) -> Result<SpawnRules, String> {
    let own: Vec<PathBuf> = fence.own().cloned().collect();
    let mut deny = Vec::new();
    for other in &fence.others {
        deny_around(other, &own, &mut deny)?;
    }
    let shared_git = fence
        .git
        .as_ref()
        .filter(|git| fence.others.iter().any(|other| git.starts_with(other)));
    deny.extend(shared_git.map(|git| path_entry(&git.join("hooks"))));
    Ok(SpawnRules {
        allow_write: own.iter().map(PathBuf::as_path).map(path_entry).collect(),
        deny_write: deny,
    })
}

/// Denies `folder`, or, when it holds a folder in `keep`, each entry in it
/// that is not on the way down to one.
fn deny_around(folder: &Path, keep: &[PathBuf], deny: &mut Vec<String>) -> Result<(), String> {
    let inside: Vec<&PathBuf> = keep
        .iter()
        .filter(|kept| kept.starts_with(folder))
        .collect();
    if inside.is_empty() {
        deny.push(path_entry(folder));
        return Ok(());
    }
    if inside.iter().any(|kept| kept.as_path() == folder) {
        return Ok(());
    }
    let entries = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", folder.display()))?;
        deny_around(&entry.path(), keep, deny)?;
    }
    Ok(())
}

fn pinned_srt(srt: Option<PathBuf>, why: &str) -> Result<PathBuf, String> {
    let install = format!("npm install -g @anthropic-ai/sandbox-runtime@{SRT_VERSION}");
    let srt = srt.ok_or_else(|| {
        format!("The agent sandbox is on{why}, but srt is not installed. Run `{install}`.")
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

/// The settings file for one spawn: the shared one, or a copy with `spawn`'s
/// rules added.
fn write_settings(data: &Path, srt: &Path, spawn: SpawnRules) -> Result<PathBuf, String> {
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
    default_write
        .extend(["opencode", "devin", "gh", "doctl"].map(|tool| path_entry(&config.join(tool))));
    for list in [&mut deny_read, &mut deny_write, &mut default_write] {
        add_resolved(list);
    }

    let path = data.join(SETTINGS_FILE);
    let mut written = Value::Null;
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
        written = saved.clone();
        Ok(())
    })
    .map_err(|e| format!("{}: {e}", path.display()))?;
    if spawn.allow_write.is_empty() && spawn.deny_write.is_empty() {
        return Ok(path);
    }
    // In a file named by its content, so a spawn starting at the same time
    // cannot rewrite this one's rules before its srt reads them.
    let mut allow: Vec<String> = serde_json::from_value(written["filesystem"]["allowWrite"].take())
        .map_err(|e| e.to_string())?;
    allow.extend(spawn.allow_write);
    let mut deny = [deny_write, spawn.deny_write].concat();
    for list in [&mut allow, &mut deny] {
        add_resolved(list);
    }
    written["filesystem"]["allowWrite"] = json!(allow);
    written["filesystem"]["denyWrite"] = json!(deny);
    let mut hash = std::hash::DefaultHasher::new();
    written.to_string().hash(&mut hash);
    let path = data.join(format!("agent-sandbox-{:016x}.json", hash.finish()));
    crate::mcp::write_json_config(&path, |saved| {
        *saved = written;
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
    match std::fs::canonicalize(path) {
        Ok(real) => Some(real),
        Err(_) => Some(resolved(path.parent()?)?.join(path.file_name()?)),
    }
}
