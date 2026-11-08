//! Codex app-server runs only a trusted hook (codex-cli 0.165.0): a `-c`
//! hook stays untrusted there, and `--dangerously-bypass-hook-trust` does
//! nothing. So the hold goes in a CODEX_HOME of its own, beside the spawn's
//! home: every entry of that home linked, but `config.toml`, which is the
//! home's own config with the hook and the trust record Codex keeps for it.
//! The user's and the Mono store's own files are never written.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

const CONFIG: &str = "config.toml";

/// The overlay of the Codex home `real` in `root`, holding the `Bash`
/// PreToolUse hook `command` (timeout `timeout_s`) as trusted. One overlay
/// per home: a second spawn on it relinks and rewrites the same folder.
pub(crate) fn overlay(
    real: &Path,
    root: &Path,
    command: &str,
    timeout_s: u64,
) -> Result<PathBuf, String> {
    let mut config = match std::fs::read_to_string(real.join(CONFIG)) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|e| format!("Codex's {CONFIG} is not TOML: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(format!("{}: {e}", real.join(CONFIG).display())),
    };
    let mut name = DefaultHasher::new();
    real.canonicalize()
        .unwrap_or_else(|_| real.to_path_buf())
        .hash(&mut name);
    let dir = root.join(format!("{:016x}", name.finish()));
    crate::mono_home::Dir::create(&dir, &[])?;
    // Codex keys a hook's trust by its file's path as it resolves it.
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    trust(&mut config, &dir.join(CONFIG), command, timeout_s)?;
    link_entries(real, &dir)?;
    super::install::write_if_changed(
        &dir.join(CONFIG),
        &toml::to_string(&config).map_err(|e| e.to_string())?,
    )?;
    Ok(dir)
}

/// `config` with the hook appended to its `hooks.PreToolUse` groups and
/// marked trusted under the key Codex gives it in the file at `path`.
fn trust(
    config: &mut toml::Table,
    path: &Path,
    command: &str,
    timeout_s: u64,
) -> Result<(), String> {
    let hooks = table(config, "hooks")?;
    let groups = match hooks
        .entry("PreToolUse")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
    {
        toml::Value::Array(groups) => groups,
        _ => return Err("Codex's hooks.PreToolUse is not a list".into()),
    };
    let key = format!("{}:pre_tool_use:{}:0", path.display(), groups.len());
    groups.push(
        toml::Value::try_from(json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": command, "timeout": timeout_s}],
        }))
        .map_err(|e| e.to_string())?,
    );
    let mut state = toml::Table::new();
    state.insert("trusted_hash".into(), trust_hash(command, timeout_s).into());
    table(hooks, "state")?.insert(key, state.into());
    Ok(())
}

fn table<'a>(parent: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table, String> {
    match parent
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
    {
        toml::Value::Table(table) => Ok(table),
        _ => Err(format!("Codex's {key} is not a table")),
    }
}

/// Codex's `currentHash` of a `Bash` PreToolUse command hook: the SHA-256 of
/// its normalized identity as JSON with sorted keys (codex-rs hooks
/// `hook_hash`, codex-cli 0.165.0).
fn trust_hash(command: &str, timeout_s: u64) -> String {
    let mut identity = json!({
        "event_name": "pre_tool_use",
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": command, "timeout": timeout_s, "async": false}],
    });
    identity.sort_all_objects();
    let digest = Sha256::digest(identity.to_string());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// `dir` links each entry of `real` but its config. A link whose entry is
/// gone is dropped; a real file Codex made in `dir` is kept.
fn link_entries(real: &Path, dir: &Path) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(real) {
        for entry in entries {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            if name == CONFIG {
                continue;
            }
            link(&real.join(&name), &dir.join(&name))?;
            names.insert(name);
        }
    }
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let stale = !names.contains(&entry.file_name())
            && entry.file_type().is_ok_and(|kind| kind.is_symlink());
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn link(source: &Path, target: &Path) -> Result<(), String> {
    match std::fs::read_link(target) {
        Ok(at) if at == source => return Ok(()),
        Ok(_) => std::fs::remove_file(target).map_err(|e| format!("{}: {e}", target.display()))?,
        // Codex made it here; it stays.
        Err(_) if std::fs::symlink_metadata(target).is_ok() => return Ok(()),
        Err(_) => {}
    }
    #[cfg(unix)]
    return std::os::unix::fs::symlink(source, target)
        .map_err(|e| format!("{}: {e}", target.display()));
    #[cfg(not(unix))]
    Err("the sleep hold's Codex home needs symbolic links (macOS, Linux)".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn folder() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("monocode-codex-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn config_of(dir: &Path) -> toml::Table {
        std::fs::read_to_string(dir.join(CONFIG))
            .unwrap()
            .parse()
            .unwrap()
    }

    // Defect: the trust record holds a hash Codex does not compute for the
    // hook, so app-server lists it untrusted and never runs it. The values
    // are codex-cli 0.165.0's `hooks/list` currentHash for each hook.
    #[test]
    fn the_trust_hash_is_the_one_codex_computes() {
        assert_eq!(
            trust_hash("/tmp/fix-p2/codex/s/hook.sh", 30),
            "sha256:55db17f37a14f9be788586ef0ee7766982083aef6b8246f577509d2dfa9c849f"
        );
    }

    // Defect: the overlay drops the user's settings or hooks, keys the trust
    // record to another file or group, or writes the user's own config.
    #[test]
    fn the_overlay_keeps_the_home_config_and_trusts_only_the_hold_hook() {
        let base = folder();
        let real = base.join("codex");
        std::fs::create_dir_all(real.join("sessions")).unwrap();
        std::fs::write(real.join("auth.json"), "{}").unwrap();
        let user = "model = \"gpt-5\"\n\n[[hooks.PreToolUse]]\nmatcher = \"Edit\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"lint\"\n";
        std::fs::write(real.join(CONFIG), user).unwrap();

        let dir = overlay(
            &real,
            &base.join("overlays"),
            "'/x y/monocode' hold --harness codex",
            150,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(real.join(CONFIG)).unwrap(), user);
        let config = config_of(&dir);
        assert_eq!(config["model"].as_str(), Some("gpt-5"));
        let groups = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["matcher"].as_str(), Some("Edit"));
        assert_eq!(
            groups[1]["hooks"][0]["command"].as_str(),
            Some("'/x y/monocode' hold --harness codex")
        );
        let state = config["hooks"]["state"].as_table().unwrap();
        let key = format!("{}:pre_tool_use:1:0", dir.join(CONFIG).display());
        assert_eq!(state.keys().collect::<Vec<_>>(), [&key]);
        assert_eq!(
            state[&key]["trusted_hash"].as_str(),
            Some(trust_hash("'/x y/monocode' hold --harness codex", 150).as_str())
        );
        for name in ["sessions", "auth.json"] {
            assert_eq!(std::fs::read_link(dir.join(name)).unwrap(), real.join(name));
        }
    }

    // Defect: a second spawn adds a second hook (each call asks twice), or
    // keeps a link to an entry the home no longer has.
    #[test]
    fn a_second_overlay_of_one_home_is_the_same_overlay() {
        let base = folder();
        let real = base.join("codex");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("history.jsonl"), "").unwrap();
        let root = base.join("overlays");
        let once = overlay(&real, &root, "hold", 150).unwrap();
        let first = config_of(&once);
        std::fs::remove_file(real.join("history.jsonl")).unwrap();
        std::fs::write(once.join("models_cache.json"), "{}").unwrap();

        let twice = overlay(&real, &root, "hold", 150).unwrap();

        assert_eq!(twice, once);
        assert_eq!(config_of(&twice), first);
        assert!(std::fs::symlink_metadata(twice.join("history.jsonl")).is_err());
        assert!(
            twice.join("models_cache.json").is_file(),
            "Codex's own file is kept"
        );
    }

    // Defect: a home whose config the overlay cannot extend still reads as
    // held, so its sessions run unheld with the row saying installed.
    #[test]
    fn a_config_the_hook_cannot_join_is_an_error() {
        for user in [
            "model = ",
            "hooks = 1",
            "[hooks]\nPreToolUse = 1",
            "[hooks]\nstate = 1",
        ] {
            let base = folder();
            let real = base.join("codex");
            std::fs::create_dir_all(&real).unwrap();
            std::fs::write(real.join(CONFIG), user).unwrap();
            assert!(
                overlay(&real, &base.join("o"), "hold", 150).is_err(),
                "{user}"
            );
        }
    }
}
