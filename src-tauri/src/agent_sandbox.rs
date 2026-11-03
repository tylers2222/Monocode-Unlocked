//! The agent sandbox (ADE-41), off unless `<app data>/agent-sandbox-on` exists.
//! Off, a harness child starts as it always did. On, it starts as `srt
//! --settings <app data>/agent-sandbox.json -- <cli> <args>`: writes only to
//! `allowWrite`, network only to `allowedDomains`, reads all but `denyRead`.
//! Every MonoCode data folder is in both deny lists. The user's `extraAllowWrite`
//! and `extraAllowedDomains` are added to the defaults on every spawn, and srt
//! lets `denyWrite` win, so an addition cannot reopen a denied path.
//! A spawn with write rules of its own (ADE-44's hook files, the Space fence,
//! its login's folder) gets a copy with them added,
//! `agent-sandbox-<hash of its content>.json`.
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

/// The folders a spawn's CLI keeps its login and state in.
#[derive(Default, Clone, Copy)]
pub(crate) struct Homes<'a> {
    /// `harness::provider_account_dir`: writable outside the app data only.
    pub(crate) account: Option<&'a Path>,
    /// A Mono's private Codex home in the app data (`codex_mono_store`):
    /// readable and writable, and nothing else in the app data is.
    pub(crate) codex: Option<&'a Path>,
}

/// srt write rules one spawn adds to the shared settings.
#[derive(Default)]
struct SpawnRules {
    allow_write: Vec<String>,
    deny_write: Vec<String>,
    /// Folders in the app data this spawn reads and writes.
    carve: Vec<PathBuf>,
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

/// Whether every agent child must run fenced: the user has more than one Space.
pub(crate) fn fence_on(store: &SessionStore) -> Result<bool, String> {
    Ok(crate::spaces::list(store)?.len() > 1)
}

/// The fence for a child of `session` (`spaces::Child::session`) starting
/// in `workdir`, `None` while the user has one Space. Its own checkout is the
/// one its Space leases around `workdir`, where the spawn hook placed it
/// (`spaces::leases::place`). A child of no session has no Space, so every
/// leased checkout is another's.
pub(crate) fn space_fence(
    store: &SessionStore,
    session: Option<&str>,
    workdir: &Path,
) -> Result<Option<Fence>, String> {
    if !fence_on(store)? {
        return Ok(None);
    }
    let space = session
        .map(|session| {
            let conn = store.lock_conn()?;
            let session = crate::harness::child_session(session);
            crate::spaces::membership::space_for_row(&conn, session, None)
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let space = space.as_ref();
    let leases = leases(store)?;
    let real = std::fs::canonicalize(workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    let key = crate::mono_gate::path_key(&crate::fs::path_to_js(&real));
    let checkout = lease_on(&leases, &key, None)
        .filter(|lease| Some(&lease.space_id) == space)
        .map(|lease| PathBuf::from(&lease.checkout_path));
    // A lease on the shared Giga workspace predates the rule that never takes one.
    let shared = crate::spaces::leases::shared_workspace();
    Ok(Some(Fence {
        git: checkout.as_deref().and_then(git_dir),
        checkout,
        others: leases
            .iter()
            .filter(|lease| Some(&lease.space_id) != space)
            .map(|lease| PathBuf::from(&lease.checkout_path))
            .filter(|other| {
                shared
                    .as_ref()
                    .is_none_or(|shared| !other.starts_with(shared))
            })
            .collect(),
    }))
}

/// The git folder the checkout at `top` commits to: its `.git`, or for a
/// linked worktree the main checkout's (`gitdir:` in its `.git` file, then
/// that folder's `commondir`).
fn git_dir(top: &Path) -> Option<PathBuf> {
    let dot_git = top.join(".git");
    if dot_git.is_dir() {
        return std::fs::canonicalize(dot_git).ok();
    }
    let link = std::fs::read_to_string(dot_git).ok()?;
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
    homes: Homes,
) -> Result<Spawn, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    command_in(&data, program, fence, spawn_deny, homes, find_srt)
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

/// `command` for the app data folder `data`, with `fence`, `spawn_deny` and
/// `homes` added to srt's rules for this spawn only; `find_srt` runs only when
/// the sandbox is on. A fence turns it on without the marker, and a fence
/// without a working srt refuses the spawn, except on Windows, where srt does
/// not run and the spawn starts unfenced.
pub(crate) fn command_in(
    data: &Path,
    program: &str,
    fence: Option<&Fence>,
    spawn_deny: &[String],
    homes: Homes,
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
    // The CLI keeps its config and refreshed login there. A login folder
    // inside the app data stays denied, as the rest of it.
    rules.allow_write.extend(
        homes
            .account
            .filter(|dir| !dir.starts_with(data))
            .map(path_entry),
    );
    rules.carve.extend(homes.codex.map(Path::to_path_buf));
    let settings = write_settings(data, &srt, rules)?;
    let mut cmd = Command::new(srt);
    cmd.arg("--settings").arg(settings).arg("--").arg(program);
    Ok(Spawn { cmd, sandboxed })
}

/// srt's write allows and denies for `fence`. srt lets a deny win, and a
/// linked copy commits into its main checkout's `.git`, so a checkout that
/// holds a folder of the spawn's own is denied around it: each entry beside
/// the way down. Git runs that folder's hooks, and its config's
/// `core.fsmonitor` or `core.hooksPath`, outside srt for every checkout that
/// shares it, and srt guards only the `.git` in the child's own folder, so
/// both stay denied.
fn fence_rules(fence: &Fence) -> Result<SpawnRules, String> {
    let own: Vec<PathBuf> = fence.own().cloned().collect();
    let mut deny = Vec::new();
    for other in &fence.others {
        deny_around(other, &own, &mut deny)?;
    }
    deny.extend(fence.git.as_deref().into_iter().flat_map(git_code));
    Ok(SpawnRules {
        allow_write: own.iter().map(PathBuf::as_path).map(path_entry).collect(),
        deny_write: deny,
        carve: Vec::new(),
    })
}

/// What git in `git` runs outside srt: its hooks, and its config's commands.
fn git_code(git: &Path) -> [String; 2] {
    ["hooks", "config"].map(|name| path_entry(&git.join(name)))
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
    // The app commits in the shared Giga workspace outside srt (giga_workspace.rs).
    deny_write.extend(git_code(
        &crate::giga_workspace::workspace_root(&home).join(".git"),
    ));
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
    default_write.extend(keychain_cache().as_deref().map(path_entry));
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
    if spawn.allow_write.is_empty() && spawn.deny_write.is_empty() && spawn.carve.is_empty() {
        return Ok(path);
    }
    // In a file named by its content, so a spawn starting at the same time
    // cannot rewrite this one's rules before its srt reads them.
    let mut allow: Vec<String> = serde_json::from_value(written["filesystem"]["allowWrite"].take())
        .map_err(|e| e.to_string())?;
    allow.extend(spawn.allow_write);
    let mut deny = [deny_write, spawn.deny_write].concat();
    let mut read: Vec<String> = spawn.carve.iter().map(|dir| path_entry(dir)).collect();
    if !spawn.carve.is_empty() {
        allow.extend(read.iter().cloned());
        carve_out(data, &spawn.carve, &mut deny)?;
    }
    for list in [&mut allow, &mut deny, &mut read] {
        add_resolved(list);
    }
    written["filesystem"]["allowRead"] = json!(read);
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

/// Opens `keep`, folders in the app data folder `data`, to writes. srt lets a
/// deny win, so `deny`'s entries on `data` and on every MonoCode data folder
/// become one on each entry beside the way down to `keep`, and one on each
/// other data folder. srt's `allowRead` reopens reads.
fn carve_out(data: &Path, keep: &[PathBuf], deny: &mut Vec<String>) -> Result<(), String> {
    let instances = data.parent().map(|dir| dir.join(ALL_INSTANCES));
    let mut covering: Vec<String> = std::iter::once(data)
        .chain(instances.as_deref())
        .map(path_entry)
        .collect();
    add_resolved(&mut covering);
    deny.retain(|entry| !covering.contains(entry));
    deny_around(data, keep, deny)?;
    let Some(parent) = data.parent() else {
        return Ok(());
    };
    let entries = std::fs::read_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    for entry in entries {
        let other = entry
            .map_err(|e| format!("{}: {e}", parent.display()))?
            .path();
        let name = other.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with("com.monocode.") && other != data {
            deny.push(path_entry(&other));
        }
    }
    Ok(())
}

/// The Keychain's module cache, in the per-user cache folder under
/// `/var/folders` (`getconf DARWIN_USER_CACHE_DIR`). Without it every
/// `security` call fails with "A Module Directory Service error", so Claude
/// Code cannot read its login and reports it is not logged in.
#[cfg(target_os = "macos")]
fn keychain_cache() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: confstr writes at most `buf.len()` bytes, its NUL included.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_CACHE_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len == 0 || len > buf.len() {
        return None;
    }
    buf.truncate(len - 1);
    Some(PathBuf::from(std::ffi::OsString::from_vec(buf)).join("mds"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_cache() -> Option<PathBuf> {
    None
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
