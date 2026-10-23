//! D4 (ADE-44) on real app data folders: what `harness_spawn` gets back from
//! `claude_deny::args_in` with the marker absent, present, and unreadable, the
//! Edit rules on the Claude Code config folder and on symlinked settings files,
//! and that every bad input fails the spawn instead of starting Claude Code bare.
//! `scripts/scenarios/app.sh d4` proves what the rules stop, with the real CLI.

use std::ffi::OsStr;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::claude_deny::args_in;

fn root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("monocode-scenario-d4-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_args(settings: &str) -> Vec<String> {
    [
        "--verbose",
        "--settings",
        settings,
        "--model",
        "claude-haiku-4-5",
    ]
    .map(String::from)
    .to_vec()
}

fn settings_of(args: &[String]) -> serde_json::Value {
    let at = args.iter().position(|arg| arg == "--settings").unwrap() + 1;
    serde_json::from_str(&args[at]).unwrap()
}

fn has_rules(args: &[String]) -> bool {
    let settings = settings_of(args);
    settings["permissions"]["deny"].is_array() && settings["sandbox"]["enabled"] == true
}

fn deny_of(args: &[String]) -> Vec<String> {
    serde_json::from_value(settings_of(args)["permissions"]["deny"].clone()).unwrap()
}

/// `link` points at a new folder or file `target` (file content `{}`).
fn link_to(target: &Path, link: &Path, folder: bool) {
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    if folder {
        std::fs::create_dir_all(target).unwrap();
    } else {
        std::fs::write(target, "{}").unwrap();
    }
    symlink(target, link).unwrap();
}

fn turn_on(data: &Path) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("claude-deny-rules-on"), "").unwrap();
}

#[test]
fn d4_marker_decides_and_bad_input_fails_the_spawn() {
    let root = root();
    let data = root.join("com.monocode.desktop");
    let (home, work) = (root.join("home"), root.join("work"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let plain = spawn_args(r#"{"alwaysThinkingEnabled":true}"#);
    let spawn = |data: &Path, config: Option<&str>, args: Vec<String>| {
        args_in(data, config.map(OsStr::new), &home, &work, false, args)
    };

    assert_eq!(
        spawn(&data, None, plain.clone()).unwrap(),
        plain,
        "off: args unchanged"
    );

    turn_on(&data);
    let on = spawn(&data, None, plain.clone()).unwrap();
    assert!(has_rules(&on), "on: rules added");
    assert_eq!(
        settings_of(&on)["alwaysThinkingEnabled"],
        true,
        "on: other keys kept"
    );
    let in_srt = settings_of(&args_in(&data, None, &home, &work, true, plain.clone()).unwrap());
    assert!(
        in_srt["permissions"]["deny"].is_array() && in_srt.get("sandbox").is_none(),
        "on inside D1's srt: deny rules, no sandbox block"
    );

    for bad in ["not json", "[]", r#""text""#] {
        let refused = spawn(&data, None, spawn_args(bad));
        assert!(refused.is_err(), "on: --settings {bad} fails the spawn");
    }
    let mut trailing = plain.clone();
    trailing.truncate(2);
    assert!(
        spawn(&data, None, trailing).is_err(),
        "on: --settings with no value fails"
    );
    for key in ["permissions", "sandbox"] {
        let refused = spawn(&data, None, spawn_args(&format!(r#"{{"{key}":{{}}}}"#)));
        assert!(
            refused.unwrap_err().contains(key),
            "on: an existing {key} key fails"
        );
    }

    let home_text = home.to_str().unwrap();
    let real_root = root.canonicalize().unwrap();
    let real = real_root.to_str().unwrap();
    let default_config = format!("Edit(/{home_text}/.claude/**)");
    assert!(
        deny_of(&on).contains(&default_config),
        "on: CLAUDE_CONFIG_DIR unset edit-denies $HOME/.claude"
    );
    link_to(&root.join("dotfiles/claude"), &home.join(".claude"), true);
    let deny = deny_of(&spawn(&data, None, plain.clone()).unwrap());
    for rule in [default_config, format!("Edit(/{real}/dotfiles/claude/**)")] {
        assert!(
            deny.contains(&rule),
            "on: symlinked ~/.claude denies {rule}"
        );
    }

    let config = root.join("claude-config");
    let config_text = config.to_str().unwrap();
    for (target, link) in [
        ("config-settings.json", config.join("settings.json")),
        ("user-settings.json", home.join(".claude/settings.json")),
        ("project-settings.json", work.join(".claude/settings.json")),
        (
            "local-settings.json",
            work.join(".claude/settings.local.json"),
        ),
    ] {
        link_to(&root.join("dotfiles").join(target), &link, false);
    }
    let deny = deny_of(&spawn(&data, Some(config_text), plain.clone()).unwrap());
    assert!(
        deny.contains(&format!("Edit(/{config_text}/**)")),
        "on: CLAUDE_CONFIG_DIR is edit-denied"
    );
    for target in ["config", "user", "project", "local"] {
        let rule = format!("Edit(/{real}/dotfiles/{target}-settings.json)");
        assert!(
            deny.contains(&rule),
            "on: symlinked {target} settings deny {rule}"
        );
    }
    let root_text = root.to_str().unwrap();
    for bad in [
        "claude-config".to_owned(),
        format!("{root_text}/work/../claude-config"),
        format!("{root_text}/./claude-config"),
    ] {
        let refused = spawn(&data, Some(&bad), plain.clone());
        assert!(
            refused.is_err(),
            "on: CLAUDE_CONFIG_DIR {bad} fails the spawn"
        );
    }

    let login = spawn(&data, None, vec!["auth".into(), "login".into()]).unwrap();
    assert_eq!(
        login[0], "--settings",
        "on: auth login gets --settings first"
    );
    assert_eq!(&login[2..], ["auth", "login"]);

    let unreadable = root.join("com.monocode.file");
    std::fs::write(&unreadable, "").unwrap();
    assert!(
        has_rules(&spawn(&unreadable, None, plain.clone()).unwrap()),
        "a marker that cannot be read counts as on"
    );

    let other = root.join("other.app");
    turn_on(&other);
    assert!(
        spawn(&other, None, plain).is_err(),
        "on: a folder not named com.monocode.* fails"
    );

    std::fs::remove_dir_all(&root).unwrap();
}
