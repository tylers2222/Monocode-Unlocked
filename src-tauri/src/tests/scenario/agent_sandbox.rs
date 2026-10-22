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

/// Defect: with the setting off a child starts wrapped, or a settings file
/// appears; with it on, a missing or other-version srt starts the child bare;
/// the settings leave the app data folder, the bundle, a startup file or a
/// search PATH dir open; or the app drops the user's own lists, or keeps a
/// user key that would reopen a denied path.
#[test]
fn the_sandbox_setting_decides_how_a_harness_child_starts() {
    let data = folder();
    let tools = folder();
    let settings = data.0.join("agent-sandbox.json");
    let marker = data.0.join("agent-sandbox-on");
    let pinned = fake_srt(&tools.0, "0.0.81");

    let off = command_in(&data.0, "fx", || panic!("srt looked up while off")).unwrap();
    assert_eq!(
        argv(&off),
        ("fx".into(), vec![]),
        "off: the CLI starts bare"
    );
    assert!(!settings.exists(), "off: no settings file");

    std::fs::write(&marker, b"").unwrap();
    let missing = command_in(&data.0, "fx", || None).unwrap_err();
    assert!(
        missing.contains("srt is not installed"),
        "missing srt: {missing}"
    );
    let older = fake_srt(&tools.0, "0.0.80");
    let wrong = command_in(&data.0, "fx", || Some(older)).unwrap_err();
    assert!(wrong.contains("needs srt 0.0.81"), "other version: {wrong}");
    assert!(!settings.exists(), "a refused spawn writes no settings");

    let on = command_in(&data.0, "fx", || Some(pinned.clone())).unwrap();
    let path = |p: &Path| p.to_string_lossy().into_owned();
    assert_eq!(
        argv(&on),
        (
            path(&pinned),
            vec![
                "--settings".into(),
                path(&settings),
                "--".into(),
                "fx".into()
            ]
        ),
        "on: srt runs the CLI with the app's settings"
    );
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let home = PathBuf::from(crate::dirs_home().unwrap());
    let mut deny_read = vec![path(&data.0)];
    deny_read.extend(
        [
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
        ]
        .map(|file| path(&home.join(file))),
    );
    assert_eq!(strings(&written["filesystem"]["denyRead"]), deny_read);
    assert_eq!(written["filesystem"]["allowRead"], json!([]));
    let deny_write = strings(&written["filesystem"]["denyWrite"]);
    assert_eq!(
        deny_write[..deny_read.len()],
        deny_read[..],
        "reads denied are writes denied"
    );
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
    let mut allow_write: Vec<String> = [
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
    .map(String::from)
    .to_vec();
    allow_write.push(path(
        &std::env::temp_dir().components().collect::<PathBuf>(),
    ));
    assert_eq!(strings(&written["filesystem"]["allowWrite"]), allow_write);
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

    std::fs::write(
        &settings,
        json!({
            "filesystem": {"allowWrite": ["/work"], "allowRead": ["/"], "denyRead": []},
            "network": {"allowedDomains": ["example.com"]},
            "allowAppleEvents": true,
        })
        .to_string(),
    )
    .unwrap();
    command_in(&data.0, "fx", || Some(pinned.clone())).unwrap();
    let edited: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
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
        strings(&edited["filesystem"]["denyRead"]),
        deny_read,
        "denyRead is the app's"
    );
    assert!(
        edited.get("allowAppleEvents").is_none(),
        "other user keys are dropped"
    );

    std::fs::write(&settings, r#"{"filesystem": {"allowWrite": "/work"}}"#).unwrap();
    let bad = command_in(&data.0, "fx", || Some(pinned.clone())).unwrap_err();
    assert!(
        bad.contains("/filesystem/allowWrite must be a list"),
        "not a list: {bad}"
    );
}
