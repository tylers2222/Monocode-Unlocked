//! The agent sandbox setting (ADE-41). The first scenario drives
//! `agent_sandbox::command_in`, the body of the command `harness_spawn` starts,
//! on a real app data folder and a stand-in srt that answers `--version`. The
//! second calls the `harness_spawn` command itself on Tauri's mock runtime with
//! stand-in CLIs. `scripts/scenarios/app.sh d1` runs the real srt in the real app.

#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::Manager;

use crate::agent_sandbox::{command_in, Spawn};

struct Folder(PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn folder() -> Folder {
    let dir = std::env::temp_dir().join(format!("ade41-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Folder(dir)
}

pub(super) fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

/// An executable that prints `version` for `--version`, as srt does.
fn fake_srt(dir: &Path, version: &str) -> PathBuf {
    script(
        &dir.join(format!("srt-{version}")),
        &format!("echo {version}"),
    )
}

fn argv(cmd: &std::process::Command) -> (String, Vec<String>) {
    let text = |part: &OsStr| part.to_string_lossy().into_owned();
    (text(cmd.get_program()), cmd.get_args().map(text).collect())
}

fn strings(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect()
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// One app data folder and the srt stand-ins, for the whole scenario. The
/// folder is reached through a link, `<root>/linked -> <root>/real`, as
/// macOS reaches `/tmp` through `/private/tmp`, so the resolved path differs
/// from the given one on every OS.
struct Rig {
    root: Folder,
    tools: Folder,
    data: PathBuf,
    settings: PathBuf,
    pinned: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let root = folder();
        std::fs::create_dir_all(root.0.join("real/com.monocode.test")).unwrap();
        std::os::unix::fs::symlink(root.0.join("real"), root.0.join("linked")).unwrap();
        let data = root.0.join("linked/com.monocode.test");
        let tools = folder();
        let settings = data.join("agent-sandbox.json");
        let pinned = fake_srt(&tools.0, "0.0.81");
        Self {
            root,
            tools,
            data,
            settings,
            pinned,
        }
    }

    fn spawn(&self) -> Result<Spawn, String> {
        command_in(&self.data, "fx", &[], || Some(self.pinned.clone()))
    }

    fn written(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.settings).unwrap()).unwrap()
    }

    /// The app data folder, every MonoCode data folder beside it (the live
    /// `com.monocode.second` among them), then login items and the shell
    /// startup files.
    fn deny_read(&self) -> Vec<String> {
        let home = PathBuf::from(crate::dirs_home().unwrap());
        let startup = [
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
        let siblings = self.data.parent().unwrap().join("com.monocode.*");
        [path(&self.data), path(&siblings)]
            .into_iter()
            .chain(startup.map(|file| path(&home.join(file))))
            .collect()
    }
}

fn off_starts_the_cli_bare(rig: &Rig) {
    let off = command_in(&rig.data, "fx", &[], || panic!("srt looked up while off")).unwrap();
    assert!(!off.sandboxed, "off: not sandboxed");
    assert_eq!(
        argv(&off.cmd),
        ("fx".into(), vec![]),
        "off: the CLI starts bare"
    );
    assert!(!rig.settings.exists(), "off: no settings file");
}

fn on_refuses_without_a_working_pinned_srt(rig: &Rig) {
    let refused = |find: Option<PathBuf>| command_in(&rig.data, "fx", &[], || find).err().unwrap();
    let missing = refused(None);
    assert!(
        missing.contains("srt is not installed"),
        "missing srt: {missing}"
    );
    let wrong = refused(Some(fake_srt(&rig.tools.0, "0.0.80")));
    assert!(wrong.contains("needs srt 0.0.81"), "other version: {wrong}");
    let silent = script(&rig.tools.0.join("srt-silent"), "echo broken >&2; exit 1");
    let silent = refused(Some(silent));
    assert!(
        silent.contains("did not answer `--version`") && silent.contains("broken"),
        "srt that exits 1 with no output: {silent}"
    );
    assert!(!rig.settings.exists(), "a refused spawn writes no settings");
}

/// Mode 000 on a parent stops every user but root; CI runs as a normal user.
fn on_refuses_an_unreadable_data_folder() {
    let root = folder();
    let data = root.0.join("locked/com.monocode.test");
    std::fs::create_dir_all(&data).unwrap();
    let locked = root.0.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = command_in(&data, "fx", &[], || {
        panic!("srt looked up for an unreadable folder")
    });
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = result
        .err()
        .expect("an unreadable data folder starts the CLI");
    assert!(
        error.contains("Permission denied"),
        "unreadable folder: {error}"
    );
}

fn on_runs_the_cli_under_srt(rig: &Rig) {
    let on = rig.spawn().unwrap();
    assert!(on.sandboxed, "on: sandboxed");
    let args = vec![
        "--settings".into(),
        path(&rig.settings),
        "--".into(),
        "fx".into(),
    ];
    assert_eq!(
        argv(&on.cmd),
        (path(&rig.pinned), args),
        "on: srt runs the CLI"
    );
}

fn on_settings_deny_the_protected_paths(rig: &Rig) {
    let written = rig.written();
    let deny_read = strings(&written["filesystem"]["denyRead"]);
    let literal = rig.deny_read();
    assert_eq!(
        deny_read[..literal.len()],
        literal[..],
        "the protected paths, in order"
    );
    // srt matches the resolved path: the folder, and the pattern for every
    // MonoCode folder beside it, which names no existing path.
    let real = std::fs::canonicalize(rig.root.0.join("real")).unwrap();
    for entry in [real.join("com.monocode.test"), real.join("com.monocode.*")] {
        let entry = path(&entry);
        assert!(
            deny_read.contains(&entry),
            "the resolved {entry} is readable"
        );
    }
    assert_eq!(written["filesystem"]["allowRead"], json!([]));
    let deny_write = strings(&written["filesystem"]["denyWrite"]);
    for entry in &deny_read {
        assert!(
            deny_write.contains(entry),
            "{entry} is readable-denied but writable"
        );
    }
    // Config that programs outside the sandbox load later. The agent may read it.
    let home = PathBuf::from(crate::dirs_home().unwrap());
    let config = [".gitconfig", ".claude/settings.json", ".codex/config.toml"]
        .map(|file| home.join(file))
        .into_iter()
        .chain([xdg_config().join("git")]);
    for entry in config.map(|entry| path(&entry)) {
        assert!(deny_write.contains(&entry), "{entry} is writable");
        assert!(!deny_read.contains(&entry), "{entry} is unreadable");
    }
    // The test binary sits in no `.app`, so the binary itself is the bundle.
    assert!(deny_write.contains(&path(&std::env::current_exe().unwrap())));
    // The stand-in srt is in no `node_modules`, so its folder holds its code.
    let srt_code = path(&std::fs::canonicalize(&rig.tools.0).unwrap());
    assert!(
        deny_write.contains(&srt_code),
        "srt's code in {srt_code} is writable"
    );
    for dir in std::env::split_paths(&crate::harness::gui_search_path()) {
        if dir.is_absolute() {
            let entry = path(&dir.components().collect::<PathBuf>());
            assert!(
                deny_write.contains(&entry),
                "search PATH dir {entry} is writable"
            );
        }
    }
    let mut unique = deny_write.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), deny_write.len(), "denyWrite has duplicates");
}

fn xdg_config() -> PathBuf {
    let home = PathBuf::from(crate::dirs_home().unwrap());
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from)
}

/// The fixed defaults, the temp dir, then OpenCode's and Devin's config folders.
fn default_allow_write() -> Vec<String> {
    let fixed = [
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
    let temp = std::env::temp_dir().components().collect::<PathBuf>();
    let tools = ["opencode", "devin"].map(|tool| xdg_config().join(tool));
    let dirs = std::iter::once(temp).chain(tools).map(|dir| path(&dir));
    let mut list: Vec<String> = fixed.map(String::from).into_iter().chain(dirs).collect();
    // On Linux the temp dir is `/tmp`, which the app lists once.
    let mut seen = std::collections::HashSet::new();
    list.retain(|entry| seen.insert(entry.clone()));
    list
}

fn on_settings_start_with_the_default_allows(rig: &Rig) {
    let written = rig.written();
    let defaults = default_allow_write();
    let allowed = strings(&written["filesystem"]["allowWrite"]);
    assert_eq!(allowed[..defaults.len()], defaults[..]);
    assert!(
        !allowed.contains(&"~/.config".to_owned()),
        "all of ~/.config is writable"
    );
    // The temp dir is `/tmp` on Linux and under `/var/folders` on macOS, where
    // srt matches the resolved path: `/tmp` is `/private/tmp` there.
    let temp = std::env::temp_dir();
    let literal = path(&temp.components().collect::<PathBuf>());
    assert!(allowed.contains(&literal), "{literal} is not writable");
    for dir in [Path::new("/tmp"), temp.as_path()] {
        let real = path(&std::fs::canonicalize(dir).unwrap());
        assert!(allowed.contains(&real), "{real} is not writable");
    }
    let domains = strings(&written["network"]["allowedDomains"]);
    for domain in [
        "*.anthropic.com",
        "*.openai.com",
        "github.com",
        "registry.npmjs.org",
        "crates.io",
    ] {
        assert!(
            domains.contains(&domain.to_owned()),
            "{domain} is not allowed"
        );
    }
    assert_eq!(written["network"]["allowLocalBinding"], json!(true));
}

fn on_adds_only_the_user_additions(rig: &Rig) {
    let first = rig.written();
    let edit = json!({
        "extraAllowWrite": ["/work"],
        "extraAllowedDomains": ["example.com"],
        "filesystem": {"allowWrite": ["/"], "allowRead": ["/"], "denyRead": []},
        "allowAppleEvents": true,
    });
    std::fs::write(&rig.settings, edit.to_string()).unwrap();
    rig.spawn().unwrap();
    let edited = rig.written();
    let allow = strings(&edited["filesystem"]["allowWrite"]);
    let first_allow = strings(&first["filesystem"]["allowWrite"]);
    assert_eq!(
        allow,
        [first_allow, vec!["/work".into()]].concat(),
        "defaults, then the user's"
    );
    let domains = strings(&edited["network"]["allowedDomains"]);
    assert_eq!(domains.last().map(String::as_str), Some("example.com"));
    assert_eq!(
        edited["extraAllowWrite"],
        json!(["/work"]),
        "the user's additions stay"
    );
    assert_eq!(
        edited["filesystem"]["allowRead"],
        json!([]),
        "a user allowRead is dropped"
    );
    assert_eq!(
        edited["filesystem"]["denyRead"],
        first["filesystem"]["denyRead"]
    );
    assert!(
        edited.get("allowAppleEvents").is_none(),
        "other user keys are dropped"
    );

    for bad in [
        r#"{"extraAllowWrite": "/work"}"#,
        r#"{"extraAllowedDomains": [1]}"#,
    ] {
        std::fs::write(&rig.settings, bad).unwrap();
        let error = rig.spawn().err().expect("a bad user list starts the CLI");
        assert!(
            error.contains("must be a list of strings"),
            "{bad}: {error}"
        );
    }
    std::fs::write(&rig.settings, "{not json").unwrap();
    let error = rig
        .spawn()
        .err()
        .expect("a settings file that is not JSON starts the CLI");
    assert!(error.contains("invalid JSON"), "not JSON: {error}");
    assert_eq!(std::fs::read_to_string(&rig.settings).unwrap(), "{not json");
}

/// The file this PR's first head wrote holds the defaults of that day, with
/// all of `~/.config` writable. The app must not keep them as user values.
fn an_old_settings_file_gets_the_current_defaults(rig: &Rig) {
    let old = json!({
        "filesystem": {"allowWrite": [".", "/tmp", "~/.config"], "denyRead": [], "denyWrite": []},
        "network": {"allowedDomains": ["*.anthropic.com"], "allowLocalBinding": true},
    });
    std::fs::write(&rig.settings, old.to_string()).unwrap();
    rig.spawn().unwrap();
    let written = rig.written();
    let allowed = strings(&written["filesystem"]["allowWrite"]);
    assert_eq!(
        allowed[..default_allow_write().len()],
        default_allow_write()[..]
    );
    assert!(
        !allowed.contains(&"~/.config".to_owned()),
        "the old ~/.config entry stayed"
    );
    assert!(strings(&written["network"]["allowedDomains"]).contains(&"github.com".to_owned()));
    assert_eq!(written["extraAllowWrite"], json!([]));
}

pub(super) fn wait_for(file: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(file) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{} never appeared", file.display());
}

const SPAWN_ROOT: &str = "MC_ADE41_SPAWN_ROOT";

/// The `harness_spawn` command on Tauri's mock runtime, with a stand-in fx and
/// a stand-in srt that each write a file in the child's folder. Each writes a
/// part file and renames it, so `wait_for` never reads a file the shell has
/// created but not yet filled. It runs in a process of its own, so the PATH
/// that puts the stand-in srt first changes no other test's environment.
fn harness_spawn_starts_the_cli_bare_when_off_and_through_srt_when_on() {
    let root = folder();
    // Resolved, as the app's binary resolver hands back a path.
    let tools = std::fs::canonicalize(&root.0).unwrap().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    script(
        &tools.join("fx"),
        r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) echo "$*" > ran.part && mv ran.part ran ;;
esac"#,
    );
    script(
        &tools.join("srt"),
        r#"[ "$1" = --version ] && { echo 0.0.81; exit 0; }
echo "$2" > through-srt.part && mv through-srt.part through-srt
shift 3
exec "$@""#,
    );
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(tools.clone()).chain(std::env::split_paths(&inherited));
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "scenario::agent_sandbox::harness_spawn_child",
            "--ignored",
            "--quiet",
        ])
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env(SPAWN_ROOT, &tools)
        .env("XDG_CONFIG_HOME", root.0.join("xdg"))
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "harness_spawn did not start the CLI as the setting says:\n{}",
        String::from_utf8_lossy(&child.stdout)
    );
}

#[test]
#[ignore = "the_sandbox_setting_decides_how_a_harness_child_starts runs it in a process of its own"]
fn harness_spawn_child() {
    let tools = PathBuf::from(std::env::var_os(SPAWN_ROOT).unwrap());
    let root = tools.parent().unwrap();
    let fx = tools.join("fx");
    // The app finds srt on its search PATH, which ends with this process's PATH.
    assert_eq!(
        crate::harness::resolve_gui_binary("srt"),
        Some(tools.join("srt")),
        "another srt comes first on the search PATH"
    );

    let data = root.join("app-data");
    let app = mock_app(&data);
    let window = main_window(&app);
    std::fs::create_dir_all(&data).unwrap();

    let spawn = |session: &str| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        let pid = crate::harness::harness_spawn(
            app.handle().clone(),
            window.clone(),
            app.state::<crate::harness::HarnessHost>(),
            session.into(),
            path(&fx),
            vec!["acp".into()],
            path(&workdir),
            None,
            Some("fx".into()),
            Some(path(&fx)),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(pid > 1, "{session}: pid {pid}");
        workdir
    };

    let off = spawn("off");
    assert_eq!(wait_for(&off.join("ran")).trim(), "acp", "off: the CLI ran");
    assert!(!off.join("through-srt").exists(), "off: srt ran");
    assert!(
        !data.join("agent-sandbox.json").exists(),
        "off: settings written"
    );

    std::fs::write(data.join("agent-sandbox-on"), b"").unwrap();
    let on = spawn("on");
    assert_eq!(wait_for(&on.join("ran")).trim(), "acp", "on: the CLI ran");
    let settings = wait_for(&on.join("through-srt"));
    assert_eq!(
        settings.trim(),
        path(&data.join("agent-sandbox.json")),
        "on: srt ran it"
    );
    // This process has `XDG_CONFIG_HOME` set, as some users do.
    let text = std::fs::read_to_string(data.join("agent-sandbox.json")).unwrap();
    let written: Value = serde_json::from_str(&text).unwrap();
    let xdg = root.join("xdg");
    let allowed = strings(&written["filesystem"]["allowWrite"]);
    assert!(
        allowed.contains(&path(&xdg.join("opencode"))),
        "on: {allowed:?}"
    );
    let denied = strings(&written["filesystem"]["denyWrite"]);
    assert!(
        denied.contains(&path(&xdg.join("git"))),
        "on: git config under XDG is writable"
    );
}

/// Tauri's mock runtime with the state `harness_spawn` reads, its app data
/// folder at `data`: Tauri names it `<data dir>/<identifier>`, and an absolute
/// identifier replaces the whole path.
pub(super) fn mock_app(data: &Path) -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = path(data);
    let app = tauri::test::mock_builder()
        .manage(crate::harness::HarnessHost::new())
        .manage(crate::control::tests::host())
        .manage(crate::session_store::SessionStore::open_in_memory().unwrap())
        .build(context)
        .unwrap();
    assert_eq!(app.path().app_data_dir().unwrap(), data);
    app
}

/// The window `harness_spawn` is called from.
pub(super) fn main_window(
    app: &tauri::App<tauri::test::MockRuntime>,
) -> tauri::WebviewWindow<tauri::test::MockRuntime> {
    tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
        .build()
        .unwrap()
}

/// Defect: with the setting off a child starts wrapped, or a settings file
/// appears; with it on, a missing, other-version or broken srt, a settings
/// file that is not JSON, or a data folder the app cannot read starts the
/// child bare; the settings leave the app data folder, a sibling MonoCode
/// folder, a startup file, git's global config, the bundle, srt's own code or
/// a search PATH dir open; an old file's defaults survive as user values; or
/// `harness_spawn` returns a pid without starting the CLI as the setting says.
#[test]
fn the_sandbox_setting_decides_how_a_harness_child_starts() {
    let rig = Rig::new();
    off_starts_the_cli_bare(&rig);
    on_refuses_an_unreadable_data_folder();
    std::fs::write(rig.data.join("agent-sandbox-on"), b"").unwrap();
    on_refuses_without_a_working_pinned_srt(&rig);
    on_runs_the_cli_under_srt(&rig);
    on_settings_deny_the_protected_paths(&rig);
    on_settings_start_with_the_default_allows(&rig);
    on_adds_only_the_user_additions(&rig);
    an_old_settings_file_gets_the_current_defaults(&rig);
    harness_spawn_starts_the_cli_bare_when_off_and_through_srt_when_on();
}
