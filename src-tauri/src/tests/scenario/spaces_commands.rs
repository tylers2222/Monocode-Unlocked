//! The login, grant and lease commands as the UI invokes them (ADE-186),
//! and the project's Spaces command (ADE-235): each Tauri command runs on a
//! mock app's migrated store, so a command that stops calling its storage,
//! or storage that stops writing, fails here.

use serde_json::json;
use tauri::Manager;

use crate::mono_roster::{self, MonoCreate};
use crate::session_store::{now_millis, SessionStore};
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, GrantLength, GrantRequest};
use crate::spaces::leases;
use crate::spaces::{self, Provider, FIRST_SPACE};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn work() -> ProviderAccount {
    ProviderAccount {
        provider: Provider::Claude,
        id: "work".into(),
        label: "Work".into(),
    }
}

/// A grant until the first login resets `resets_at`, as the usage-limit
/// card sends it.
fn grant(session_id: &str, resets_at: i64) -> GrantRequest {
    GrantRequest {
        session_id: session_id.into(),
        provider: Provider::Claude,
        from_account_id: "default".into(),
        to_account_id: "work".into(),
        length: GrantLength::UntilReset,
        first_login_resets_at: Some(resets_at),
    }
}

/// Defect: a login, Space login, grant or lease command returns without
/// storing, removing or reading what it was given; removing one grant takes
/// another; or a lease path that is not absolute (a bare drive, a drive with
/// no slash, a slash in the wrong place) is stored.
#[test]
fn the_login_grant_and_lease_commands_store_what_the_ui_sends() {
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open_in_memory().unwrap())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let store = || app.state::<SessionStore>();
    let ids = || -> Vec<(Provider, String)> {
        account::spaces_accounts_list(store())
            .unwrap()
            .into_iter()
            .map(|a| (a.provider, a.id))
            .collect()
    };
    let default = |provider| (provider, "default".to_owned());
    let defaults = [
        default(Provider::Claude),
        default(Provider::Codex),
        default(Provider::Devin),
    ];
    assert_eq!(ids(), defaults, "the login list lost each CLI's own login");

    account::spaces_account_save(store(), work()).unwrap();
    assert!(
        ids().contains(&(Provider::Claude, "work".into())),
        "a saved login is not listed"
    );
    // The card grants only another Space's login (`grant_from_card`): the
    // sessions are in Space 1, so `work` is Space 2's.
    let two = spaces::create(&store(), "Two", None, 0).unwrap();
    let login = SpaceLogin {
        space_id: two.id,
        provider: Provider::Claude,
        account_id: "work".into(),
    };
    account::spaces_login_set(store(), login.clone()).unwrap();
    assert_eq!(
        account::spaces_logins_list(store()).unwrap(),
        [login],
        "a Space's login was not stored or not listed"
    );

    let in_30_days = now_millis() + 30 * DAY_MS;
    let window = crate::scenario::agent_sandbox::main_window(&app);
    let [s1, s2] = ["s1", "s2"].map(|session| {
        grants::spaces_grant_save(window.clone(), store(), grant(session, in_30_days)).unwrap()
    });
    assert_eq!(
        (
            grants::spaces_grant_get(store(), "s1".into()).unwrap(),
            s1.expires_at
        ),
        (Some(s1), in_30_days),
        "a 30 day grant was not stored or not read back"
    );
    grants::spaces_grant_remove(store(), "s1".into()).unwrap();
    assert_eq!(
        grants::spaces_grant_get(store(), "s1".into()).unwrap(),
        None,
        "a removed grant is still there"
    );
    assert_eq!(
        (
            grants::spaces_grant_get(store(), "s2".into()).unwrap(),
            s2.expires_at
        ),
        (Some(s2), in_30_days),
        "removing one session's grant took another's"
    );

    the_lease_commands_take_only_absolute_folders(&app);

    assert_eq!(
        account::spaces_account_remove(store(), Provider::Claude, "default".into()),
        Err("The default provider account cannot be removed".into())
    );
    account::spaces_account_remove(store(), Provider::Claude, "work".into()).unwrap();
    assert_eq!(ids(), defaults, "a removed login is still listed");
    assert_eq!(
        account::spaces_logins_list(store()).unwrap(),
        [],
        "a removed login is still a Space's login"
    );
    assert_eq!(
        grants::spaces_grant_get(store(), "s2".into()).unwrap(),
        None,
        "a grant to a removed login is still there"
    );
}

/// Leases through the commands: a bare drive, a drive with no slash, a slash
/// in the wrong place and a non-letter drive are refused (folder_key's `>`
/// and each `&&` side); a real folder named with a trailing slash is stored
/// under its resolved `pathKey()` form, listed, and released. `C:/` is leased
/// on Windows only; on unix it does not resolve, so it is refused.
fn the_lease_commands_take_only_absolute_folders(app: &tauri::App<tauri::test::MockRuntime>) {
    let store = || app.state::<SessionStore>();
    for path in ["C:", "C:work", "ab/work", "1:/work"] {
        assert_eq!(
            leases::spaces_lease_take(store(), path.into(), FIRST_SPACE.into()),
            Err(format!("Not an absolute folder: {path}")),
            "leased the folder {path:?}"
        );
    }
    let folder = std::env::temp_dir().join(format!("ade173-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let folder_text = folder.to_string_lossy().into_owned();
    // The key as `leases::resolve` makes it: Windows `canonicalize` gives a
    // `\\?\` path, and macOS resolves /var to /private/var.
    let key = {
        let real = std::fs::canonicalize(&folder).unwrap();
        let text = real.to_string_lossy();
        let plain = match text.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => text.trim_start_matches(r"\\?\").to_owned(),
        };
        spaces::folder_key(&plain).unwrap()
    };
    leases::spaces_lease_take(store(), format!("{folder_text}/"), FIRST_SPACE.into()).unwrap();
    #[cfg(unix)]
    assert_eq!(
        leases::spaces_lease_take(store(), "C:/".into(), FIRST_SPACE.into()),
        Err("No such folder: C:/".into()),
        "leased a Windows folder on unix"
    );
    #[cfg(windows)]
    leases::spaces_lease_take(store(), "C:/".into(), FIRST_SPACE.into()).unwrap();
    let leased = || -> Vec<(String, String)> {
        leases::spaces_leases_list(store())
            .unwrap()
            .into_iter()
            .map(|l| (l.checkout_path, l.space_id))
            .collect()
    };
    let mut held = vec![(key, FIRST_SPACE.to_owned())];
    if cfg!(windows) {
        held.push(("c:".to_owned(), FIRST_SPACE.to_owned()));
    }
    held.sort();
    assert_eq!(
        leased(),
        held,
        "a lease was not stored, not listed, or not keyed as pathKey() does"
    );
    leases::spaces_lease_release(store(), folder_text.clone(), FIRST_SPACE.into()).unwrap();
    #[cfg(windows)]
    leases::spaces_lease_release(store(), "C:/".into(), FIRST_SPACE.into()).unwrap();
    assert_eq!(leased(), [], "a released lease is still listed");
    assert!(
        leases::spaces_lease_release(store(), folder_text, FIRST_SPACE.into()).is_err(),
        "released a lease nobody holds"
    );
    std::fs::remove_dir_all(&folder).unwrap();
}

/// Defect: `spaces_project_spaces` answers only the assigned Space (as
/// `spaces_project_get` does) and drops the Space of a Mono that lists the
/// project, or answers a path that is no project key instead of refusing it.
#[test]
fn the_project_spaces_command_lists_every_space_a_project_is_in() {
    let app = tauri::test::mock_builder()
        .manage(SessionStore::open_in_memory().unwrap())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let store = || app.state::<SessionStore>();
    let spaces_of = |path: &str| spaces::spaces_project_spaces(store(), path.into());

    spaces::spaces_project_set(store(), "/work/a".into(), FIRST_SPACE.into()).unwrap();
    assert_eq!(
        spaces_of("/work/a"),
        Ok(vec![FIRST_SPACE.to_owned()]),
        "a project assigned to Space 1 alone is not in Space 1 only"
    );

    let two = spaces::create(&store(), "Two", None, 0).unwrap();
    // A new Mono joins the Space of the window that creates it (`space_for_row`).
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": "two-mono",
        "role": "mono",
        "data": {"name": "Two Mono", "sessionId": "two-mono-chat", "projects": ["/work/a/"]},
        "sortOrder": 0,
    }))
    .unwrap();
    let mono = mono_roster::validate_create(mono).unwrap();
    mono_roster::create(&store().lock_conn().unwrap(), &mono, Some(&two.id)).unwrap();
    assert_eq!(
        spaces_of("/work/a"),
        Ok(vec![FIRST_SPACE.to_owned(), two.id]),
        "the Space of a Mono that lists the project is missing, or the assigned Space is not first"
    );

    assert_eq!(
        spaces_of("work/a"),
        Err("Not an absolute folder: work/a".into()),
        "answered a path that is no project key"
    );
}
