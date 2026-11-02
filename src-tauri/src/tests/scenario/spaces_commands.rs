//! The login, grant and lease commands as the UI invokes them (ADE-186):
//! each Tauri command runs on a mock app's migrated store, so a command that
//! stops calling its storage, or storage that stops writing, fails here.

use tauri::Manager;

use crate::session_store::SessionStore;
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::grants::{self, RouteGrant};
use crate::spaces::leases;
use crate::spaces::{Provider, FIRST_SPACE};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn work() -> ProviderAccount {
    ProviderAccount {
        provider: Provider::Claude,
        id: "work".into(),
        label: "Work".into(),
    }
}

fn grant(session_id: &str) -> RouteGrant {
    RouteGrant {
        session_id: session_id.into(),
        provider: Provider::Claude,
        from_account_id: "default".into(),
        to_account_id: "work".into(),
        granted_at: 1_000,
        expires_at: 1_000 + 30 * DAY_MS,
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
    let login = SpaceLogin {
        space_id: FIRST_SPACE.into(),
        provider: Provider::Claude,
        account_id: "work".into(),
    };
    account::spaces_login_set(store(), login.clone()).unwrap();
    assert_eq!(
        account::spaces_logins_list(store()).unwrap(),
        [login],
        "a Space's login was not stored or not listed"
    );

    for session in ["s1", "s2"] {
        grants::spaces_grant_save(store(), grant(session)).unwrap();
    }
    assert_eq!(
        grants::spaces_grant_get(store(), "s1".into()).unwrap(),
        Some(grant("s1")),
        "a 30 day grant was not stored or not read back"
    );
    grants::spaces_grant_remove(store(), "s1".into()).unwrap();
    assert_eq!(
        grants::spaces_grant_get(store(), "s1".into()).unwrap(),
        None,
        "a removed grant is still there"
    );
    assert_eq!(
        grants::spaces_grant_get(store(), "s2".into()).unwrap(),
        Some(grant("s2")),
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
/// and each `&&` side); an absolute folder is stored under its `pathKey()`
/// form, listed, and released.
fn the_lease_commands_take_only_absolute_folders(app: &tauri::App<tauri::test::MockRuntime>) {
    let store = || app.state::<SessionStore>();
    for path in ["C:", "C:work", "ab/work", "1:/work"] {
        assert_eq!(
            leases::spaces_lease_take(store(), path.into(), FIRST_SPACE.into()),
            Err(format!("Not an absolute folder: {path}")),
            "leased the folder {path:?}"
        );
    }
    leases::spaces_lease_take(store(), "C:/".into(), FIRST_SPACE.into()).unwrap();
    leases::spaces_lease_take(store(), "/work/x/".into(), FIRST_SPACE.into()).unwrap();
    let leased = || -> Vec<(String, String)> {
        leases::spaces_leases_list(store())
            .unwrap()
            .into_iter()
            .map(|l| (l.checkout_path, l.space_id))
            .collect()
    };
    assert_eq!(
        leased(),
        [
            ("/work/x".to_owned(), FIRST_SPACE.to_owned()),
            ("c:".to_owned(), FIRST_SPACE.to_owned())
        ],
        "a lease was not stored, not listed, or not keyed as pathKey() does"
    );
    for path in ["/work/x", "C:/"] {
        leases::spaces_lease_release(store(), path.into(), FIRST_SPACE.into()).unwrap();
    }
    assert_eq!(leased(), [], "a released lease is still listed");
    assert!(
        leases::spaces_lease_release(store(), "/work/x".into(), FIRST_SPACE.into()).is_err(),
        "released a lease nobody holds"
    );
}
