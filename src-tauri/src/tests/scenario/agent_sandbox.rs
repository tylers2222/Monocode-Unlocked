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
        command_in(&self.data, "fx", None, &[], || Some(self.pinned.clone()))
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
    let off = command_in(&rig.data, "fx", None, &[], || {
        panic!("srt looked up while off")
    })
    .unwrap();
    assert!(!off.sandboxed, "off: not sandboxed");
    assert_eq!(
        argv(&off.cmd),
        ("fx".into(), vec![]),
        "off: the CLI starts bare"
    );
    assert!(!rig.settings.exists(), "off: no settings file");
}

fn on_refuses_without_a_working_pinned_srt(rig: &Rig) {
    let refused = |find: Option<PathBuf>| {
        command_in(&rig.data, "fx", None, &[], || find)
            .err()
            .unwrap()
    };
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
    let result = command_in(&data, "fx", None, &[], || {
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

/// The fixed defaults, the temp dir, then the OpenCode, Devin, gh and doctl
/// config folders.
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
        "~/MonoCode",
        "~/Library/Application Support/doctl",
    ];
    let temp = std::env::temp_dir().components().collect::<PathBuf>();
    let tools = ["opencode", "devin", "gh", "doctl"].map(|tool| xdg_config().join(tool));
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
    std::fs::create_dir_all(&data).unwrap();

    let spawn = |session: &str| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        let pid = crate::harness::harness_spawn(
            app.handle().clone(),
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
            None,
        )
        .unwrap()
        .pid;
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
    mock_app_with(
        data,
        crate::session_store::SessionStore::open_in_memory().unwrap(),
    )
}

/// `mock_app` on `store`, so a spawn sees the rows a scenario wrote.
pub(super) fn mock_app_with(
    data: &Path,
    store: crate::session_store::SessionStore,
) -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = path(data);
    let app = tauri::test::mock_builder()
        .manage(crate::harness::HarnessHost::new())
        .manage(store)
        .build(context)
        .unwrap();
    assert_eq!(app.path().app_data_dir().unwrap(), data);
    app
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

/// A stand-in srt in `dir` that writes the settings file it is given to
/// `srt-settings` in the child's folder, then runs the CLI as it is. Spawns on
/// this thread find it in place of the search PATH's.
pub(super) fn stand_in_srt(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let srt = script(
        &dir.join("srt"),
        r#"[ "$1" = --version ] && { echo 0.0.81; exit 0; }
printf '%s' "$2" > srt-settings
shift 3
exec "$@""#,
    );
    crate::agent_sandbox::SRT_STAND_IN.with(|found| *found.borrow_mut() = Some(Some(srt.clone())));
    srt
}

/// The Space write fence (ADE-177) through `harness_spawn`, on a git project
/// that two Spaces open: the spawn hook (ADE-173) places Space 2's session in
/// its own copy, and the stand-in srt keeps the settings each child got.
/// The real srt is not on the CI box; the PR quotes a real srt run on
/// made-up folders, where seatbelt refuses a denied write with "Operation not
/// permitted".
mod space_fence {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::{json, Value};
    use tauri::test::MockRuntime;
    use tauri::Manager;

    use super::{
        folder, mock_app, path, script, stand_in_srt, strings, wait_for, xdg_config, Folder,
    };
    use crate::agent_sandbox::{space_fence, SRT_STAND_IN};
    use crate::session_store::{self, SessionStore};

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Session `id` of `space`, saved in `cwd` as the window saves it.
    fn save(store: &SessionStore, id: &str, cwd: &Path, space: &str) {
        let session = serde_json::from_value(json!({
            "id": id,
            "cwd": path(cwd),
            "harness": "codex",
            "model": "codex:default",
            "modelSettings": {},
            "runtimeMode": "auto",
            "title": id,
            "blocks": [{"id": "u1", "role": "user", "text": "Start"}],
        }))
        .unwrap();
        let conn = store.lock_conn().unwrap();
        session_store::upsert_session(&conn, &session).unwrap();
        conn.execute(
            "UPDATE sessions SET space_id = ?2 WHERE id = ?1",
            [id, space],
        )
        .unwrap();
    }

    struct Rig {
        _root: Folder,
        project: PathBuf,
        copy: PathBuf,
        fx: PathBuf,
        data: PathBuf,
        app: tauri::App<MockRuntime>,
    }

    impl Rig {
        /// `project`, a git checkout with `README` and `src/` in its one
        /// commit, and sessions `one` (Space 1) and `two` (Space 2) in it.
        fn new() -> Self {
            let root = folder();
            let real = std::fs::canonicalize(&root.0).unwrap();
            let project = real.join("project");
            std::fs::create_dir_all(project.join("src")).unwrap();
            std::fs::write(project.join("src/lib.rs"), "").unwrap();
            std::fs::write(project.join("README"), "").unwrap();
            git(&project, &["init", "-q", "-b", "main"]);
            git(&project, &["add", "."]);
            git(&project, &["commit", "-qm", "initial"]);
            let fx = script(
                &real.join("fx"),
                r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) pwd -P > ran.part && mv ran.part ran ;;
esac"#,
            );
            // Claude Code's deny rules need a `com.monocode.*` data folder.
            let data = real.join("com.monocode.ade177");
            std::fs::create_dir_all(&data).unwrap();
            let app = mock_app(&data);
            let store = app.state::<SessionStore>();
            save(&store, "one", &project, crate::spaces::FIRST_SPACE);
            save(&store, "two", &project, "space-2");
            Self {
                copy: real.join("project-worktrees/space-2/two"),
                _root: root,
                project,
                fx,
                data,
                app,
            }
        }

        fn store(&self) -> tauri::State<'_, SessionStore> {
            self.app.state::<SessionStore>()
        }

        /// `session` asks to start in the project; the child runs where the
        /// spawn hook placed it, and its `ran` names that folder.
        fn spawn(&self, session: &str) -> Result<crate::harness::Spawned, String> {
            self.spawn_in(session, &self.project)
        }

        fn spawn_in(
            &self,
            session: &str,
            folder: &Path,
        ) -> Result<crate::harness::Spawned, String> {
            crate::harness::harness_spawn(
                self.app.handle().clone(),
                self.app.state::<crate::harness::HarnessHost>(),
                session.into(),
                path(&self.fx),
                vec!["acp".into()],
                path(folder),
                None,
                Some("fx".into()),
                Some(path(&self.fx)),
                None,
                None,
                None,
                None,
            )
        }

        /// The child started in `folder` through srt: srt's settings there.
        fn fenced_in(&self, folder: &Path) -> (Vec<String>, Vec<String>) {
            assert_eq!(
                wait_for(&folder.join("ran")).trim(),
                path(folder),
                "the child ran elsewhere"
            );
            let settings = wait_for(&folder.join("srt-settings"));
            let written: Value =
                serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
            let fs = &written["filesystem"];
            (strings(&fs["allowWrite"]), strings(&fs["denyWrite"]))
        }
    }

    /// A deny on `kept` or a folder above it, which would refuse every write there.
    fn denied_above(deny: &[String], kept: &Path) -> Vec<String> {
        deny.iter()
            .filter(|entry| kept.starts_with(entry.as_str()))
            .cloned()
            .collect()
    }

    fn with_one_space_a_spawn_starts_as_the_marker_says(rig: &Rig) {
        assert!(space_fence(&rig.store(), Some("one"), &rig.project)
            .unwrap()
            .is_none());
        // ADE-199: the caller learns srt does not run it, so Codex keeps its sandbox.
        assert!(!rig.spawn("one").unwrap().sandboxed, "one Space: sandboxed");
        assert_eq!(
            wait_for(&rig.project.join("ran")).trim(),
            path(&rig.project)
        );
        assert!(
            !rig.project.join("srt-settings").exists(),
            "one Space without the marker: the child ran through srt"
        );
        std::fs::remove_file(rig.project.join("ran")).unwrap();
    }

    /// Space 2's child writes its own copy and the `.git` the copy commits to,
    /// and nothing else of Space 1's checkout.
    fn space_two_writes_only_its_own_copy(rig: &Rig) {
        // ADE-199: the caller learns srt runs it, so Codex starts no sandbox inside.
        assert!(
            rig.spawn("two").unwrap().sandboxed,
            "two Spaces: not sandboxed"
        );
        let (allow, deny) = rig.fenced_in(&rig.copy);
        let git = rig.project.join(".git");
        for own in [&rig.copy, &git] {
            assert!(
                allow.contains(&path(own)),
                "{} is not writable: {allow:?}",
                own.display()
            );
            assert_eq!(
                denied_above(&deny, own),
                Vec::<String>::new(),
                "{} is denied",
                own.display()
            );
        }
        assert!(
            !allow.contains(&path(&rig.project)),
            "Space 1's checkout is writable"
        );
        // Git runs `.git/config`'s core.fsmonitor and core.hooksPath, and
        // `.git/hooks`, outside srt for Space 1's checkout too.
        let space_one = ["README", "src", ".git/hooks", ".git/config"];
        for other in space_one.map(|name| rig.project.join(name)) {
            assert!(
                deny.contains(&path(&other)),
                "Space 1's {} is writable: {deny:?}",
                other.display()
            );
        }
        let home = PathBuf::from(crate::dirs_home().unwrap());
        for giga in ["hooks", "config"].map(|name| home.join("MonoCode/.git").join(name)) {
            assert!(
                deny.contains(&path(&giga)),
                "{} is writable",
                giga.display()
            );
        }
        let temp = path(&std::env::temp_dir().components().collect::<PathBuf>());
        let shared = [
            "~/MonoCode",
            "~/.claude",
            "~/.codex",
            "~/Library/Application Support/doctl",
        ]
        .map(String::from)
        .into_iter()
        .chain(["gh", "doctl"].map(|tool| path(&xdg_config().join(tool))))
        .chain([temp]);
        for entry in shared {
            assert!(allow.contains(&entry), "the shared {entry} is not writable");
            assert!(!deny.contains(&entry), "the shared {entry} is denied");
        }
        assert!(!allow.contains(&path(&home)), "all of HOME is writable");
    }

    /// Space 1's child is kept out of the whole copy Space 2 holds.
    fn space_one_cannot_write_space_twos_copy(rig: &Rig) {
        rig.spawn("one").unwrap();
        let (allow, deny) = rig.fenced_in(&rig.project);
        assert!(
            allow.contains(&path(&rig.project)),
            "Space 1's own checkout is not writable"
        );
        assert!(
            deny.contains(&path(&rig.copy)),
            "Space 2's copy is writable: {deny:?}"
        );
        assert!(
            !allow.contains(&path(&rig.copy)),
            "Space 2's copy is writable"
        );
        assert_eq!(denied_above(&deny, &rig.project), Vec::<String>::new());
        let inside: Vec<&String> = deny
            .iter()
            .filter(|entry| Path::new(entry).starts_with(&rig.project))
            .collect();
        let git_code = ["hooks", "config"].map(|name| path(&rig.project.join(".git").join(name)));
        assert_eq!(
            inside,
            git_code.iter().collect::<Vec<_>>(),
            "Space 1's own checkout is denied beyond its git hooks and config"
        );
        let giga = PathBuf::from(crate::dirs_home().unwrap()).join("MonoCode");
        assert!(
            !deny.contains(&path(&giga)),
            "the shared Giga workspace is denied to Space 1"
        );
    }

    /// The Giga workspace `~/MonoCode` is shared by every Space: a Space 2
    /// session there stays, takes no lease, and its git code stays denied.
    fn the_giga_workspace_is_shared_and_never_leased(rig: &Rig) {
        let giga = PathBuf::from(crate::dirs_home().unwrap()).join("MonoCode");
        let folder = giga.join("gigas/g");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(giga.join("index.md"), "").unwrap();
        git(&giga, &["init", "-q", "-b", "main"]);
        git(&giga, &["add", "."]);
        git(&giga, &["commit", "-qm", "initial"]);
        save(&rig.store(), "giga", &folder, "space-2");
        let spawned = rig.spawn_in("giga", &folder).unwrap();
        assert_eq!(spawned.cwd, path(&folder), "the Giga folder was copied");
        let (allow, deny) = rig.fenced_in(&folder);
        assert!(allow.contains(&"~/MonoCode".to_owned()));
        assert_eq!(denied_above(&deny, &folder), Vec::<String>::new());
        for code in ["hooks", "config"].map(|name| giga.join(".git").join(name)) {
            assert!(
                deny.contains(&path(&code)),
                "{} is writable",
                code.display()
            );
        }
        let leased = crate::spaces::leases::leases(&rig.store()).unwrap();
        assert!(
            leased
                .iter()
                .all(|lease| !Path::new(&lease.checkout_path).starts_with(&giga)),
            "the Giga workspace was leased: {leased:?}"
        );
    }

    /// OpenCode v2 runs its sessions' shells in a background service srt
    /// cannot fence, so with two Spaces it is refused; with one it runs.
    fn opencode_v2_is_refused_while_fenced(rig: &Rig, fenced: bool) {
        let opencode = script(
            &rig.data.with_file_name("tools").join("opencode"),
            r#"[ "$1" = service ] && { echo "http://127.0.0.1:4096"; exit 0; }
echo "opencode 2.0.0""#,
        );
        let ran = tauri::async_runtime::block_on(crate::harness::harness_exec(
            rig.app.handle().clone(),
            path(&opencode),
            vec!["service".into(), "start".into()],
            Some(path(&rig.project)),
            Some("opencode".into()),
            Some(path(&opencode)),
            None,
        ));
        match ran {
            Ok(ran) => assert!(!fenced && ran.stdout.contains("4096"), "{}", ran.stdout),
            Err(error) => assert!(
                fenced && error.contains("OpenCode v2 cannot run fenced between Spaces yet"),
                "{error}"
            ),
        }
    }

    /// A helper no session owns has no Space: every leased checkout is another's.
    fn a_helper_of_no_session_owns_no_checkout(rig: &Rig) {
        let fence = space_fence(&rig.store(), None, &rig.project)
            .unwrap()
            .unwrap();
        assert_eq!(fence.others, [rig.project.clone(), rig.copy.clone()]);
    }

    /// Claude Code's deny rules (ADE-44) name the same checkouts as srt's
    /// denies: a stand-in `claude` in Space 2 keeps the args it got.
    fn claude_gets_edit_rules_on_the_other_spaces_checkouts(rig: &Rig) {
        let fence = space_fence(&rig.store(), Some("two"), &rig.copy)
            .unwrap()
            .unwrap();
        assert_eq!(
            fence.others,
            std::slice::from_ref(&rig.project),
            "Space 2's fence"
        );
        let claude = script(
            &rig.data.with_file_name("tools").join("claude"),
            r#"[ "$1" = --version ] && { echo "2.1.309 (Claude Code)"; exit 0; }
printf '%s\0' "$@" > argv.part && mv argv.part argv"#,
        );
        std::fs::write(rig.data.join("claude-deny-rules-on"), b"").unwrap();
        crate::harness::harness_spawn(
            rig.app.handle().clone(),
            rig.app.state::<crate::harness::HarnessHost>(),
            "two".into(),
            path(&claude),
            vec!["--settings".into(), "{}".into()],
            path(&rig.project),
            None,
            Some("claude".into()),
            Some(path(&claude)),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let argv = wait_for(&rig.copy.join("argv"));
        let settings = argv.split_terminator('\0').nth(1).unwrap();
        let settings: Value = serde_json::from_str(settings).unwrap();
        let deny = strings(&settings["permissions"]["deny"]);
        let rule = format!("Edit(/{}/**)", path(&rig.project));
        assert!(deny.contains(&rule), "no {rule}: {deny:?}");
        let own = format!("Edit(/{}/**)", path(&rig.copy));
        assert!(
            !deny.contains(&own),
            "Claude Code may not edit its own copy"
        );
        std::fs::remove_file(rig.data.join("claude-deny-rules-on")).unwrap();
    }

    fn without_srt_a_fenced_spawn_is_refused(rig: &Rig) {
        SRT_STAND_IN.with(|found| *found.borrow_mut() = Some(None));
        std::fs::remove_file(rig.copy.join("ran")).unwrap();
        let error = rig
            .spawn("two")
            .expect_err("Space 2 started unfenced without srt");
        assert!(
            error.contains("srt is not installed") && error.contains("fence each Space's writes"),
            "the refusal: {error}"
        );
        assert!(!rig.copy.join("ran").exists(), "the child ran without srt");
    }

    /// Defect: with two Spaces a child starts without srt, or with srt but
    /// without the fence; Space 2's child may write Space 1's checkout, its
    /// `.git` hooks or config, the Giga workspace's, or all of HOME, or may
    /// not write its own copy, the `.git` that copy commits to or the shared
    /// folders; Space 1's child may write Space 2's copy or not its own
    /// checkout; the Giga workspace is leased or denied to a Space; Claude
    /// Code's rules miss the other Space's checkout; OpenCode v2's service
    /// starts while fenced; a fenced spawn runs when srt is missing; or one
    /// Space without the marker starts through srt. A process of its own with
    /// a temp HOME, so its `~/MonoCode` is made up.
    #[test]
    fn the_space_fence_keeps_each_space_out_of_the_others_checkouts() {
        let home = folder();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "scenario::agent_sandbox::space_fence::space_fence_child",
                "--ignored",
                "--quiet",
            ])
            .env("HOME", std::fs::canonicalize(&home.0).unwrap())
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "the fence let a Space write what it must not:\n{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }

    #[test]
    #[ignore = "the_space_fence_keeps_each_space_out_of_the_others_checkouts runs it with a temp HOME"]
    fn space_fence_child() {
        let rig = Rig::new();
        stand_in_srt(&rig.data.with_file_name("tools"));
        with_one_space_a_spawn_starts_as_the_marker_says(&rig);
        opencode_v2_is_refused_while_fenced(&rig, false);
        rig.store()
            .lock_conn()
            .unwrap()
            .execute(
                "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Space 2', 0)",
                [],
            )
            .unwrap();
        space_two_writes_only_its_own_copy(&rig);
        the_giga_workspace_is_shared_and_never_leased(&rig);
        space_one_cannot_write_space_twos_copy(&rig);
        a_helper_of_no_session_owns_no_checkout(&rig);
        claude_gets_edit_rules_on_the_other_spaces_checkouts(&rig);
        opencode_v2_is_refused_while_fenced(&rig, true);
        without_srt_a_fenced_spawn_is_refused(&rig);
    }
}
