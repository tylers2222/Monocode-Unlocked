//! ADE-263, T18: the Worktrees commands as the page invokes them, from a window
//! of one Space, on a temp repository and an in-memory store.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusqlite::params;
use tauri::{Listener, Manager};

use super::space_checkouts::{git_project, temp_root};
use crate::session_store::SessionStore;
use crate::spaces::{self, projects, SettingsPatch, FIRST_SPACE};
use crate::worktrees::{self, OtherSpaceSession};

const TWO: &str = "space-2";

/// A temp repository with a worktree `copy`, and a mock app on a store whose
/// sessions `one-a` (Space 1) and `two-a` (Space 2) work in it. The change event
/// counts into the returned counter.
struct Rig {
    root: PathBuf,
    cwd: String,
    path: String,
    worktree: PathBuf,
    app: tauri::App<tauri::test::MockRuntime>,
    heard: Arc<AtomicUsize>,
}

impl Rig {
    fn new() -> Self {
        let root = temp_root("ade263-t18");
        let project = git_project(&root);
        let worktree = root.join("copy");
        add_worktree(&project, &worktree, "copy");
        let worktree = worktree.canonicalize().unwrap();
        let cwd = project.to_string_lossy().into_owned();
        let path = worktree.to_string_lossy().into_owned();
        let store = SessionStore::open_in_memory().unwrap();
        spaces::create(&store, "Space 2", None, 1).unwrap();
        for (id, space) in [("one-a", FIRST_SPACE), ("two-a", TWO)] {
            store
                .lock_conn()
                .unwrap()
                .execute(
                    "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, worktree_cwd, space_id)
                     VALUES (?1, ?2, 'codex', 'm', 'auto', 't', 1, 1, ?3, ?4)",
                    params![id, cwd, path, space],
                )
                .unwrap();
        }
        let app = tauri::test::mock_builder()
            .manage(store)
            .manage(crate::pty::PtyHost::new())
            .manage(crate::harness::HarnessHost::new())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let heard = Arc::new(AtomicUsize::new(0));
        let count = heard.clone();
        app.listen_any(projects::CHANGED, move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        Self {
            root,
            cwd,
            path,
            worktree,
            app,
            heard,
        }
    }

    fn events(&self) -> usize {
        self.heard.load(Ordering::SeqCst)
    }

    fn remove(&self) -> Result<worktrees::WorktreeRemoval, String> {
        worktrees::git_worktree_remove(
            super::agent_sandbox::main_window(&self.app),
            self.cwd.clone(),
            self.path.clone(),
            true,
            Some(true),
            self.app.state(),
            self.app.state(),
        )
    }

    fn remove_for_a_lead(&self) -> Result<worktrees::WorktreeRemoval, String> {
        worktrees::git_orchestration_worktree_remove(
            self.app.handle().clone(),
            self.cwd.clone(),
            self.path.clone(),
            self.app.state(),
            self.app.state(),
            self.app.state(),
        )
    }

    fn delete_row(&self, id: &str) {
        let store: tauri::State<'_, SessionStore> = self.app.state();
        store
            .lock_conn()
            .unwrap()
            .execute("DELETE FROM sessions WHERE id = ?1", [id])
            .unwrap();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn add_worktree(project: &Path, worktree: &Path, branch: &str) {
    let added = Command::new("git")
        .current_dir(project)
        .args(["worktree", "add", "-q", "-b", branch])
        .arg(worktree)
        .status()
        .unwrap();
    assert!(added.success());
}

const REFUSAL: &str = "A session in Space 2 uses this worktree.";

/// Defect (F4): the page's `sessionIds` holds another Space's session, so its
/// delete takes it; `otherSpaceSessions` misses one; or the check that runs
/// before any conversation is destroyed, or the removal, ignores the window's
/// Space, or tells the windows of a refusal.
#[test]
fn a_window_sees_and_removes_only_for_its_own_space() {
    let rig = Rig::new();
    let window = super::agent_sandbox::main_window(&rig.app);
    let listed =
        worktrees::git_worktrees(window.clone(), rig.cwd.clone(), rig.app.state()).unwrap();
    let tree = listed
        .worktrees
        .iter()
        .find(|tree| !tree.is_main)
        .expect("the worktree is listed");
    assert_eq!(tree.session_ids, ["one-a"], "only this Space's sessions");
    assert_eq!(
        tree.other_space_sessions,
        [OtherSpaceSession {
            session_id: "two-a".into(),
            space_id: TWO.into(),
        }]
    );
    let json = serde_json::to_value(tree).unwrap();
    assert_eq!(json["otherSpaceSessions"][0]["spaceId"], TWO);

    let check = worktrees::git_worktree_check_remove(
        window,
        rig.cwd.clone(),
        rig.path.clone(),
        true,
        rig.app.state(),
        rig.app.state(),
    );
    assert_eq!(
        check,
        Err(REFUSAL.to_owned()),
        "T18 refuses before any delete"
    );
    assert_eq!(rig.remove().err().as_deref(), Some(REFUSAL));
    assert!(rig.worktree.exists());
    assert_eq!(rig.events(), 0, "a refusal tells no window");

    rig.delete_row("one-a");
    let window = super::agent_sandbox::main_window(&rig.app);
    let patch = SettingsPatch {
        active_space_id: Some(TWO.into()),
        ..SettingsPatch::default()
    };
    spaces::spaces_settings_set(window, rig.app.state(), patch).unwrap();
    rig.remove().expect("Space 2 removes its own worktree");
    assert!(!rig.worktree.exists());
    assert_eq!(rig.events(), 1, "a removal tells the windows");
}

/// Defect: an orchestration worktree in a Space the window does not show cannot
/// be cleaned up, or sessions of two Spaces do not stop it.
#[test]
fn a_leads_cleanup_stops_only_for_sessions_of_two_spaces() {
    let rig = Rig::new();
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(REFUSAL),
        "sessions of two Spaces stop the lead's cleanup too"
    );
    rig.delete_row("one-a");
    rig.remove_for_a_lead()
        .expect("a window on Space 1 cleans up a Space 2 lead's worktree");
    assert!(!rig.worktree.exists());
    assert_eq!(rig.events(), 1);
}
