//! ADE-263, T18: the Worktrees commands as the page invokes them, from a window
//! of one Space, on a temp repository and an in-memory store.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusqlite::params;
use serde_json::json;
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
        let rig = Self {
            root,
            cwd,
            path,
            worktree,
            app,
            heard,
        };
        rig.show(FIRST_SPACE);
        rig
    }

    /// A window that never read the settings shows no Space.
    fn forget_the_window(&self) {
        let store: tauri::State<'_, SessionStore> = self.app.state();
        store.window_spaces.lock().unwrap().clear();
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
            super::agent_sandbox::main_window(&self.app),
            self.cwd.clone(),
            self.path.clone(),
            self.app.state(),
            self.app.state(),
            self.app.state(),
        )
    }

    fn check(&self) -> Result<(), String> {
        worktrees::git_worktree_check_remove(
            super::agent_sandbox::main_window(&self.app),
            self.cwd.clone(),
            self.path.clone(),
            true,
            self.app.state(),
            self.app.state(),
        )
    }

    /// The window's own switch, which moves its Space and the active one.
    fn show(&self, space: &str) {
        let patch = SettingsPatch {
            active_space_id: Some(space.into()),
            ..SettingsPatch::default()
        };
        let window = super::agent_sandbox::main_window(&self.app);
        spaces::spaces_settings_set(window, self.app.state(), patch).unwrap();
    }

    /// A saved lead in `space` whose run has a task on `checkout`.
    fn run_by_a_lead(&self, space: &str, checkout: &str) {
        let store: tauri::State<'_, SessionStore> = self.app.state();
        let conn = store.lock_conn().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, space_id)
             VALUES ('lead', ?1, 'codex', 'm', 'auto', 't', 1, 1, ?2)",
            params![self.cwd, space],
        )
        .unwrap();
        self.run_without_a_saved_lead(&conn, checkout);
    }

    fn run_without_a_saved_lead(&self, conn: &rusqlite::Connection, checkout: &str) {
        let run = json!({"status": "active", "tasks": [
            {"sessionId": "two-a", "workspace": {"checkoutCwd": checkout}}
        ]});
        crate::session_store::save_orchestration(conn, "lead", &run).unwrap();
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

    assert_eq!(
        rig.check(),
        Err(REFUSAL.to_owned()),
        "T18 refuses before any delete"
    );
    assert_eq!(rig.remove().err().as_deref(), Some(REFUSAL));
    assert!(rig.worktree.exists());
    assert_eq!(rig.events(), 0, "a refusal tells no window");

    rig.delete_row("one-a");
    rig.show(TWO);
    rig.remove().expect("Space 2 removes its own worktree");
    assert!(!rig.worktree.exists());
    assert_eq!(rig.events(), 1, "a removal tells the windows");
}

/// B1. Defect: a lead's cleanup removes a worktree used only by another Space's
/// sessions, with `force`, because S comes from anywhere but the calling window.
#[test]
fn a_leads_cleanup_answers_for_the_calling_windows_space() {
    let rig = Rig::new();
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(REFUSAL),
        "sessions of two Spaces stop the lead's cleanup too"
    );
    rig.delete_row("one-a");
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(REFUSAL),
        "a window on Space 1 removed a worktree only Space 2 uses"
    );
    assert!(rig.worktree.exists(), "the worktree went");
    assert_eq!(rig.events(), 0, "a refusal tells no window");

    rig.show(TWO);
    rig.remove_for_a_lead()
        .expect("a window on Space 2 cleans up Space 2's worktree");
    assert!(!rig.worktree.exists());
    assert_eq!(rig.events(), 1);
}

/// B2. Defect: a failed read of the sessions counts as no sessions, so the check
/// passes and the page goes on to delete conversations.
#[test]
fn a_failed_session_read_refuses() {
    let rig = Rig::new();
    let store: tauri::State<'_, SessionStore> = rig.app.state();
    store
        .lock_conn()
        .unwrap()
        .execute_batch("ALTER TABLE sessions RENAME TO gone")
        .unwrap();
    assert!(rig.check().is_err(), "the check passed on a failed read");
    assert!(rig.remove().is_err(), "the removal passed on a failed read");
    assert!(
        rig.remove_for_a_lead().is_err(),
        "the lead's cleanup passed on a failed read"
    );
    assert!(rig.worktree.exists(), "the worktree went");
    assert_eq!(rig.events(), 0);
}

/// B3. Defect: a removal reads the app's active Space, which another window's
/// switch moves, not the calling window's.
#[test]
fn another_windows_switch_does_not_move_a_removal() {
    let rig = Rig::new();
    rig.show(FIRST_SPACE);
    let store: tauri::State<'_, SessionStore> = rig.app.state();
    let patch = SettingsPatch {
        active_space_id: Some(TWO.into()),
        ..SettingsPatch::default()
    };
    spaces::set_settings(&store, &patch).unwrap();

    let window = super::agent_sandbox::main_window(&rig.app);
    let listed = worktrees::git_worktrees(window, rig.cwd.clone(), rig.app.state()).unwrap();
    let tree = listed.worktrees.iter().find(|tree| !tree.is_main).unwrap();
    assert_eq!(tree.session_ids, ["one-a"], "it listed the active Space's");
    assert_eq!(tree.other_space_sessions[0].space_id, TWO);
    assert_eq!(rig.check(), Err(REFUSAL.to_owned()));
    assert_eq!(rig.remove().err().as_deref(), Some(REFUSAL));
    assert_eq!(rig.remove_for_a_lead().err().as_deref(), Some(REFUSAL));
    assert!(rig.worktree.exists());
}

/// Defect: a Space 2 lead's cleanup is refused whenever its window shows Space 1,
/// leaving a worktree behind, because S ignores the lead (I11); or a run's folder
/// written in another form of the path is not matched.
#[test]
fn a_leads_cleanup_answers_for_the_leads_own_space_whatever_the_window_shows() {
    let rig = Rig::new();
    rig.show(FIRST_SPACE);
    rig.delete_row("one-a");
    rig.run_by_a_lead(TWO, &format!("{}/", rig.path));
    rig.remove_for_a_lead()
        .expect("a Space 2 lead cleans its own worktree from a window on Space 1");
    assert!(!rig.worktree.exists());
    assert_eq!(rig.events(), 1);
}

/// Defect: the lead's Space lets a cleanup past another Space's session, which T18
/// must still refuse.
#[test]
fn a_leads_own_space_does_not_get_past_another_spaces_session() {
    let rig = Rig::new();
    rig.show(TWO);
    rig.run_by_a_lead(TWO, &rig.path);
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some("A session in Space 1 uses this worktree."),
    );
    assert!(rig.worktree.exists(), "the worktree went");
    assert_eq!(rig.events(), 0);
}

/// Defect: a run that does not have this worktree, or a lead with no saved row,
/// decides the Space; either must keep the calling window's, which fails safe.
#[test]
fn a_cleanup_no_saved_lead_answers_for_keeps_the_windows_space() {
    let rig = Rig::new();
    rig.show(FIRST_SPACE);
    rig.delete_row("one-a");
    rig.run_by_a_lead(TWO, "/ade263-another-worktree");
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(REFUSAL),
        "a run of another worktree decided the Space"
    );
    assert!(rig.worktree.exists());

    rig.delete_row("lead");
    let store: tauri::State<'_, SessionStore> = rig.app.state();
    rig.run_without_a_saved_lead(&store.lock_conn().unwrap(), &rig.path);
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(REFUSAL),
        "a lead with no saved row decided the Space"
    );
    assert!(rig.worktree.exists());
    assert_eq!(rig.events(), 0);
}

/// Defect: a window that shows no Space acts for Space 1 or the active Space, so
/// it deletes what another Space uses. It lists no session as its own, its check
/// and removal refuse, and a lead's cleanup needs a saved lead to answer for it.
#[test]
fn a_window_with_no_space_is_refused_and_lists_nothing_as_its_own() {
    const NO_SPACE: &str = "This window has no Space yet.";
    let rig = Rig::new();
    rig.delete_row("two-a");
    rig.forget_the_window();
    let window = super::agent_sandbox::main_window(&rig.app);
    let listed = worktrees::git_worktrees(window, rig.cwd.clone(), rig.app.state()).unwrap();
    let tree = listed.worktrees.iter().find(|tree| !tree.is_main).unwrap();
    assert!(
        tree.session_ids.is_empty(),
        "it listed a session as its own"
    );
    assert_eq!(tree.other_space_sessions[0].session_id, "one-a");
    assert_eq!(rig.check(), Err(NO_SPACE.to_owned()));
    assert_eq!(rig.remove().err().as_deref(), Some(NO_SPACE));
    assert_eq!(rig.remove_for_a_lead().err().as_deref(), Some(NO_SPACE));
    assert!(rig.worktree.exists());

    let store: tauri::State<'_, SessionStore> = rig.app.state();
    rig.run_without_a_saved_lead(&store.lock_conn().unwrap(), &rig.path);
    assert_eq!(
        rig.remove_for_a_lead().err().as_deref(),
        Some(NO_SPACE),
        "an unsaved lead took the active Space"
    );
    assert!(rig.worktree.exists());

    rig.run_by_a_lead(FIRST_SPACE, &rig.path);
    rig.remove_for_a_lead()
        .expect("a saved lead answers for a window with no Space");
    assert!(!rig.worktree.exists());
}
