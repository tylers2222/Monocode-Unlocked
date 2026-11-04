//! ADE-209: a helper call under the ownerless-helper rule that names no
//! owner is judged as the Space of the window that called it, the one that
//! window read or switched to (`spaces_settings_get`, `spaces_settings_set`),
//! never the Space another window switched the app to. Its guard
//! (`leases::keep_ownerless`) refuses it in another Space's checkout, and it
//! starts with its Space's login (`account::resolve_in`), not the CLI's own.
//!
//! ADE-244: a new session or Mono row with no parent joins the Space of the
//! window that saves it (`session_upsert`, `monos_create`), so it is listed
//! where that window shows, and an unsaved tab's helper is judged by that
//! same Space. A parent's Space still comes first, and a `sessions.start`
//! row of the agent CLI or MCP whose lead is not saved still joins the app's
//! active Space.

use std::path::{Path, PathBuf};

use serde_json::json;
use tauri::test::MockRuntime;
use tauri::{App, Manager, WebviewWindow};

use super::agent_sandbox::{mock_app, script, wait_for};
use super::space_checkouts::{git_project, temp_root, two_spaces};
use super::space_login::save_automation_run;
use crate::harness::HarnessAccount;
use crate::mono_roster::{self, MonoCreate};
use crate::session_store::{self, SessionStore, SessionUpsert};
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, RouteGrant};
use crate::spaces::{
    self, leases, prepare_spawn, Child, Helper, ProjectLogin, Provider, SettingsPatch, SpawnTarget,
    FIRST_SPACE,
};

const TWO: &str = "space-2";

struct Rig {
    app: App<MockRuntime>,
    fx: PathBuf,
    /// The git checkout Space 1 holds.
    project: PathBuf,
    /// Space 2's own copy of it.
    copy: PathBuf,
    /// Window A stays in Space 1; window B switches to Space 2.
    a: WebviewWindow<MockRuntime>,
    b: WebviewWindow<MockRuntime>,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn store(rig: &Rig) -> tauri::State<'_, SessionStore> {
    rig.app.state::<SessionStore>()
}

fn window(app: &App<MockRuntime>, label: &str) -> WebviewWindow<MockRuntime> {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .build()
        .unwrap()
}

/// The Space `window` shows, as `spaces_settings_get` answers it.
fn shows(rig: &Rig, window: &WebviewWindow<MockRuntime>) -> String {
    spaces::spaces_settings_get(window.clone(), store(rig))
        .unwrap()
        .active_space_id
}

/// What `window` calls on a switch.
fn switch(rig: &Rig, window: &WebviewWindow<MockRuntime>, space: &str) {
    let patch = SettingsPatch {
        active_space_id: Some(space.to_owned()),
        route_grant_ttl_ms: None,
    };
    let settings = spaces::spaces_settings_set(window.clone(), store(rig), patch).unwrap();
    assert_eq!(settings.active_space_id, space);
}

/// `window` starts helper `key` of `owner` in `workdir` with the CLI's
/// `default` login named, as the text helpers send it. The stand-in writes
/// its folder and `CLAUDE_CONFIG_DIR` to `<folder>/<key>`. Returns the folder
/// the command names, or why the spawn hook refused it.
fn helper(
    rig: &Rig,
    window: &WebviewWindow<MockRuntime>,
    key: &str,
    owner: Option<&str>,
    workdir: &Path,
    provider: &str,
) -> Result<String, String> {
    let account = HarnessAccount {
        provider: provider.into(),
        id: "default".into(),
    };
    helper_on(rig, window, (key, owner), workdir, account)
}

/// [`helper`], naming login `account`.
fn helper_on(
    rig: &Rig,
    window: &WebviewWindow<MockRuntime>,
    (key, owner): (&str, Option<&str>),
    workdir: &Path,
    account: HarnessAccount,
) -> Result<String, String> {
    crate::harness::harness_spawn(
        rig.app.handle().clone(),
        window.clone(),
        rig.app.state::<crate::harness::HarnessHost>(),
        key.into(),
        text(&rig.fx),
        vec!["acp".into(), key.into()],
        text(workdir),
        Some(account),
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

/// A `harness_exec` from `window` acting for the Grok text cleanup, in `cwd`.
fn cleanup(rig: &Rig, window: &WebviewWindow<MockRuntime>, cwd: &Path) -> Result<PathBuf, String> {
    tauri::async_runtime::block_on(crate::harness::harness_exec(
        window.clone(),
        text(&rig.fx),
        vec!["models".into(), "--json".into()],
        Some(text(cwd)),
        Some("fx".into()),
        Some(text(&rig.fx)),
        Some(Helper {
            owner: None,
            key: Some("monocode-grok-text".into()),
        }),
    ))
    .map(|ran| PathBuf::from(ran.cwd))
}

/// The login folder of Claude login `id`, as the stand-in prints it; empty
/// for the CLI's own login.
fn login(rig: &Rig, id: Option<&str>) -> String {
    id.map(|id| {
        text(&crate::harness::provider_account_path(rig.app.handle(), "claude", id).unwrap())
    })
    .unwrap_or_default()
}

/// Helper `key` from `window` runs in `folder` with Claude login `login`.
fn runs(
    rig: &Rig,
    window: &WebviewWindow<MockRuntime>,
    key: &str,
    owner: Option<&str>,
    folder: &Path,
    login: &str,
    why: &str,
) {
    assert_eq!(
        helper(rig, window, key, owner, folder, "claude"),
        Ok(text(folder)),
        "{why}"
    );
    assert_eq!(
        wait_for(&folder.join(key)),
        format!("{}\n{login}\n", text(folder)),
        "{why}: it ran in another folder or with another login"
    );
}

/// The refusal of a call judged as `space` in `folder`, which `holder` holds.
fn is_refusal(
    result: &Result<impl std::fmt::Debug, String>,
    folder: &Path,
    holder: &str,
    space: &str,
) -> bool {
    result.as_ref().is_err_and(|error| {
        error.starts_with(&format!("{} is in {holder}'s checkout", text(folder)))
            && error.contains(&format!("from {space} "))
    })
}

/// Helper `key` from `window` in `folder` is refused as a call of `space`,
/// and nothing is spawned there.
fn refused(
    rig: &Rig,
    window: &WebviewWindow<MockRuntime>,
    key: &str,
    owner: Option<&str>,
    (folder, holder): (&Path, &str),
    space: &str,
    why: &str,
) {
    let result = helper(rig, window, key, owner, folder, "claude");
    assert!(
        is_refusal(&result, folder, holder, space),
        "{why}: {result:?}"
    );
    assert!(
        !folder.join(key).exists(),
        "{why}: the helper was spawned there"
    );
}

/// Defect: the guard judges a call of no stored session by the app's active
/// Space, by Space 1, or by any Space but the calling window's, so window A
/// (Space 1) gets into Space 2's checkout after window B switches, or is
/// refused in its own; `spaces_settings_get` answers A with B's Space; a
/// helper in Space 2's checkout starts with the CLI's own login, or with no
/// refusal when Space 2 has no login for its CLI; a helper that names an
/// owner moves with the window that called instead of keeping its owner's
/// Space and login; or a closed window's Space outlives it.
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
  *) printf '%s\n%s\n' "$(pwd -P)" "$CLAUDE_CONFIG_DIR" > "$2.part" && mv "$2.part" "$2" ;;
esac"#,
        ),
        copy: root.join("project-worktrees/space-2/two"),
        project,
        a: window(&app, "window-a"),
        b: window(&app, "window-b"),
        app,
    };
    account::save_account(
        &store(&rig),
        &ProviderAccount {
            provider: Provider::Claude,
            id: "work2".into(),
            label: "Space 2 work".into(),
        },
        0,
    )
    .unwrap();
    let space_two_login = SpaceLogin {
        space_id: TWO.into(),
        provider: Provider::Claude,
        account_id: "work2".into(),
    };
    account::spaces_login_set(store(&rig), space_two_login).unwrap();
    // Space 1's session holds the project; Space 2's gets its own copy.
    let a = &rig.a;
    runs(
        &rig,
        a,
        "monocode-claude-text-one",
        Some("one"),
        &rig.project,
        "",
        "Space 1's helper left its project",
    );
    assert_eq!(
        helper(
            &rig,
            a,
            "monocode-claude-text-two",
            Some("two"),
            &rig.project,
            "claude"
        ),
        Ok(text(&rig.copy)),
        "Space 2's helper was not placed in its own copy"
    );
    wait_for(&rig.copy.join("monocode-claude-text-two"));
    let held = || -> Vec<(String, String)> {
        leases::leases(&store(&rig))
            .unwrap()
            .into_iter()
            .map(|lease| (lease.checkout_path, lease.space_id))
            .collect()
    };
    let leased = vec![
        (text(&rig.project), FIRST_SPACE.to_owned()),
        (text(&rig.copy), TWO.to_owned()),
    ];
    assert_eq!(held(), leased);

    each_window_keeps_its_own_space(&rig);
    new_rows_join_the_saving_windows_space(&rig);
    window_a_stays_in_space_one(&rig);
    window_b_works_in_space_two(&rig);
    an_owned_helper_keeps_its_owners_space(&rig);
    an_unsaved_owner_is_checked_as_a_saved_one(&rig);
    assert_eq!(held(), leased, "a call of no stored session took a lease");
    a_closed_window_forgets_its_space(&rig);
    let _ = std::fs::remove_dir_all(&root);
}

/// Both windows read the settings at launch; B's switch to Space 2 is the
/// app's active Space now, but A still reads Space 1.
fn each_window_keeps_its_own_space(rig: &Rig) {
    assert_eq!(shows(rig, &rig.a), FIRST_SPACE);
    assert_eq!(shows(rig, &rig.b), FIRST_SPACE);
    switch(rig, &rig.b, TWO);
    assert_eq!(
        spaces::settings(&store(rig)).unwrap().active_space_id,
        TWO,
        "B's switch did not move the app's active Space"
    );
    assert_eq!(
        shows(rig, &rig.a),
        FIRST_SPACE,
        "A reads the Space B switched to"
    );
    assert_eq!(shows(rig, &rig.b), TWO);
    let c = window(&rig.app, "window-c");
    assert_eq!(
        shows(rig, &c),
        TWO,
        "a new window does not open in the Space B saved for the app"
    );
    // A save that names no Space (the grant length) answers A's own Space
    // and leaves the app's.
    let ttl = SettingsPatch {
        active_space_id: None,
        route_grant_ttl_ms: Some(60_000),
    };
    let saved = spaces::spaces_settings_set(rig.a.clone(), store(rig), ttl).unwrap();
    assert_eq!(
        (saved.active_space_id, saved.route_grant_ttl_ms),
        (FIRST_SPACE.to_owned(), 60_000),
        "a save from A answered another Space than A's, or lost the grant length"
    );
    assert_eq!(
        spaces::settings(&store(rig)).unwrap().active_space_id,
        TWO,
        "A's save that named no Space moved the app's active Space"
    );
}

/// While B shows Space 2, A's ownerless commit-text helper is refused in
/// Space 2's copy (B1) and runs in Space 1's checkout with the CLI's own
/// login (M1); A's cleanup exec is refused in Space 2's copy.
fn window_a_stays_in_space_one(rig: &Rig) {
    let a = &rig.a;
    refused(
        rig,
        a,
        "monocode-claude-text-a-into-two",
        None,
        (&rig.copy, TWO),
        FIRST_SPACE,
        "a Space 1 window's ownerless helper ran in Space 2's copy after another window switched",
    );
    runs(
        rig,
        a,
        "monocode-claude-text-a",
        None,
        &rig.project,
        "",
        "a Space 1 window's ownerless helper left Space 1's checkout after another window switched",
    );
    let exec = cleanup(rig, a, &rig.copy);
    assert!(
        is_refusal(&exec, &rig.copy, TWO, FIRST_SPACE),
        "a Space 1 window's cleanup exec ran in Space 2's copy: {exec:?}"
    );
}

/// B's ownerless commit-text helper runs in Space 2's copy with Space 2's
/// login, though the window named `default` (B2), and is refused in the
/// checkout only Space 1 holds. With no Space 2 login for its CLI it is
/// refused. A git panel's call, which names no login, gets Space 2's too.
fn window_b_works_in_space_two(rig: &Rig) {
    let b = &rig.b;
    let work2 = login(rig, Some("work2"));
    runs(
        rig,
        b,
        "monocode-claude-text-b",
        None,
        &rig.copy,
        &work2,
        "a Space 2 window's ownerless helper did not run in Space 2's copy with Space 2's login",
    );
    refused(
        rig,
        b,
        "monocode-claude-text-b-into-one",
        None,
        (&rig.project, FIRST_SPACE),
        TWO,
        "a Space 2 window's ownerless helper ran in the checkout only Space 1 holds",
    );
    assert_eq!(cleanup(rig, b, &rig.copy), Ok(rig.copy.clone()));
    let exec = cleanup(rig, b, &rig.project);
    assert!(
        is_refusal(&exec, &rig.project, FIRST_SPACE, TWO),
        "a Space 2 window's cleanup exec ran in Space 1's checkout: {exec:?}"
    );
    assert_eq!(
        helper(rig, b, "monocode-codex-text-b", None, &rig.copy, "codex"),
        Err("Space 2 has no Codex login".into()),
        "a Space 2 helper started with the CLI's own login where Space 2 has none"
    );
    assert!(!rig.copy.join("monocode-codex-text-b").exists());
    let neutral = rig.app.path().app_cache_dir().unwrap().join("helper-cwd");
    let unnamed = prepare_spawn(
        &store(rig),
        Child::Helper {
            key: Some("monocode-claude-text-panel"),
            owner: None,
            neutral: &neutral,
        },
        TWO,
        Some(Provider::Claude),
        0,
        SpawnTarget {
            workdir: rig.copy.clone(),
            account: None,
        },
    )
    .unwrap();
    assert_eq!(
        unnamed
            .account
            .map(|account| (account.provider, account.id)),
        Some(("claude".to_owned(), "work2".to_owned())),
        "a Space 2 git panel's helper, which names no login, kept the CLI's own"
    );
}

/// A helper that names an owner takes its Space and login from that owner,
/// never from the calling window. After B's switch, Space 1's session `one`
/// still runs in its project from window B, and Space 2's session `two` in
/// its copy from window A. An owner that is no stored session is in the
/// Space its row would join (`space_for_row`): an unsaved child of `two`
/// (`app-two-<request>`) is Space 2's from either window, and an unsaved tab
/// is its window's (ADE-244): A's `/` picker runs in A's own Space 1
/// checkout, though the app's active Space is Space 2 (#527 m1).
fn an_owned_helper_keeps_its_owners_space(rig: &Rig) {
    let (a, b) = (&rig.a, &rig.b);
    let work2 = login(rig, Some("work2"));
    runs(
        rig,
        b,
        "monocode-claude-text-one-from-b",
        Some("one"),
        &rig.project,
        "",
        "Space 1's session's helper from a Space 2 window left Space 1's project",
    );
    assert_eq!(
        helper(
            rig,
            a,
            "monocode-claude-text-two-from-a",
            Some("two"),
            &rig.project,
            "claude"
        ),
        Ok(text(&rig.copy)),
        "Space 2's session's helper from a Space 1 window left Space 2's copy"
    );
    assert_eq!(
        wait_for(&rig.copy.join("monocode-claude-text-two-from-a")),
        format!("{}\n{work2}\n", text(&rig.copy)),
        "Space 2's session's helper from a Space 1 window did not start with Space 2's login"
    );
    for (window, side) in [(a, "a"), (b, "b")] {
        runs(
            rig,
            window,
            &format!("monocode-omp-skills-lead-{side}"),
            Some("app-two-r1"),
            &rig.copy,
            &work2,
            "a Space 2 lead's unsaved child was refused in Space 2's copy, or kept the CLI's own login",
        );
        refused(
            rig,
            window,
            &format!("monocode-omp-skills-lead-into-one-{side}"),
            Some("app-two-r1"),
            (&rig.project, FIRST_SPACE),
            TWO,
            "a Space 2 lead's unsaved child was judged by its window's Space",
        );
    }
    runs(
        rig,
        a,
        "monocode-pi-skills-tab-a",
        Some("unsaved-tab"),
        &rig.project,
        "",
        "window A's unsaved tab's picker was refused in A's own Space 1 checkout after B switched",
    );
    refused(
        rig,
        a,
        "monocode-pi-skills-tab-into-two-a",
        Some("unsaved-tab"),
        (&rig.copy, TWO),
        FIRST_SPACE,
        "window A's unsaved tab's picker was judged by another Space than A's",
    );
    runs(
        rig,
        b,
        "monocode-pi-skills-tab-b",
        Some("unsaved-tab"),
        &rig.copy,
        &work2,
        "window B's unsaved tab's picker did not run in Space 2's copy with Space 2's login",
    );
    refused(
        rig,
        b,
        "monocode-pi-skills-tab-into-one-b",
        Some("unsaved-tab"),
        (&rig.project, FIRST_SPACE),
        TWO,
        "window B's unsaved tab's picker was judged by another Space than B's",
    );
}

/// What `window` saves for new session `id` in the project, naming `lead`
/// (`session_upsert`): the Space its reply gives it, which the project's
/// session list gives it too.
fn save(rig: &Rig, window: &WebviewWindow<MockRuntime>, id: &str, lead: Option<&str>) -> String {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": text(&rig.project),
        "harness": "codex",
        "model": "codex:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [{"id": "u1", "role": "user", "text": "Start"}],
        "leadSessionId": lead,
    }))
    .unwrap();
    let reply = session_store::session_upsert(window.clone(), store(rig), session).unwrap();
    let listed = session_store::session_list_by_project(store(rig), text(&rig.project)).unwrap();
    let row = listed.iter().find(|row| row.id == id);
    assert_eq!(
        row.map(|row| row.space_id.as_str()),
        Some(reply.space_id.as_str()),
        "{id}: the project's list gives it another Space than the save's reply"
    );
    reply.space_id
}

/// The Space of new Mono `id` that `window` creates (`monos_create`), as the
/// roster lists it.
fn create_mono(rig: &Rig, window: &WebviewWindow<MockRuntime>, id: &str) -> String {
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": id,
        "role": "mono",
        "data": {"name": id},
        "sortOrder": 0,
    }))
    .unwrap();
    let roster =
        mono_roster::monos_create(rig.app.handle().clone(), window.clone(), store(rig), mono)
            .unwrap();
    let row = roster.monos.into_iter().find(|row| row.id == id).unwrap();
    row.space_id
}

/// After B's switch to Space 2, the app's active Space, a session or Mono
/// with no parent that A saves joins Space 1, the Space A shows, and one B
/// saves joins Space 2. A parent's Space comes first from either window. A
/// `sessions.start` row (an `app-<lead>-<request>` id, or a lead) whose lead
/// is not saved joins the app's active Space, as before ADE-244.
fn new_rows_join_the_saving_windows_space(rig: &Rig) {
    let (a, b) = (&rig.a, &rig.b);
    assert_eq!(
        save(rig, a, "row-a", None),
        shows(rig, a),
        "a session saved from window A after B switched is not in the Space A shows"
    );
    assert_eq!(shows(rig, a), FIRST_SPACE);
    assert_eq!(
        save(rig, b, "row-b", None),
        TWO,
        "a session saved from window B is not in Space 2"
    );
    assert_eq!(
        create_mono(rig, a, "mono-a"),
        FIRST_SPACE,
        "a Mono created from window A after B switched is not in the Space A shows"
    );
    assert_eq!(
        create_mono(rig, b, "mono-b"),
        TWO,
        "a Mono created from window B is not in Space 2"
    );
    assert_eq!(
        save(rig, a, "app-two-r7", Some("two")),
        TWO,
        "a child of Space 2's session saved from window A left its parent's Space"
    );
    assert_eq!(
        save(rig, b, "app-one-r7", Some("one")),
        FIRST_SPACE,
        "a child of Space 1's session saved from window B left its parent's Space"
    );
    assert_eq!(
        save(rig, a, "app-gone-r1", None),
        TWO,
        "an agent's start whose lead is not saved left the app's active Space"
    );
    assert_eq!(
        save(rig, a, "worker-of-gone", Some("gone")),
        TWO,
        "a worker whose lead is not saved left the app's active Space"
    );
}

/// #527 m2: a helper of an unsaved owner takes the project of the
/// automation run it is, as `account::resolve` does, and an ended
/// usage-limit grant of that owner refuses it, as `grants::route` refuses a
/// stored owner's helper.
fn an_unsaved_owner_is_checked_as_a_saved_one(rig: &Rig) {
    let auto = rig.fx.with_file_name("auto-project");
    let elsewhere = rig.fx.with_file_name("elsewhere");
    for folder in [&auto, &elsewhere] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let proj2 = ProviderAccount {
        provider: Provider::Claude,
        id: "proj2".into(),
        label: "Space 2 project".into(),
    };
    account::save_account(&store(rig), &proj2, 0).unwrap();
    spaces::set_project_space(&store(rig), &text(&auto), TWO).unwrap();
    let project_login = ProjectLogin {
        space_id: TWO.into(),
        project_path: text(&auto),
        provider: Provider::Claude,
        account_id: "proj2".into(),
    };
    spaces::set_project_login(&store(rig), &project_login).unwrap();
    save_automation_run(&store(rig).lock_conn().unwrap(), "auto-run", &text(&auto));
    runs(
        rig,
        &rig.a,
        "monocode-claude-text-auto-run",
        Some("auto-run"),
        &elsewhere,
        &login(rig, Some("proj2")),
        "an unsaved automation run's helper did not start with its run's project's login",
    );

    let ended = RouteGrant {
        session_id: "tab-granted".into(),
        provider: Provider::Claude,
        from_account_id: "proj2".into(),
        to_account_id: "work2".into(),
        granted_at: 0,
        expires_at: 1,
    };
    grants::save_grant(&store(rig), &ended).unwrap();
    let key = "monocode-claude-text-granted";
    let work2 = HarnessAccount {
        provider: "claude".into(),
        id: "work2".into(),
    };
    let result = helper_on(rig, &rig.b, (key, Some("tab-granted")), &rig.copy, work2);
    assert!(
        result
            .as_ref()
            .is_err_and(|error| error.starts_with("route_grant_expired")),
        "an unsaved owner's helper ran on its ended grant: {result:?}"
    );
    assert!(!rig.copy.join(key).exists(), "the helper was spawned");
    assert_eq!(grants::grant(&store(rig), "tab-granted"), Ok(None));
}

/// Closing window B (`destroy_window`, the end of every workspace window's
/// close) forgets its Space, and A keeps its own.
fn a_closed_window_forgets_its_space(rig: &Rig) {
    crate::window::destroy_window(rig.b.clone(), store(rig)).unwrap();
    assert_eq!(
        spaces::window_space(&store(rig), "window-b"),
        Ok(FIRST_SPACE.to_owned()),
        "a closed window's Space outlived it"
    );
    assert_eq!(
        spaces::window_space(&store(rig), "window-a"),
        Ok(FIRST_SPACE.to_owned())
    );
}
