//! D4 (ADE-44) on real app data folders: what `harness_spawn` gets back from
//! `claude_deny::args_in` with the marker absent, present, and unreadable, and
//! that every bad input fails the spawn instead of starting Claude Code bare.
//! `scripts/scenarios/app.sh d4` proves what the rules stop, with the real CLI.

use std::path::{Path, PathBuf};

use crate::claude_deny::args_in;

fn root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "monocode-scenario-d4-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
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

fn turn_on(data: &Path) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("claude-deny-rules-on"), "").unwrap();
}

#[test]
fn d4_marker_decides_and_bad_input_fails_the_spawn() {
    let root = root();
    let data = root.join("com.monocode.desktop");
    std::fs::create_dir_all(&data).unwrap();
    let plain = spawn_args(r#"{"alwaysThinkingEnabled":true}"#);

    assert_eq!(
        args_in(&data, plain.clone()).unwrap(),
        plain,
        "off: args unchanged"
    );

    turn_on(&data);
    let on = args_in(&data, plain.clone()).unwrap();
    assert!(has_rules(&on), "on: rules added");
    assert_eq!(
        settings_of(&on)["alwaysThinkingEnabled"],
        true,
        "on: other keys kept"
    );

    for bad in ["not json", "[]", r#""text""#] {
        let refused = args_in(&data, spawn_args(bad));
        assert!(refused.is_err(), "on: --settings {bad} fails the spawn");
    }
    let mut trailing = plain.clone();
    trailing.truncate(2);
    assert!(
        args_in(&data, trailing).is_err(),
        "on: --settings with no value fails"
    );
    for key in ["permissions", "sandbox"] {
        let refused = args_in(&data, spawn_args(&format!(r#"{{"{key}":{{}}}}"#)));
        assert!(
            refused.unwrap_err().contains(key),
            "on: an existing {key} key fails"
        );
    }

    let login = args_in(&data, vec!["auth".into(), "login".into()]).unwrap();
    assert_eq!(
        login[0], "--settings",
        "on: auth login gets --settings first"
    );
    assert_eq!(&login[2..], ["auth", "login"]);

    let unreadable = root.join("com.monocode.file");
    std::fs::write(&unreadable, "").unwrap();
    assert!(
        has_rules(&args_in(&unreadable, plain.clone()).unwrap()),
        "a marker that cannot be read counts as on"
    );

    let other = root.join("other.app");
    turn_on(&other);
    assert!(
        args_in(&other, plain).is_err(),
        "on: a folder not named com.monocode.* fails"
    );

    std::fs::remove_dir_all(&root).unwrap();
}
