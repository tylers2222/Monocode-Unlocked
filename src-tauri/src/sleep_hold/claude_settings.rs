//! Which of Claude Code's settings sources turn the sleep hold's command
//! hook off for a spawn (lead ruling P2 r1 M3): `disableAllHooks` in any
//! source it loads drops every command hook, ours too.

use std::path::Path;

use serde_json::{Map, Value};

/// Claude Code's managed settings, which outrank every other source.
#[cfg(target_os = "macos")]
pub(super) const MANAGED: &str = "/Library/Application Support/ClaudeCode/managed-settings.json";
#[cfg(not(target_os = "macos"))]
pub(super) const MANAGED: &str = "/etc/claude-code/managed-settings.json";

/// The folders Claude Code reads its settings from for a spawn.
pub(crate) struct ClaudeFiles<'a> {
    /// The user's config folder (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
    pub(crate) config: &'a Path,
    /// The spawn's working folder, whose `.claude` is the project's.
    pub(crate) workdir: &'a Path,
    pub(crate) managed: &'a Path,
}

/// The first settings source the spawn loads that turns every hook off, or
/// (managed) allows only its own: the spawn's `--settings` object `flag`,
/// then the files. A file that is absent or not JSON turns nothing off:
/// Claude Code skips it too.
pub(super) fn hooks_off(
    flag: &Map<String, Value>,
    files: &ClaudeFiles,
    args: &[String],
) -> Option<String> {
    let off = |settings: &Value| settings.get("disableAllHooks") == Some(&Value::Bool(true));
    if flag.get("disableAllHooks") == Some(&Value::Bool(true)) {
        return Some("--settings".into());
    }
    let read = |path: &Path| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    if let Some(managed) = read(files.managed) {
        if off(&managed) || managed.get("allowManagedHooksOnly") == Some(&Value::Bool(true)) {
            return Some(format!("managed settings {}", files.managed.display()));
        }
    }
    let project = files.workdir.join(".claude");
    let sources = [
        ("user", files.config.join("settings.json")),
        ("project", project.join("settings.json")),
        ("local", project.join("settings.local.json")),
    ];
    let loaded = setting_sources(args);
    sources
        .into_iter()
        .filter(|(name, _)| loaded.as_ref().is_none_or(|loaded| loaded.contains(name)))
        .find(|(_, path)| read(path).is_some_and(|settings| off(&settings)))
        .map(|(name, path)| format!("{name} settings {}", path.display()))
}

/// The sources `--setting-sources` names (`None`: the flag is absent, so all).
fn setting_sources(args: &[String]) -> Option<Vec<&str>> {
    let value = args.iter().enumerate().find_map(|(at, arg)| {
        arg.strip_prefix("--setting-sources=").or_else(|| {
            (arg == "--setting-sources")
                .then(|| args.get(at + 1).map(String::as_str))
                .flatten()
        })
    })?;
    Some(value.split(',').map(str::trim).collect())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn folder() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("monocode-claude-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn files(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        (
            dir.join("config"),
            dir.join("work"),
            dir.join("managed.json"),
        )
    }

    fn off_in(dir: &Path, args: &[&str]) -> Option<String> {
        let (config, workdir, managed) = files(dir);
        let files = ClaudeFiles {
            config: &config,
            workdir: &workdir,
            managed: &managed,
        };
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        hooks_off(&Map::new(), &files, &args)
    }

    // Defect: `disableAllHooks` in a settings file the session loads (the
    // user's, the project's, its local one, or managed policy) drops the
    // hook, and the hold reads as installed.
    #[test]
    fn hooks_turned_off_in_any_source_the_session_loads_are_found() {
        let off = r#"{"disableAllHooks":true}"#;
        let only_managed = r#"{"allowManagedHooksOnly":true}"#;
        let cases = [
            ("user", "config/settings.json", off),
            ("project", "work/.claude/settings.json", off),
            ("local", "work/.claude/settings.local.json", off),
            ("managed", "managed.json", off),
            ("managed", "managed.json", only_managed),
        ];
        for (source, file, text) in cases {
            let dir = folder();
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                off_in(&dir, &[]),
                Some(format!("{source} settings {}", path.display())),
                "{file}: {text}"
            );
        }
    }

    // Defect: a file the session does not load (a source --setting-sources
    // leaves out, settings that are not JSON, hooks left on) reads as
    // turning hooks off, so a held session alarms as unheld.
    #[test]
    fn sources_the_session_does_not_load_turn_nothing_off() {
        let dir = folder();
        let project = dir.join("work/.claude");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(project.join("settings.json"), r#"{"disableAllHooks":true}"#).unwrap();
        std::fs::write(project.join("settings.local.json"), "not json").unwrap();
        let user_on = r#"{"disableAllHooks":false}"#;
        std::fs::write(dir.join("config/settings.json"), user_on).unwrap();
        for flag in [
            &["--setting-sources=user,local"][..],
            &["--setting-sources", "user"],
        ] {
            assert_eq!(off_in(&dir, flag), None, "{flag:?}");
        }
        assert!(off_in(&dir, &[]).is_some(), "every source loads");
    }
}
