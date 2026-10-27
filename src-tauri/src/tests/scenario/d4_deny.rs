//! D4 (ADE-44) on real app data folders: what `harness_spawn` gets back from
//! `claude_deny::args_in` with the marker absent, present, and unreadable, the
//! Edit rules on the Claude Code config folder and on symlinked settings files,
//! and that every bad input fails the spawn instead of starting Claude Code bare.
//! The second scenario runs the `harness_spawn` command itself with a stand-in
//! `claude`, D4 off, on, and on inside D1's srt. `scripts/scenarios/app.sh d4`
//! proves what the rules stop, with the real CLI.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};
use crate::claude_deny::{args_in, srt_deny_in};
use serde_json::json;

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
        "on: a source the fixed .mcp.json rule matches adds no rule of its own"
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
    // The temp folder real, then as given (on macOS behind `/var`, a link in `/`).
    let real_root = root.canonicalize().unwrap();
    every_link_and_real_path_on_the_list_is_covered(&real_root.join("links-real"), &data);
    every_link_and_real_path_on_the_list_is_covered(&root.join("links"), &data);
    every_command_key_is_read(&root, &data);
    agent_skill_and_command_folders_are_denied(&root, &data);
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

/// How the kernel resolves `path`, worked out apart from `link_chain`: one name
/// at a time from `/`; at a link, its target is spliced in where the link was
/// (after the folders before it if relative) and the walk starts again from
/// `/`. Returns each link it meets outside `/`, spelled where it lives (the
/// folders before it resolved), and the real path.
fn resolve(path: &Path) -> (Vec<String>, String) {
    let name = |part: Component| part.as_os_str().to_owned();
    let mut names: Vec<OsString> = path.components().skip(1).map(name).collect();
    let (mut at, mut i, mut hops, mut links) = (PathBuf::from("/"), 0, 0, Vec::new());
    while i < names.len() {
        if names[i] == ".." {
            at.pop();
            i += 1;
            continue;
        }
        let here = at.join(&names[i]);
        let Ok(target) = here.read_link() else {
            at = here;
            i += 1;
            continue;
        };
        hops += 1;
        assert!(hops <= 40, "a link loop on the way to {path:?}");
        if at != Path::new("/") {
            links.push(here.to_string_lossy().into_owned());
        }
        let kept = if target.is_absolute() { 0 } else { i };
        let spliced = target
            .components()
            .filter(|part| matches!(part, Component::Normal(_) | Component::ParentDir))
            .map(name);
        names = (names[..kept].iter().cloned())
            .chain(spliced)
            .chain(names[i + 1..].iter().cloned())
            .collect();
        (at, i) = (PathBuf::from("/"), 0);
    }
    (links, at.to_string_lossy().into_owned())
}

/// Whether a rule in `deny` matches `path` the way Claude Code reads it: `**`
/// for any names, an exact path otherwise.
fn matched(deny: &[String], path: &str) -> bool {
    let (folders, name) = path.rsplit_once('/').unwrap();
    deny.iter().any(|rule| match rule.as_str() {
        "Edit(//**/.claude/**)" => folders.split('/').any(|folder| folder == ".claude"),
        "Edit(//**/.claude.json)" => name == ".claude.json",
        "Edit(//**/.mcp.json)" => name == ".mcp.json",
        rule => rule
            .strip_prefix("Edit(/")
            .and_then(|rule| rule.strip_suffix(')'))
            .is_some_and(|rule| {
                rule == path
                    || rule
                        .strip_suffix("**")
                        .is_some_and(|dir| path.starts_with(dir))
            }),
    })
}

/// Under `at`, a link at each level: a folder above the config folder, the
/// config folder, its settings file, a folder inside `.claude` that a hook
/// script is reached through (`~/.claude/hooks`), one with a link inside its
/// target (`<work>/.claude/hooks -> ../tools/hooks`, `<work>/tools ->
/// vendor/tools`) and a link of the hook's own folders behind it
/// (`tools/hooks/lib -> ../../shared/lib`), three folder links each inside the
/// one before, a relative link whose `..` climbs past the folder an earlier
/// link points at, a skills folder, and a project settings file through a
/// folder link on the way. Returns HOME, the workdir and the config folder.
fn link_layouts(at: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (home, work) = (at.join("home"), at.join("work"));
    link_to(&at.join("above-real"), &at.join("above"), true);
    link_to(&at.join("config-real"), &at.join("above-real/config"), true);
    std::fs::create_dir_all(at.join("config-real/agents")).unwrap();
    let config = at.join("above/config");
    link_to(
        &at.join("dotfiles/config.json"),
        &config.join("settings.json"),
        false,
    );
    let scripts = "~/.claude/hooks/a.sh; ~/.claude/hooks/up/b.sh; ~/.claude/deep/one/two/c.sh";
    let hook = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": scripts}]}]}});
    std::fs::write(at.join("dotfiles/config.json"), hook.to_string()).unwrap();
    link_to(&at.join("tools/hooks"), &home.join(".claude/hooks"), true);
    std::fs::write(at.join("tools/hooks/a.sh"), "true\n").unwrap();
    std::fs::create_dir_all(at.join("above-real/up-real")).unwrap();
    symlink("../../above/up-real", at.join("tools/hooks/up")).unwrap();
    std::fs::write(at.join("above-real/up-real/b.sh"), "true\n").unwrap();
    for nest in ["nest1", "nest2", "nest3"] {
        std::fs::create_dir_all(home.join(nest)).unwrap();
    }
    symlink("../nest1", home.join(".claude/deep")).unwrap();
    symlink("../nest2", home.join("nest1/one")).unwrap();
    symlink("../nest3", home.join("nest2/two")).unwrap();
    std::fs::write(home.join("nest3/c.sh"), "true\n").unwrap();
    link_to(
        &at.join("shared/skills"),
        &work.join(".claude/skills"),
        true,
    );
    link_to(&at.join("hop-v1"), &at.join("hop"), true);
    let fmt = r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": ".claude/hooks/fmt.sh .claude/hooks/lib/fmt.sh"}]}]}}"#;
    std::fs::write(at.join("hop-v1/settings.json"), fmt).unwrap();
    symlink(
        at.join("hop/settings.json"),
        work.join(".claude/settings.json"),
    )
    .unwrap();
    std::fs::create_dir_all(work.join("vendor/tools/hooks")).unwrap();
    std::fs::create_dir_all(work.join("vendor/shared/lib")).unwrap();
    symlink("vendor/tools", work.join("tools")).unwrap();
    symlink("../tools/hooks", work.join(".claude/hooks")).unwrap();
    symlink("../../shared/lib", work.join("tools/hooks/lib")).unwrap();
    std::fs::write(work.join("vendor/tools/hooks/fmt.sh"), "true\n").unwrap();
    std::fs::write(work.join("vendor/shared/lib/fmt.sh"), "true\n").unwrap();
    (home, work, config)
}

/// What `srt` and the Edit rules `deny` miss of `path` (a folder if `folder`):
/// the path as written, each link `resolve` meets on the way to it, and its
/// real path. srt (macOS) blocks moving each folder of a path it denies as
/// written, so a link at one of those folders needs no entry of its own.
fn link_misses(path: &Path, folder: bool, srt: &[String], deny: &[String]) -> Vec<String> {
    let (links, real) = resolve(path);
    assert_eq!(
        Path::new(&real),
        path.canonicalize().unwrap(),
        "the model resolves {path:?} as the kernel does"
    );
    let pinned = |link: &str| {
        srt.iter()
            .any(|entry| entry.starts_with(&format!("{link}/")))
    };
    let written = path.to_string_lossy().into_owned();
    let unpinned = links.into_iter().filter(|link| !pinned(link));
    let mut misses = Vec::new();
    for link in std::iter::once(written).chain(unpinned) {
        if !srt.contains(&link) {
            misses.push(format!("srt misses {link} on the way to {path:?}"));
        }
        if !matched(deny, &link) {
            misses.push(format!(
                "no Edit rule matches {link} on the way to {path:?}"
            ));
        }
    }
    if !srt.contains(&real) {
        misses.push(format!("srt misses {real}, the real {path:?}"));
    }
    let inside = if folder { format!("{real}/x.md") } else { real };
    if !matched(deny, &inside) {
        misses.push(format!(
            "no Edit rule matches {inside}, in the real {path:?}"
        ));
    }
    misses
}

/// Defect (reviews r3 to r9: one link layout per round): a protected path as
/// written, a link on the way to it, or its real path has no Edit rule or is
/// not on the srt list, so the Write tool writes through it or Bash inside srt
/// retargets it. Each path of `link_layouts` must have no `link_misses`.
fn every_link_and_real_path_on_the_list_is_covered(at: &Path, data: &Path) {
    let (home, work, config) = link_layouts(at);
    let config_dir = Some(config.as_os_str());
    let srt = srt_deny_in(data, config_dir, &home, &work).unwrap();
    let deny = deny_of(&args_in(data, config_dir, &home, &work, false, spawn_args("{}")).unwrap());
    let files = [
        config.join("settings.json"),
        home.join(".claude/hooks/a.sh"),
        home.join(".claude/hooks/up/b.sh"),
        home.join(".claude/deep/one/two/c.sh"),
        work.join(".claude/settings.json"),
        work.join(".claude/hooks/fmt.sh"),
        work.join(".claude/hooks/lib/fmt.sh"),
    ];
    let folders = [config.join("agents"), work.join(".claude/skills")];
    let misses: Vec<String> = (files.iter().map(|file| (file, false)))
        .chain(folders.iter().map(|dir| (dir, true)))
        .flat_map(|(path, folder)| link_misses(path, folder, &srt, &deny))
        .collect();
    assert!(
        misses.is_empty(),
        "on: {}\nsrt: {srt:?}\ndeny: {deny:?}",
        misses.join("\n")
    );
    let config_real = config.canonicalize().unwrap();
    assert!(
        matched(
            &deny,
            &format!("{}/settings.local.json", config_real.display())
        ),
        "on: the real config folder is denied: {deny:?}"
    );
    // The link targets above are spelled through the temp folder, which on
    // macOS is behind a link in `/` (`/var`). Only root can replace one, and a
    // rule on it would deny the whole tree behind it.
    let in_root = |path: &str| Path::new(path).parent() == Some(Path::new("/"));
    assert!(
        !srt.iter().any(|entry| in_root(entry)),
        "on: no link in / for srt: {srt:?}"
    );
    assert!(
        !deny.iter().any(|rule| rule
            .strip_prefix("Edit(/")
            .is_some_and(|rule| in_root(rule.trim_end_matches(')')))),
        "on: no Edit rule on a link in /: {deny:?}"
    );
}

/// A folder link slot of `grid_misses`: no link (a real folder), or a link
/// with an absolute or a relative target.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Folder,
    Absolute,
    Relative,
}

/// Defect (reviews r3 to r10: each round found one more link layout that the
/// hand-built ones missed): a path, link or real path of some layout is not
/// covered. Every layout of the grid, built under the temp folder real and as
/// given (on macOS behind `/var`, a link in `/`): a folder link or none above
/// `.claude` (`work`), in it (`.claude/hooks`) and inside that one's target
/// (`hooks/lib`), each absolute or relative; a hook script spelled with
/// `sub/..` at each place before, between and after them, or with none; and
/// the script spelled without `..` as a second source, or not. Lists every
/// miss of every layout.
#[test]
fn every_generated_link_layout_is_covered() {
    let root = root();
    let data = root.join("com.monocode.desktop");
    turn_on(&data);
    let slots = [Slot::Folder, Slot::Absolute, Slot::Relative];
    let triples = slots.iter().flat_map(|&a| {
        slots
            .iter()
            .flat_map(move |&b| slots.map(move |c| [a, b, c]))
    });
    let (mut misses, mut layouts) = (Vec::new(), 0);
    for (spelled, base) in [
        ("real", root.canonicalize().unwrap()),
        ("given", root.clone()),
    ] {
        for links in triples.clone() {
            for dots in 0..6 {
                for plain in [false, true] {
                    layouts += 1;
                    let at = base.join(format!("grid{layouts}"));
                    let label = format!(
                        "temp {spelled}, links {links:?}, `sub/..` at {dots}, plain source {plain}"
                    );
                    let found = grid_misses(&at, links, dots, plain, &data);
                    misses.extend(found.into_iter().map(|miss| format!("{label}: {miss}")));
                }
            }
        }
    }
    assert!(
        misses.is_empty(),
        "on: {} misses in {layouts} layouts:\n{}",
        misses.len(),
        misses.join("\n")
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// Builds one layout under `at` and returns its `link_misses`. `links` are the
/// slots of `work`, `work/.claude/hooks` and `hooks/lib`; the hook script
/// `work/.claude/hooks/lib/x.sh` is spelled with `sub/..` before name `dots`
/// (1 to 5; 0: none).
fn grid_misses(at: &Path, links: [Slot; 3], dots: usize, plain: bool, data: &Path) -> Vec<String> {
    let (home, work) = (at.join("home"), at.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    // `link` is a real folder, or a link to the new folder `real`.
    let place = |slot: Slot, link: &Path, absolute: &Path, relative: &str, real: &Path| {
        let folder = if matches!(slot, Slot::Folder) {
            link
        } else {
            real
        };
        std::fs::create_dir_all(folder).unwrap();
        match slot {
            Slot::Folder => {}
            Slot::Absolute => symlink(absolute, link).unwrap(),
            Slot::Relative => symlink(relative, link).unwrap(),
        }
        folder.to_owned()
    };
    let real_work = at.join("work-real");
    let real_work = place(links[0], &work, &real_work, "work-real", &real_work);
    let claude = real_work.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let hooks = place(
        links[1],
        &claude.join("hooks"),
        &work.join("tools/hooks"),
        "../tools/hooks",
        &real_work.join("tools/hooks"),
    );
    let lib = place(
        links[2],
        &hooks.join("lib"),
        &work.join("shared/lib"),
        "../../shared/lib",
        &real_work.join("shared/lib"),
    );
    let dirs: [&Path; 5] = [at, &real_work, &claude, &hooks, &lib];
    for dir in dirs {
        std::fs::create_dir_all(dir.join("sub")).unwrap();
    }
    std::fs::write(lib.join("x.sh"), "true\n").unwrap();
    let mut names = vec!["work", ".claude", "hooks", "lib", "x.sh"];
    if dots > 0 {
        names.insert(dots - 1, "sub/..");
    }
    let mut scripts = vec![PathBuf::from(format!(
        "{}/{}",
        at.display(),
        names.join("/")
    ))];
    if plain && dots > 0 {
        scripts.push(work.join(".claude/hooks/lib/x.sh"));
    }
    let command = scripts
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let hook = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": command}]}]}});
    std::fs::write(claude.join("settings.json"), hook.to_string()).unwrap();
    let srt = srt_deny_in(data, None, &home, &work).unwrap();
    let deny = deny_of(&args_in(data, None, &home, &work, false, spawn_args("{}")).unwrap());
    std::iter::once(work.join(".claude/settings.json"))
        .chain(scripts)
        .flat_map(|path| link_misses(&path, false, &srt, &deny))
        .collect()
}

/// Defect (reviews r6 m3, r7 m1): a key in which Claude Code 2.1.309 keeps a
/// command is not read, so srt leaves the `.claude` script it names writable.
/// `CLI_KEYS` is the CLI's own two lists (settings keys, MCP server keys) plus a
/// hook's `command`, by where each sits; `COMMAND_KEYS` must equal it.
fn every_command_key_is_read(root: &Path, data: &Path) {
    const CLI_KEYS: [(&str, &str); 14] = [
        ("settings", "/apiKeyHelper"),
        ("settings", "/awsAuthRefresh"),
        ("settings", "/awsCredentialExport"),
        ("settings", "/fileSuggestion/command"),
        ("settings", "/gcpAuthRefresh"),
        ("settings", "/otelHeadersHelper"),
        ("settings", "/processWrapper"),
        ("settings", "/proxyAuthHelper"),
        ("settings", "/statusLine/command"),
        ("settings", "/subagentStatusLine/command"),
        ("hook", "/command"),
        ("server", "/command"),
        ("server", "/args"),
        ("server", "/headersHelper"),
    ];
    let mut cli: Vec<&str> = CLI_KEYS.iter().map(|(_, key)| *key).collect();
    cli.sort_unstable();
    cli.dedup();
    let mut ours: Vec<&str> = crate::claude_deny::COMMAND_KEYS.split(' ').collect();
    ours.sort_unstable();
    assert_eq!(ours, cli, "COMMAND_KEYS is the CLI's list");

    let (home, work) = (root.join("keys-home"), root.join("keys"));
    std::fs::create_dir_all(work.join(".claude")).unwrap();
    let (mut settings, mut hook, mut server) = (json!({}), json!({"type": "command"}), json!({}));
    let mut scripts = Vec::new();
    for (holder, key) in CLI_KEYS {
        let script = format!(".claude/{holder}{}.sh", key.replace('/', "-"));
        let value = if key == "/args" {
            json!([script])
        } else {
            json!(script)
        };
        let holder = match holder {
            "settings" => &mut settings,
            "hook" => &mut hook,
            _ => &mut server,
        };
        let names: Vec<&str> = key.split('/').skip(1).collect();
        let (last, folders) = names.split_last().unwrap();
        let mut at = holder;
        for name in folders {
            at = at
                .as_object_mut()
                .unwrap()
                .entry(*name)
                .or_insert(json!({}));
        }
        at[*last] = value;
        scripts.push(script);
    }
    settings["hooks"] = json!({"Stop": [{"hooks": [hook]}]});
    std::fs::write(work.join(".claude/settings.json"), settings.to_string()).unwrap();
    let mcp = json!({"mcpServers": {"tools": server}});
    std::fs::write(work.join(".mcp.json"), mcp.to_string()).unwrap();
    let srt = srt_deny_in(data, None, &home, &work).unwrap();
    let work = work.canonicalize().unwrap();
    for script in scripts {
        let script = work.join(script).to_string_lossy().into_owned();
        assert!(
            srt.contains(&script),
            "on: srt denies writes to {script}: {srt:?}"
        );
    }
}

/// Defect (review r7 m2): agent, skill and command files can define hooks, MCP
/// servers and `!` commands, but srt leaves their folders writable.
fn agent_skill_and_command_folders_are_denied(root: &Path, data: &Path) {
    let (home, work, config) = (
        root.join("r7m2-home"),
        root.join("r7m2"),
        root.join("r7m2-config"),
    );
    for dir in [home.join(".claude"), work.join(".claude"), config.clone()] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let srt = srt_deny_in(data, Some(config.as_os_str()), &home, &work).unwrap();
    let real = |path: &Path| path.canonicalize().unwrap().to_string_lossy().into_owned();
    for folder in [
        format!("{}/agents", real(&config)),
        format!("{}/.claude/commands", real(&home)),
        format!("{}/.claude/skills", real(&work)),
    ] {
        assert!(
            srt.contains(&folder),
            "on: srt denies writes to {folder}: {srt:?}"
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
