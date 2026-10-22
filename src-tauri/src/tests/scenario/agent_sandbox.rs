//! The agent sandbox setting (ADE-41) through `agent_sandbox::command_in`, the
//! body of the command `harness_spawn` starts, on a real app data folder and a
//! stand-in srt that answers `--version`. `scripts/scenarios/app.sh d1` runs
//! the real srt in the real app.

#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::agent_sandbox::command_in;

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

/// An executable that prints `version` for `--version`, as srt does.
fn fake_srt(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join(format!("srt-{version}"));
    std::fs::write(&path, format!("#!/bin/sh\necho {version}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
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

/// One app data folder and the srt stand-ins, for the whole scenario.
struct Rig {
    data: Folder,
    tools: Folder,
    settings: PathBuf,
    pinned: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let data = folder();
        let tools = folder();
        let settings = data.0.join("agent-sandbox.json");
        let pinned = fake_srt(&tools.0, "0.0.81");
        Self {
            data,
            tools,
            settings,
            pinned,
        }
    }

    fn spawn(&self) -> Result<std::process::Command, String> {
        command_in(&self.data.0, "fx", || Some(self.pinned.clone()))
    }

    fn written(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.settings).unwrap()).unwrap()
    }

    /// The app data folder, every MonoCode data folder beside it (the live
    /// `com.monocode.second` among them), then LaunchAgents and the shell
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
        let siblings = self.data.0.parent().unwrap().join("com.monocode.*");
        [path(&self.data.0), path(&siblings)]
            .into_iter()
            .chain(startup.map(|file| path(&home.join(file))))
            .collect()
    }
}

fn off_starts_the_cli_bare(rig: &Rig) {
    let off = command_in(&rig.data.0, "fx", || panic!("srt looked up while off")).unwrap();
    assert_eq!(
        argv(&off),
        ("fx".into(), vec![]),
        "off: the CLI starts bare"
    );
    assert!(!rig.settings.exists(), "off: no settings file");
}

fn on_refuses_without_the_pinned_srt(rig: &Rig) {
    let missing = command_in(&rig.data.0, "fx", || None).unwrap_err();
    assert!(
        missing.contains("srt is not installed"),
        "missing srt: {missing}"
    );
    let older = fake_srt(&rig.tools.0, "0.0.80");
    let wrong = command_in(&rig.data.0, "fx", || Some(older)).unwrap_err();
    assert!(wrong.contains("needs srt 0.0.81"), "other version: {wrong}");
    assert!(!rig.settings.exists(), "a refused spawn writes no settings");
}

fn on_runs_the_cli_under_srt(rig: &Rig) {
    let on = rig.spawn().unwrap();
    let args = vec![
        "--settings".into(),
        path(&rig.settings),
        "--".into(),
        "fx".into(),
    ];
    assert_eq!(argv(&on), (path(&rig.pinned), args), "on: srt runs the CLI");
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
    // The folder sits in the temp dir, which macOS reaches through /private.
    let real = path(&std::fs::canonicalize(&rig.data.0).unwrap());
    assert!(deny_read.contains(&real), "the resolved {real} is readable");
    assert_eq!(written["filesystem"]["allowRead"], json!([]));
    let deny_write = strings(&written["filesystem"]["denyWrite"]);
    for entry in &deny_read {
        assert!(
            deny_write.contains(entry),
            "{entry} is readable-denied but writable"
        );
    }
    // The test binary sits in no `.app`, so the binary itself is the bundle.
    assert!(deny_write.contains(&path(&std::env::current_exe().unwrap())));
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

fn on_settings_start_with_the_default_allows(rig: &Rig) {
    let written = rig.written();
    let defaults = [
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
    ]
    .map(String::from);
    let allowed = strings(&written["filesystem"]["allowWrite"]);
    assert_eq!(allowed[..defaults.len()], defaults[..]);
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

fn on_keeps_only_the_user_lists(rig: &Rig) {
    let first = rig.written();
    let edit = json!({
        "filesystem": {"allowWrite": ["/work"], "allowRead": ["/"], "denyRead": []},
        "network": {"allowedDomains": ["example.com"]},
        "allowAppleEvents": true,
    });
    std::fs::write(&rig.settings, edit.to_string()).unwrap();
    rig.spawn().unwrap();
    let edited = rig.written();
    assert_eq!(
        edited["filesystem"]["allowWrite"],
        json!(["/work"]),
        "the user's allowWrite stays"
    );
    assert_eq!(
        edited["network"]["allowedDomains"],
        json!(["example.com"]),
        "the user's domains stay"
    );
    assert_eq!(
        edited["filesystem"]["allowRead"],
        json!([]),
        "a user allowRead is dropped"
    );
    assert_eq!(
        edited["filesystem"]["denyRead"], first["filesystem"]["denyRead"],
        "denyRead is the app's"
    );
    assert!(
        edited.get("allowAppleEvents").is_none(),
        "other user keys are dropped"
    );

    std::fs::write(&rig.settings, r#"{"filesystem": {"allowWrite": "/work"}}"#).unwrap();
    let bad = rig.spawn().unwrap_err();
    assert!(
        bad.contains("/filesystem/allowWrite must be a list"),
        "not a list: {bad}"
    );
}

/// Defect: with the setting off a child starts wrapped, or a settings file
/// appears; with it on, a missing or other-version srt starts the child bare;
/// the settings leave the app data folder, the bundle, a startup file or a
/// search PATH dir open; or the app drops the user's own lists, or keeps a
/// user key that would reopen a denied path.
#[test]
fn the_sandbox_setting_decides_how_a_harness_child_starts() {
    let rig = Rig::new();
    off_starts_the_cli_bare(&rig);
    std::fs::write(rig.data.0.join("agent-sandbox-on"), b"").unwrap();
    on_refuses_without_the_pinned_srt(&rig);
    on_runs_the_cli_under_srt(&rig);
    on_settings_deny_the_protected_paths(&rig);
    on_settings_start_with_the_default_allows(&rig);
    on_keeps_only_the_user_lists(&rig);
}
