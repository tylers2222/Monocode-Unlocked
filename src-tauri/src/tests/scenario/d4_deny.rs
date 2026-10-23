//! D4 (ADE-44) on real app data folders: what `harness_spawn` gets back from
//! `claude_deny::args_in` with the marker absent, present, and unreadable, the
//! Edit rules on the Claude Code config folder and on symlinked settings files,
//! and that every bad input fails the spawn instead of starting Claude Code bare.
//! The second scenario runs the `harness_spawn` command itself with a stand-in
//! `claude`, D4 off, on, and on inside D1's srt. `scripts/scenarios/app.sh d4`
//! proves what the rules stop, with the real CLI.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};
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
    std::fs::write(work.join(".mcp.json"), "{}").unwrap();
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
    assert!(
        !deny_of(&on)
            .iter()
            .any(|rule| rule.ends_with("/work/.mcp.json)")),
        "on: a hook source that is not a symlink adds no rule"
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

    an_unreadable_hook_source_fails_the_spawn(&root, &data, &work);
    std::fs::remove_dir_all(&root).unwrap();
}

fn an_unreadable_hook_source_fails_the_spawn(root: &Path, data: &Path, work: &Path) {
    let locked = root.join("locked-home");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads through a 000 folder; only a user the mode stops can see this.
    if std::fs::symlink_metadata(locked.join(".claude"))
        .is_err_and(|e| e.kind() == ErrorKind::PermissionDenied)
    {
        assert!(
            args_in(data, None, &locked, work, false, spawn_args("{}")).is_err(),
            "on: a hook source that cannot be read fails the spawn"
        );
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

const SPAWN_ROOT: &str = "MC_ADE44_SPAWN_ROOT";

/// Defect: `harness_spawn` starts Claude Code without the rules while D4 is on,
/// changes its args while D4 is off, or keeps Claude Code's sandbox block when
/// D1 runs the spawn inside srt. A process of its own, so the PATH that puts
/// the stand-ins first and the temp HOME change no other test.
#[test]
fn harness_spawn_gives_claude_the_rules_and_drops_the_sandbox_block_inside_srt() {
    let root = root();
    let tools = root.canonicalize().unwrap().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    script(
        &tools.join("claude"),
        r#"[ "$1" = --version ] && { echo "2.1.309 (Claude Code)"; exit 0; }
printf '%s\0' "$@" > argv.part && mv argv.part argv"#,
    );
    script(
        &tools.join("srt"),
        r#"[ "$1" = --version ] && { echo 0.0.81; exit 0; }
shift 3
exec "$@""#,
    );
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(tools.clone()).chain(std::env::split_paths(&inherited));
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "scenario::d4_deny::harness_spawn_child",
            "--ignored",
            "--quiet",
        ])
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env("HOME", root.join("home"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env(SPAWN_ROOT, &tools)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "harness_spawn did not give Claude Code the rules the settings say:\n{}",
        String::from_utf8_lossy(&child.stdout)
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
#[ignore = "harness_spawn_gives_claude_the_rules_and_drops_the_sandbox_block_inside_srt runs it in a process of its own"]
fn harness_spawn_child() {
    let tools = PathBuf::from(std::env::var_os(SPAWN_ROOT).unwrap());
    let root = tools.parent().unwrap();
    let claude = tools.join("claude").to_string_lossy().into_owned();
    let data = root.join("com.monocode.d4");
    let app = mock_app(&data);
    std::fs::create_dir_all(&data).unwrap();
    let spawn = |session: &str| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        crate::harness::harness_spawn(
            app.handle().clone(),
            app.state::<crate::harness::HarnessHost>(),
            session.into(),
            claude.clone(),
            spawn_args("{}"),
            workdir.to_string_lossy().into_owned(),
            None,
            Some("claude".into()),
            Some(claude.clone()),
            None,
            None,
        )
        .unwrap();
        let argv = wait_for(&workdir.join("argv"));
        argv.split_terminator('\0')
            .map(String::from)
            .collect::<Vec<_>>()
    };

    assert_eq!(spawn("off"), spawn_args("{}"), "off: args unchanged");
    turn_on(&data);
    assert!(has_rules(&spawn("on")), "on: rules and sandbox block");
    std::fs::write(data.join("agent-sandbox-on"), "").unwrap();
    let in_srt = settings_of(&spawn("in-srt"));
    assert!(
        in_srt["permissions"]["deny"].is_array() && in_srt.get("sandbox").is_none(),
        "inside srt: rules, no sandbox block"
    );
}
