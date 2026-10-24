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
use crate::claude_deny::{args_in, srt_deny_in};

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
    assert!(
        srt_deny_in(&data, None, &home, &work).unwrap().is_empty(),
        "off: nothing for srt to deny"
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
    a_settings_chain_is_denied_at_every_link(&root, &data, &home, &work, config_text);
    a_folder_link_on_the_way_is_denied_to_srt(&root, &data, &home);
    a_script_a_project_hook_runs_is_denied_to_srt(&root, &data, &home);
    a_dangling_settings_link_is_denied_where_it_points(&root, &data, &home);
    a_config_folder_behind_a_folder_link_is_denied_where_it_is(&root, &data, &home, &work);
    a_script_a_helper_or_mcp_server_runs_is_denied_to_srt(&root, &data);
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

/// `link` -> `<dir>/current.json` -> `v1.json` (the "current version" layout),
/// and the two hops after `link`, as the kernel spells them.
fn two_link_chain(link: &Path, dir: &Path) -> [String; 2] {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::fs::write(dir.join("v1.json"), "{}").unwrap();
    symlink("v1.json", dir.join("current.json")).unwrap();
    let _ = std::fs::remove_file(link);
    symlink(dir.join("current.json"), link).unwrap();
    let dir = dir.canonicalize().unwrap();
    ["current.json", "v1.json"].map(|hop| dir.join(hop).to_string_lossy().into_owned())
}

/// Defect: a link in the middle of a settings chain gets no rule, so Bash can
/// point it at a file the agent wrote; or srt, which runs Bash without Claude
/// Code's sandbox, is not told the hook files and their links.
fn a_settings_chain_is_denied_at_every_link(
    root: &Path,
    data: &Path,
    home: &Path,
    work: &Path,
    config: &str,
) {
    let chain = two_link_chain(&work.join(".claude/settings.local.json"), &root.join("cfg"));
    let on = args_in(
        data,
        Some(OsStr::new(config)),
        home,
        work,
        false,
        spawn_args("{}"),
    );
    let deny = deny_of(&on.unwrap());
    for hop in &chain {
        let rule = format!("Edit(/{hop})");
        assert!(
            deny.contains(&rule),
            "on: a two-link settings chain denies {rule}"
        );
    }
    let real = root.canonicalize().unwrap();
    let real = real.to_str().unwrap();
    let srt = srt_deny_in(data, Some(OsStr::new(config)), home, work).unwrap();
    let expected = [
        format!("{real}/claude-config/settings.json"),
        format!("{real}/dotfiles/config-settings.json"),
        format!("{real}/work/.claude/settings.json"),
        format!("{real}/dotfiles/project-settings.json"),
        format!("{real}/work/.claude/settings.local.json"),
        format!("{real}/work/.mcp.json"),
    ];
    for path in expected.iter().chain(&chain) {
        assert!(srt.contains(path), "on: srt denies writes to {path}");
    }
    assert!(
        !srt.iter().any(|path| path.ends_with(".claude.json")),
        "on: srt leaves .claude.json to Claude Code, which writes it"
    );
}

/// Defect (review r5 C1): srt is not told a folder link on the way from a
/// settings link to its target, so Bash inside srt points it at a hook folder.
fn a_folder_link_on_the_way_is_denied_to_srt(root: &Path, data: &Path, home: &Path) {
    let work = root.join("c1");
    link_to(&work.join("cfg-v1"), &work.join("cfg"), true);
    std::fs::write(work.join("cfg-v1/settings.json"), "{}").unwrap();
    std::fs::create_dir_all(work.join(".claude")).unwrap();
    symlink(
        work.join("cfg/settings.json"),
        work.join(".claude/settings.local.json"),
    )
    .unwrap();
    let link = format!("{}/cfg", work.canonicalize().unwrap().display());
    let srt = srt_deny_in(data, None, home, &work).unwrap();
    assert!(
        srt.contains(&link),
        "on: srt denies writes to the folder link {link}: {srt:?}"
    );
}

/// Defect (review r5 E1): a script under `.claude` that the user's project hook
/// runs is not denied to srt, so Bash inside srt rewrites what the hook runs.
fn a_script_a_project_hook_runs_is_denied_to_srt(root: &Path, data: &Path, home: &Path) {
    let work = root.join("e1");
    std::fs::create_dir_all(work.join(".claude/hooks")).unwrap();
    std::fs::write(work.join(".claude/hooks/fmt.sh"), "true\n").unwrap();
    let hook = r#"sh "$CLAUDE_PROJECT_DIR"/.claude/hooks/fmt.sh && echo done"#;
    let settings = serde_json::json!({"hooks": {"PreToolUse": [
        {"matcher": "*", "hooks": [{"type": "command", "command": hook}]}]}});
    std::fs::write(work.join(".claude/settings.json"), settings.to_string()).unwrap();
    let script = format!(
        "{}/.claude/hooks/fmt.sh",
        work.canonicalize().unwrap().display()
    );
    let srt = srt_deny_in(data, None, home, &work).unwrap();
    assert!(
        srt.contains(&script),
        "on: srt denies writes to {script}: {srt:?}"
    );
}

/// Defect (review r5 G4): a settings link to a file not there yet is denied as
/// `.claude/../gen/settings.json`, which matches nothing, so the Write tool
/// creates the target with a hook in it.
fn a_dangling_settings_link_is_denied_where_it_points(root: &Path, data: &Path, home: &Path) {
    let work = root.join("g4");
    std::fs::create_dir_all(work.join(".claude")).unwrap();
    symlink(
        "../gen/settings.json",
        work.join(".claude/settings.local.json"),
    )
    .unwrap();
    let rule = format!(
        "Edit(/{}/gen/settings.json)",
        work.canonicalize().unwrap().display()
    );
    let deny = deny_of(&args_in(data, None, home, &work, false, spawn_args("{}")).unwrap());
    assert!(
        deny.contains(&rule),
        "on: a dangling settings link denies {rule}: {deny:?}"
    );
}

/// Defect (review r6 m2): a config folder reached through a folder link gets an
/// Edit rule only as written, so the Write tool plants a hook through its real
/// path.
fn a_config_folder_behind_a_folder_link_is_denied_where_it_is(
    root: &Path,
    data: &Path,
    home: &Path,
    work: &Path,
) {
    link_to(&root.join("m2-real"), &root.join("m2-link"), true);
    let config = root.join("m2-link/claude-config");
    std::fs::create_dir_all(&config).unwrap();
    let rule = format!(
        "Edit(/{}/m2-real/claude-config/**)",
        root.canonicalize().unwrap().display()
    );
    let on = args_in(
        data,
        Some(config.as_os_str()),
        home,
        work,
        false,
        spawn_args("{}"),
    );
    let deny = deny_of(&on.unwrap());
    assert!(
        deny.contains(&rule),
        "on: a config folder behind a folder link denies {rule}: {deny:?}"
    );
}

/// Defect (review r6 m3): a script under `.claude` that a helper key or an MCP
/// server names is not denied to srt, so Bash inside srt rewrites what the next
/// spawn runs.
fn a_script_a_helper_or_mcp_server_runs_is_denied_to_srt(root: &Path, data: &Path) {
    let (home, work) = (root.join("m3-home"), root.join("m3"));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let settings = serde_json::json!({"apiKeyHelper": "~/.claude/key.sh",
        "statusLine": {"type": "command", "command": "sh .claude/status.sh"}});
    std::fs::write(home.join(".claude/settings.json"), settings.to_string()).unwrap();
    let mcp = serde_json::json!({"mcpServers": {"tools": {"command": "node",
        "args": ["${CLAUDE_PROJECT_DIR}/.claude/mcp/server.js"]}}});
    std::fs::write(work.join(".mcp.json"), mcp.to_string()).unwrap();
    let srt = srt_deny_in(data, None, &home, &work).unwrap();
    let [home, work] = [home, work].map(|dir| dir.canonicalize().unwrap());
    for script in [
        home.join(".claude/key.sh"),
        work.join(".claude/status.sh"),
        work.join(".claude/mcp/server.js"),
    ] {
        let script = script.to_string_lossy().into_owned();
        assert!(
            srt.contains(&script),
            "on: srt denies writes to {script}: {srt:?}"
        );
    }
}

const SPAWN_ROOT: &str = "MC_ADE44_SPAWN_ROOT";

/// Defect: `harness_spawn` starts Claude Code without the rules while D4 is on,
/// changes its args while D4 is off, keeps Claude Code's sandbox block when D1
/// runs the spawn inside srt, or does not give srt the hook files there. A
/// process of its own, so the PATH that puts the stand-ins first and the temp
/// HOME change no other test.
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
printf '%s' "$2" > srt-settings
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
    let spawn = |session: &str, account: Option<crate::harness::HarnessAccount>| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        crate::harness::harness_spawn(
            app.handle().clone(),
            app.state::<crate::harness::HarnessHost>(),
            session.into(),
            claude.clone(),
            spawn_args("{}"),
            workdir.to_string_lossy().into_owned(),
            account,
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

    assert_eq!(spawn("off", None), spawn_args("{}"), "off: args unchanged");
    turn_on(&data);
    assert!(has_rules(&spawn("on", None)), "on: rules and sandbox block");
    std::fs::write(data.join("agent-sandbox-on"), "").unwrap();
    let work = root.join("in-srt");
    let chain = two_link_chain(&work.join(".claude/settings.local.json"), &work.join("cfg"));
    let in_srt = settings_of(&spawn("in-srt", None));
    assert!(
        in_srt["permissions"]["deny"].is_array() && in_srt.get("sandbox").is_none(),
        "inside srt: rules, no sandbox block"
    );
    spawn("in-srt-later", None);
    let given = std::fs::read_to_string(work.join("srt-settings")).unwrap();
    let srt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(given).unwrap()).unwrap();
    let deny: Vec<String> = serde_json::from_value(srt["filesystem"]["denyWrite"].clone()).unwrap();
    let hooks = ["settings.json", "settings.local.json"].map(|file| {
        work.join(".claude")
            .join(file)
            .to_string_lossy()
            .into_owned()
    });
    for path in hooks.iter().chain(&chain) {
        assert!(
            deny.contains(path),
            "inside srt: srt denies writes to {path}, also after a later spawn wrote its own settings"
        );
    }
    a_claude_account_spawn_is_denied_its_own_config(root, &data, &spawn);
}

/// Defect (review r6 m1): a spawn on a Claude account gets the rules of
/// `$HOME/.claude`, not of the account's config folder, so the account's
/// symlinked settings and the script its hook runs stay writable.
fn a_claude_account_spawn_is_denied_its_own_config(
    root: &Path,
    data: &Path,
    spawn: &dyn Fn(&str, Option<crate::harness::HarnessAccount>) -> Vec<String>,
) {
    let dotfile = root.join("dotfiles/account-settings.json");
    let config = data.join("provider-accounts/claude/work-b");
    link_to(&dotfile, &config.join("settings.json"), false);
    let hook = "sh ~/.claude/hooks/account.sh";
    let settings = serde_json::json!({"hooks": {"Stop": [
        {"hooks": [{"type": "command", "command": hook}]}]}});
    std::fs::write(&dotfile, settings.to_string()).unwrap();
    let account = serde_json::from_value(serde_json::json!({"provider": "claude", "id": "work-b"}));
    let deny = deny_of(&spawn("account", Some(account.unwrap())));
    let rule = format!("Edit(/{})", dotfile.display());
    assert!(
        deny.contains(&rule),
        "account: the account's symlinked settings deny {rule}: {deny:?}"
    );
    let given = std::fs::read_to_string(root.join("account/srt-settings")).unwrap();
    let srt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(given).unwrap()).unwrap();
    let srt: Vec<String> = serde_json::from_value(srt["filesystem"]["denyWrite"].clone()).unwrap();
    for path in [dotfile, root.join("home/.claude/hooks/account.sh")] {
        let path = path.to_string_lossy().into_owned();
        assert!(
            srt.contains(&path),
            "account: srt denies writes to {path}: {srt:?}"
        );
    }
}
