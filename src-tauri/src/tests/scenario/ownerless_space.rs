//! ADE-209: the ownerless-helper rule's guard (`leases::keep_ownerless`)
//! judges a call of no stored session by the calling window's Space, the
//! active Space the window saves on each switch (`spaces_settings_set`), not
//! by Space 1. An owner that is no stored session is judged by the Space its
//! row would join (`space_for_row`).

use std::path::{Path, PathBuf};

use tauri::test::MockRuntime;
use tauri::{App, Manager};

use super::agent_sandbox::{mock_app, script, wait_for};
use super::space_checkouts::{git_project, temp_root, two_spaces};
use crate::session_store::SessionStore;
use crate::spaces::{self, leases, Helper, SettingsPatch, FIRST_SPACE};

struct Rig {
    app: App<MockRuntime>,
    fx: PathBuf,
    /// The git checkout Space 1 holds.
    project: PathBuf,
    /// Space 2's own copy of it.
    copy: PathBuf,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// What the window calls on a switch.
fn switch_to(rig: &Rig, space: &str) {
    let patch = SettingsPatch {
        active_space_id: Some(space.to_owned()),
        route_grant_ttl_ms: None,
    };
    spaces::spaces_settings_set(rig.app.state(), patch).unwrap();
}

/// Starts helper `key` of `owner` in `workdir` through `harness_spawn`; the
/// stand-in writes the folder it runs in to `<folder>/<key>`. Returns the
/// folder the command names, or why the spawn hook refused it.
fn helper(rig: &Rig, key: &str, owner: Option<&str>, workdir: &Path) -> Result<String, String> {
    crate::harness::harness_spawn(
        rig.app.handle().clone(),
        rig.app.state::<crate::harness::HarnessHost>(),
        key.into(),
        text(&rig.fx),
        vec!["acp".into(), key.into()],
        text(workdir),
        None,
        Some("fx".into()),
        Some(text(&rig.fx)),
        None,
        None,
        None,
        Some(Helper {
            owner: owner.map(String::from),
            key: None,
        }),
    )
    .map(|spawned| spawned.cwd)
}

/// A `harness_exec` acting for helper child `key` of no session, in `cwd`.
fn exec(rig: &Rig, key: &str, cwd: &Path) -> Result<PathBuf, String> {
    tauri::async_runtime::block_on(crate::harness::harness_exec(
        rig.app.handle().clone(),
        text(&rig.fx),
        vec!["models".into(), "--json".into()],
        Some(text(cwd)),
        Some("fx".into()),
        Some(text(&rig.fx)),
        Some(Helper {
            owner: None,
            key: Some(key.into()),
        }),
    ))
    .map(|ran| PathBuf::from(ran.cwd))
}

fn refused(folder: &Path, holder: &str) -> Result<String, String> {
    Err(format!(
        "{} is in {holder}'s checkout: a call of no session runs only outside another Space's checkout",
        text(folder)
    ))
}

/// Helper `key` (of `owner`) runs in `folder`: the command names it and the
/// stand-in wrote it there.
fn runs_in(rig: &Rig, key: &str, owner: Option<&str>, folder: &Path, why: &str) {
    assert_eq!(helper(rig, key, owner, folder), Ok(text(folder)), "{why}");
    assert_eq!(
        wait_for(&folder.join(key)),
        format!("{}\n", text(folder)),
        "{why}: the helper ran in another folder"
    );
}

/// Helper `key` (of `owner`) in `folder`, which `holder` holds, is refused
/// and nothing is spawned there.
fn refused_in(rig: &Rig, key: &str, owner: Option<&str>, folder: &Path, holder: &str, why: &str) {
    assert_eq!(
        helper(rig, key, owner, folder),
        refused(folder, holder),
        "{why}"
    );
    assert!(
        !folder.join(key).exists(),
        "{why}: the helper was spawned there"
    );
}

/// Defect: the guard ignores the window's Space and judges every call of no
/// session as Space 1's, so a Space 2 window's commit-text helper runs in
/// the checkout only Space 1 holds and is refused in Space 2's own copy; the
/// guard compares the lease with another Space than the caller's; or a
/// Space 1 window's call is judged otherwise than before.
#[test]
fn an_ownerless_helper_is_judged_by_the_calling_windows_space() {
    let root = temp_root("ade209");
    let project = git_project(&root);
    let data = root.join("app-data");
    std::fs::create_dir_all(&data).unwrap();
    let app = mock_app(&data);
    two_spaces(&app.state::<SessionStore>(), &project);
    let rig = Rig {
        fx: script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  models) pwd -P ;;
  *) pwd -P > "$2.part" && mv "$2.part" "$2" ;;
esac"#,
        ),
        copy: root.join("project-worktrees/space-2/two"),
        project,
        app,
    };
    // Space 1's session holds the project; Space 2's gets its own copy.
    runs_in(
        &rig,
        "monocode-claude-text-one",
        Some("one"),
        &rig.project,
        "Space 1's helper left its project",
    );
    assert_eq!(
        helper(&rig, "monocode-claude-text-two", Some("two"), &rig.project),
        Ok(text(&rig.copy)),
        "Space 2's helper was not placed in its own copy"
    );
    wait_for(&rig.copy.join("monocode-claude-text-two"));
    let held = || {
        leases::leases(&rig.app.state::<SessionStore>())
            .unwrap()
            .into_iter()
            .map(|lease| (lease.checkout_path, lease.space_id))
            .collect::<Vec<_>>()
    };
    let leased = vec![
        (text(&rig.project), FIRST_SPACE.to_owned()),
        (text(&rig.copy), "space-2".to_owned()),
    ];
    assert_eq!(held(), leased);

    a_space_one_window_is_unchanged(&rig, "1");
    a_space_two_window_keeps_to_its_own_checkout(&rig);
    switch_to(&rig, FIRST_SPACE);
    a_space_one_window_is_unchanged(&rig, "2");
    an_unsaved_owner_is_judged_by_the_space_it_would_join(&rig);
    assert_eq!(held(), leased, "a call of no stored session took a lease");
    let _ = std::fs::remove_dir_all(&root);
}

/// With Space 1 active, an ownerless commit-text helper runs in Space 1's
/// checkout and is refused in Space 2's copy, as before ADE-209.
fn a_space_one_window_is_unchanged(rig: &Rig, round: &str) {
    runs_in(
        rig,
        &format!("monocode-claude-text-s1-{round}"),
        None,
        &rig.project,
        "a Space 1 window's ownerless helper left Space 1's checkout",
    );
    refused_in(
        rig,
        &format!("monocode-claude-text-s1-into-two-{round}"),
        None,
        &rig.copy,
        "space-2",
        "a Space 1 window's ownerless helper ran in Space 2's copy",
    );
    assert_eq!(
        exec(rig, "monocode-grok-text", &rig.copy),
        Err(refused(&rig.copy, "space-2").unwrap_err()),
        "a Space 1 window's cleanup exec ran in Space 2's copy"
    );
}

/// After the window switches to Space 2, its ownerless commit-text helper
/// pointed at the checkout only Space 1 holds is refused and nothing spawns
/// there; the same call in Space 2's own copy runs there.
fn a_space_two_window_keeps_to_its_own_checkout(rig: &Rig) {
    switch_to(rig, "space-2");
    refused_in(
        rig,
        "monocode-claude-text-s2-into-one",
        None,
        &rig.project,
        FIRST_SPACE,
        "a Space 2 window's ownerless commit-text helper ran in Space 1's checkout",
    );
    assert_eq!(
        exec(rig, "monocode-grok-text", &rig.project),
        Err(refused(&rig.project, FIRST_SPACE).unwrap_err()),
        "a Space 2 window's cleanup exec ran in Space 1's checkout"
    );
    runs_in(
        rig,
        "monocode-claude-text-s2",
        None,
        &rig.copy,
        "a Space 2 window's ownerless commit-text helper was refused in Space 2's copy",
    );
    assert_eq!(
        exec(rig, "monocode-grok-text", &rig.copy),
        Ok(rig.copy.clone()),
        "a Space 2 window's cleanup exec was refused in Space 2's copy"
    );
    // The `/` picker of a tab not saved yet joins the active Space.
    refused_in(
        rig,
        "monocode-pi-skills-s2-into-one",
        Some("unsaved-tab"),
        &rig.project,
        FIRST_SPACE,
        "an unsaved tab's picker in a Space 2 window ran in Space 1's checkout",
    );
    runs_in(
        rig,
        "monocode-pi-skills-s2",
        Some("unsaved-tab"),
        &rig.copy,
        "an unsaved tab's picker in a Space 2 window was refused in Space 2's copy",
    );
}

/// With Space 1 active, the picker of an unsaved session that Space 2's
/// session `two` started (`app-two-<request>`) is in Space 2, where its row
/// will be saved, as its login is (`account::resolve`).
fn an_unsaved_owner_is_judged_by_the_space_it_would_join(rig: &Rig) {
    refused_in(
        rig,
        "monocode-omp-skills-lead-into-one",
        Some("app-two-r1"),
        &rig.project,
        FIRST_SPACE,
        "a Space 2 lead's unsaved child was judged by the active Space",
    );
    runs_in(
        rig,
        "monocode-omp-skills-lead",
        Some("app-two-r1"),
        &rig.copy,
        "a Space 2 lead's unsaved child was refused in Space 2's copy",
    );
}
