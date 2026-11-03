//! ADE-224 r1 M1: a Mono's private Codex home is in its spawn's srt
//! `allowWrite`, so a sandboxed agent may plant links in it, and the app copies a
//! thread into it outside srt (`codex_mono_store_copy`). The app must refuse
//! each link where it writes, never follow it, and leave its target as it was.

use std::path::{Path, PathBuf};

use crate::codex_mono_store::copy_rollouts;

const ROLLOUT: &str = "rollout-2026-10-08T10-00-00-root.jsonl";

/// A login with one saved thread, a Mono home, and a folder outside both
/// that holds one precious file.
struct Scratch {
    root: PathBuf,
    source: PathBuf,
    home: PathBuf,
    outside: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ade224-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let (source, home, outside) = (root.join("login"), root.join("home"), root.join("outside"));
        let day = source.join("sessions/2026/10/08");
        for dir in [&day, &home, &outside] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let meta = r#"{"type":"session_meta","payload":{"id":"root"}}"#;
        std::fs::write(day.join(ROLLOUT), format!("{meta}\n")).unwrap();
        std::fs::write(outside.join("precious"), "precious").unwrap();
        Scratch {
            root,
            source,
            home,
            outside,
        }
    }

    /// A link at `at` in the home to `to`.
    fn plant(&self, at: &str, to: &Path) -> PathBuf {
        let link = self.home.join(at);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(to, &link).unwrap();
        link
    }

    fn copy(&self) -> Result<(), String> {
        let rollout = self.source.join("sessions/2026/10/08").join(ROLLOUT);
        copy_rollouts(&self.source, &self.home, "root", &[rollout])
    }

    /// Every entry outside the home, with its content.
    fn outside(&self) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = std::fs::read_dir(&self.outside)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(&path).unwrap_or_default())
            })
            .collect();
        entries.sort();
        entries
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Defect: copying a thread into a Mono home writes through a link planted
/// there, to a file (the pending marker, the rollout, the agent snapshot) or
/// to a folder (the markers' folder, a sessions folder), so a file outside the
/// home changes or appears, or the copy succeeds without naming the link.
#[test]
fn a_copy_into_a_mono_home_refuses_every_planted_link() {
    let untouched = vec![("precious".to_owned(), "precious".to_owned())];
    let clean = Scratch::new();
    clean.copy().unwrap();
    assert!(clean
        .home
        .join("sessions/2026/10/08")
        .join(ROLLOUT)
        .is_file());

    let files = [
        ".monocode-migrations/root".to_owned(),
        format!("sessions/2026/10/08/{ROLLOUT}"),
        ".monocode-agent-state/root.json".to_owned(),
    ];
    for at in &files {
        let scratch = Scratch::new();
        let link = scratch.plant(at, &scratch.outside.join("precious"));
        let error = scratch.copy().expect_err(at);
        assert!(error.contains(&link.display().to_string()), "{at}: {error}");
        assert_eq!(
            scratch.outside(),
            untouched,
            "{at}: the copy wrote through the link"
        );
        assert!(link.is_symlink(), "{at}: the link went");
    }
    for at in [".monocode-migrations", "sessions/2026"] {
        let scratch = Scratch::new();
        let link = scratch.plant(at, &scratch.outside);
        let error = scratch.copy().expect_err(at);
        assert!(error.contains(&link.display().to_string()), "{at}: {error}");
        assert_eq!(
            scratch.outside(),
            untouched,
            "{at}: the copy wrote through the link"
        );
    }
}
