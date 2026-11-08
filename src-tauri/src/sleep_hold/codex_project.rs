//! Codex also loads `<dir>/.codex/config.toml` for each folder from the
//! project root down to the session's cwd, and a trusted one can turn every
//! hook off, the hold's too. Trust and layers as codex-cli 0.165.0 resolves
//! them (app-server `config/read` and `hooks/list` probes):
//! - the project root is the nearest folder holding `.git` (a folder or a
//!   worktree's file); outside a repo only the cwd's own `.codex` loads;
//! - a layer in `dir` takes the first `trust_level` of `projects."<dir>"`,
//!   `projects."<root>"`, then, in a worktree, `projects."<main repo>"`;
//! - a key matches a folder's path as given or canonical, exactly; a layer
//!   with no trust is disabled.

use std::path::{Path, PathBuf};

use super::codex_home::hooks_off;

/// Why the hold is missing in a session in `cwd` whose user config is
/// `user`: a project layer Codex loads turns hooks off, or may (a layer it
/// cannot read, or whose trust cannot be resolved). `None` when none does.
pub(super) fn hooks_off_in(user: &toml::Table, cwd: &Path) -> Option<String> {
    let root = cwd.ancestors().find(|dir| dir.join(".git").exists());
    let main = root.and_then(main_repo);
    let projects = user.get("projects").and_then(toml::Value::as_table);
    let layers = match root {
        Some(root) => cwd
            .ancestors()
            .take_while(|dir| dir.starts_with(root))
            .collect(),
        None => vec![cwd],
    };
    for dir in layers {
        let file = dir.join(".codex").join("config.toml");
        let off = match std::fs::read_to_string(&file) {
            Ok(text) => text
                .parse::<toml::Table>()
                .map(|config| hooks_off(&config))
                .map_err(|e| format!("{} is not TOML: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => Err(format!("{}: {e}", file.display())),
        };
        if off == Ok(false) {
            continue;
        }
        let keys = [
            Some(Ok(dir)),
            root.map(Ok),
            main.as_ref()
                .map(|main| main.as_deref().map_err(String::as_str)),
        ];
        let trusted = trusted(projects, keys.into_iter().flatten());
        if trusted == Ok(false) {
            continue;
        }
        let reason = off
            .err()
            .unwrap_or_else(|| format!("codex hooks disabled in {}", file.display()));
        return Some(match trusted {
            Err(why) => format!("{reason}, if its project is trusted: {why}"),
            Ok(_) => reason,
        });
    }
    None
}

/// Whether the first `trust_level` set for one of `keys`, in order, is
/// `trusted`; an error when a key needed cannot be resolved or read.
fn trusted<'a>(
    projects: Option<&toml::Table>,
    keys: impl Iterator<Item = Result<&'a Path, &'a str>>,
) -> Result<bool, String> {
    for key in keys {
        let key = key.map_err(str::to_owned)?;
        let canonical = key.canonicalize().ok();
        let level = [Some(key), canonical.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|path| {
                projects?
                    .get(path.to_string_lossy().as_ref())?
                    .get("trust_level")
            });
        match level.map(toml::Value::as_str) {
            None => {}
            Some(Some("trusted")) => return Ok(true),
            Some(Some("untrusted")) => return Ok(false),
            Some(_) => {
                return Err(format!(
                    "the trust_level of {} is not trusted or untrusted",
                    key.display()
                ))
            }
        }
    }
    Ok(false)
}

/// The main repo of the worktree at `root`, from its `.git` file's gitdir
/// and that gitdir's `commondir`. `None` when `root` is no worktree.
fn main_repo(root: &Path) -> Option<Result<PathBuf, String>> {
    let marker = root.join(".git");
    if marker.is_dir() {
        return None;
    }
    let unknown = || {
        format!(
            "the main repo of the worktree {} is unknown",
            root.display()
        )
    };
    let gitdir = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|text| Some(root.join(text.trim().strip_prefix("gitdir:")?.trim())));
    let common = gitdir.and_then(|gitdir| {
        let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        gitdir.join(common.trim()).canonicalize().ok()
    });
    Some(
        common
            .filter(|common| common.file_name() == Some(".git".as_ref()))
            .and_then(|common| common.parent().map(Path::to_path_buf))
            .ok_or_else(unknown),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A folder holding the repo `proj`, its worktree `wt`, and `plain`,
    /// which is in no repo.
    fn repos() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("monocode-codex-project-{}", uuid::Uuid::new_v4()))
            .join("x");
        let gitdir = dir.join("proj/.git/worktrees/wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(dir.join("wt")).unwrap();
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let gitdir = dir.join("proj/.git/worktrees/wt");
        std::fs::write(
            dir.join("wt/.git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        dir
    }

    fn user(dir: &Path, trust: &[(&str, &str)]) -> toml::Table {
        let mut projects = toml::Table::new();
        for (path, level) in trust {
            let mut project = toml::Table::new();
            project.insert("trust_level".into(), (*level).into());
            let path = if *path == "." {
                dir.to_path_buf()
            } else {
                dir.join(path)
            };
            projects.insert(path.display().to_string(), project.into());
        }
        let mut user = toml::Table::new();
        user.insert("projects".into(), projects.into());
        user
    }

    fn layer(dir: &Path, text: &str) {
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(dir.join(".codex/config.toml"), text).unwrap();
    }

    // Defect: the hold reads a project layer Codex does not load (and is
    // missing for nothing), or skips one it loads (and reads as installed
    // with no hook running). Each case is a codex-cli 0.165.0 app-server
    // probe: which trust key decides a layer, and which folders have one.
    #[test]
    fn project_layers_count_as_codex_loads_them() {
        let off = "[features]\nhooks = false\n";
        for (trust, at, cwd, want_off) in [
            (&[("proj", "trusted")][..], "proj/sub", "proj/sub", true),
            (
                &[("proj", "trusted"), ("proj/sub", "untrusted")],
                "proj/sub",
                "proj/sub",
                false,
            ),
            (&[("proj/sub", "trusted")], "proj", "proj/sub", false),
            (&[("proj/a", "trusted")], "proj/a/b", "proj/a/b", false),
            (&[("proj/a", "trusted")], "proj/a", "proj/a/b", true),
            (&[("proj/sub", "trusted")], "proj", "proj/sub/deep", false),
            (&[("wt/sub", "trusted")], "wt", "wt/sub", false),
            (&[("proj", "trusted")], "wt/sub", "wt/sub", true),
            (
                &[("proj", "trusted"), ("wt", "untrusted")],
                "wt",
                "wt",
                false,
            ),
            (
                &[("proj", "untrusted"), ("wt", "trusted")],
                "wt",
                "wt",
                true,
            ),
            (&[("proj", "trusted")], "proj", "wt", false),
            (&[("plain/a", "trusted")], "plain/a", "plain/a/b", false),
            (&[("plain/a", "trusted")], "plain/a/b", "plain/a/b", false),
            (&[("plain/a/b", "trusted")], "plain/a/b", "plain/a/b", true),
            (&[(".", "trusted")], "proj", "proj", false),
        ] {
            let dir = repos();
            std::fs::create_dir_all(dir.join(cwd)).unwrap();
            layer(&dir.join(at), off);
            let got = hooks_off_in(&user(&dir, trust), &dir.join(cwd));
            let want = want_off.then(|| {
                format!(
                    "codex hooks disabled in {}",
                    dir.join(at).join(".codex/config.toml").display()
                )
            });
            assert_eq!(got, want, "{trust:?} layer {at} cwd {cwd}");
        }
    }

    // Defect: a trust key written through a symlink is matched (or missed)
    // unlike Codex, which matches the path as the cwd gives it or its
    // canonical form, but not a key that only canonicalizes to the cwd.
    #[test]
    fn a_trust_key_matches_the_cwd_as_given_or_canonical() {
        let dir = repos();
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("proj"), &link).unwrap();
        layer(&dir.join("proj"), "[features]\nhooks = false\n");
        for (key, cwd, want_off) in [
            ("link", "link", true),
            ("proj", "link", true),
            ("link", "proj", false),
        ] {
            let got = hooks_off_in(&user(&dir, &[(key, "trusted")]), &dir.join(cwd));
            assert_eq!(got.is_some(), want_off, "key {key} cwd {cwd}: {got:?}");
        }
    }

    // Defect: a layer whose hooks setting or trust cannot be read is taken
    // as harmless, so the hold reads as installed when Codex may run no hook.
    #[test]
    fn a_layer_that_cannot_be_ruled_out_leaves_the_hold_missing() {
        let dir = repos();
        layer(&dir.join("proj"), "[features]\ncodex_hooks = false\n");
        let trusted = user(&dir, &[("proj", "trusted")]);
        assert!(
            hooks_off_in(&trusted, &dir.join("proj")).is_some(),
            "legacy name"
        );

        layer(&dir.join("proj"), "features = [");
        let got = hooks_off_in(&trusted, &dir.join("proj")).unwrap();
        assert!(got.contains("is not TOML"), "{got}");

        layer(&dir.join("proj"), "[features]\nhooks = false\n");
        let bogus = user(&dir, &[("proj", "bogus")]);
        let got = hooks_off_in(&bogus, &dir.join("proj")).unwrap();
        assert!(got.contains("if its project is trusted"), "{got}");

        std::fs::remove_dir_all(dir.join("proj/.git/worktrees")).unwrap();
        layer(&dir.join("wt"), "[features]\nhooks = false\n");
        let got = hooks_off_in(&trusted, &dir.join("wt")).unwrap();
        assert!(got.contains("main repo of the worktree"), "{got}");
        let wt = user(&dir, &[("wt", "untrusted")]);
        assert_eq!(
            hooks_off_in(&wt, &dir.join("wt")),
            None,
            "its own key decides"
        );
    }
}
