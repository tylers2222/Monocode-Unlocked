//! I11, G8: an agent's action uses the agent's Space, not the Space its window
//! shows now, and never the app's active Space. Proof R10, R15, and General's
//! ruling on a `sessions.start` from a lead that is not saved yet (ADE-249). On
//! an in-memory store with the real functions; no folder is touched.

use std::path::PathBuf;

use serde_json::json;
use tauri::test::MockRuntime;
use tauri::{App, Manager, WebviewWindow};

use crate::harness::HarnessAccount;
use crate::mono_roster::{self, MonoCreate};
use crate::session_store::{self, SessionStore, SessionUpsert};
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::{
    self, prepare_spawn, Child, Provider, SettingsPatch, SpawnTarget, FIRST_SPACE,
};

const TWO: &str = "space-2";
const TWO_LOGIN: &str = "work2";

/// A store with Space 2 on its own Claude login. Space 1 keeps the CLI's.
fn store() -> SessionStore {
    let store = SessionStore::open_in_memory().unwrap();
    spaces::create(&store, "Space 2", None, 1).unwrap();
    let login = ProviderAccount {
        provider: Provider::Claude,
        id: TWO_LOGIN.into(),
        label: "Work".into(),
    };
    account::save_account(&store, &login, 0).unwrap();
    account::set_login(
        &store,
        &SpaceLogin {
            space_id: TWO.into(),
            provider: Provider::Claude,
            account_id: TWO_LOGIN.into(),
        },
    )
    .unwrap();
    store
}

/// The login a session's own child starts with, from a window on `window`.
fn first_spawn(store: &SessionStore, session: &str, window: Option<&str>) -> String {
    let target = SpawnTarget {
        workdir: PathBuf::from("/ade263-no-such-folder"),
        account: Some(HarnessAccount {
            provider: "claude".into(),
            id: "default".into(),
        }),
    };
    let spawned = prepare_spawn(
        store,
        Child::Session(session),
        window,
        Some(Provider::Claude),
        1,
        target,
    )
    .unwrap();
    spawned.account.unwrap().id
}

/// What a window on `window` saves of session `id` with lead `lead`; its Space.
fn save(store: &SessionStore, id: &str, lead: Option<&str>, window: Option<&str>) -> String {
    let session: SessionUpsert = serde_json::from_value(json!({
        "id": id,
        "cwd": "/ade263-no-such-folder",
        "harness": "claude",
        "model": "claude:default",
        "modelSettings": {},
        "runtimeMode": "auto",
        "title": id,
        "blocks": [{"id": "u", "role": "user", "text": "Start"}],
        "leadSessionId": lead,
    }))
    .unwrap();
    let conn = store.lock_conn().unwrap();
    session_store::upsert_session(&conn, &session, window).unwrap();
    conn.query_row("SELECT space_id FROM sessions WHERE id = ?1", [id], |row| {
        row.get(0)
    })
    .unwrap()
}

/// General's ruling. Defect: a `sessions.start` from a Space 2 lead that is not
/// saved yet joins the window's Space or the app's active one, or its first spawn
/// takes another Space's login than its row's; or the lead's own first save
/// leaves the Space its first child ran in. Each is tested with the window on
/// Space 1 and the app's active Space on Space 1 too.
#[test]
fn a_start_from_an_unsaved_lead_joins_the_leads_space_and_its_login() {
    let store = store();
    assert_eq!(
        first_spawn(&store, "lead#1", Some(TWO)),
        TWO_LOGIN,
        "the lead's child runs while the window shows Space 2"
    );
    let window = Some(FIRST_SPACE);

    assert_eq!(
        first_spawn(&store, "app-lead-r1", window),
        TWO_LOGIN,
        "a worker's first spawn before its save"
    );
    assert_eq!(save(&store, "app-lead-r1", Some("lead"), window), TWO);
    assert_eq!(
        first_spawn(&store, "app-lead-r1", window),
        TWO_LOGIN,
        "its first spawn uses its row's login"
    );

    assert_eq!(
        save(&store, "named-by-lead", Some("lead"), window),
        TWO,
        "a row that names the lead, with no app- id"
    );
    assert_eq!(first_spawn(&store, "named-by-lead", window), TWO_LOGIN);

    assert_eq!(
        save(&store, "lead", None, window),
        TWO,
        "the lead's own first save, after the window switched"
    );
    assert_eq!(
        first_spawn(&store, "lead", window),
        TWO_LOGIN,
        "and it stays"
    );

    assert_eq!(
        first_spawn(&store, "again", Some(TWO)),
        TWO_LOGIN,
        "a pin's own first spawn"
    );
    assert_eq!(
        first_spawn(&store, "again", window),
        TWO_LOGIN,
        "a second spawn keeps the first spawn's Space"
    );
}

/// Defect: a helper pins its owner, so a picker in one window decides the Space
/// of a tab another window then runs; or a window that shows no Space pins the
/// app's active Space for good.
#[test]
fn only_a_sessions_own_child_from_a_window_with_a_space_pins_it() {
    let store = store();
    let target = || SpawnTarget {
        workdir: PathBuf::from("/ade263-no-such-folder"),
        account: None,
    };
    prepare_spawn(
        &store,
        Child::Helper {
            key: None,
            owner: Some("tab"),
            neutral: std::path::Path::new("/ade263-neutral"),
        },
        Some(TWO),
        None,
        1,
        target(),
    )
    .unwrap();
    assert_eq!(
        save(&store, "tab", None, Some(FIRST_SPACE)),
        FIRST_SPACE,
        "a helper pinned its owner"
    );
    first_spawn(&store, "no-window", None);
    assert_eq!(
        save(&store, "no-window", None, Some(TWO)),
        TWO,
        "a window with no Space pinned the active Space"
    );
}

/// Defect (I11 order): a row with no parent and no spawn takes the app's active
/// Space while a window exists, or the active Space stops being the last
/// fallback for a window that shows no Space; or a pin outranks a saved row.
#[test]
fn the_window_comes_before_the_active_space_and_a_saved_row_before_a_pin() {
    let store = store();
    let active_two = SettingsPatch {
        active_space_id: Some(TWO.into()),
        ..SettingsPatch::default()
    };
    spaces::set_settings(&store, &active_two).unwrap();
    let window = Some(FIRST_SPACE);
    assert_eq!(save(&store, "app-gone-r1", None, window), FIRST_SPACE);
    assert_eq!(save(&store, "of-gone", Some("gone"), window), FIRST_SPACE);
    assert_eq!(save(&store, "no-parent", None, window), FIRST_SPACE);
    assert_eq!(save(&store, "no-window", None, None), TWO, "last fallback");

    first_spawn(&store, "no-parent", Some(TWO));
    assert_eq!(
        save(&store, "no-parent", None, Some(TWO)),
        FIRST_SPACE,
        "a saved row keeps its Space, and writes no pin"
    );
    let pins: i64 = store
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM temp.first_spawn_spaces WHERE session_id = 'no-parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pins, 0);
}

fn mock_app() -> App<MockRuntime> {
    tauri::test::mock_builder()
        .manage(store())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap()
}

fn window_on(app: &App<MockRuntime>, label: &str, space: &str) -> WebviewWindow<MockRuntime> {
    let window = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .build()
        .unwrap();
    let patch = SettingsPatch {
        active_space_id: Some(space.into()),
        ..SettingsPatch::default()
    };
    spaces::spaces_settings_set(window.clone(), app.state(), patch).unwrap();
    window
}

fn create_mono(
    app: &App<MockRuntime>,
    window: &WebviewWindow<MockRuntime>,
    id: &str,
    caller: Option<&str>,
) -> String {
    let mono: MonoCreate = serde_json::from_value(json!({
        "id": id,
        "role": "mono",
        "data": {"name": id, "sessionId": format!("{id}-chat")},
        "sortOrder": 0,
    }))
    .unwrap();
    let roster = mono_roster::monos_create(
        app.handle().clone(),
        window.clone(),
        app.state(),
        mono,
        caller.map(str::to_owned),
    )
    .unwrap();
    roster
        .monos
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
        .space_id
}

/// R10, R15, G8. Defect: a Mono a Giga creates joins the Space the window shows,
/// not the Giga's, so its first chat runs on another Space's login; or a Mono the
/// user creates stops following the window; or an unknown caller's action fails
/// instead of being the window's.
#[test]
fn a_mono_an_agent_creates_joins_the_agents_space_and_a_users_the_windows() {
    let app = mock_app();
    let two = window_on(&app, "two", TWO);
    let one = window_on(&app, "one", FIRST_SPACE);
    let store: tauri::State<'_, SessionStore> = app.state();
    assert_eq!(
        save(&store, "general-chat", None, Some(TWO)),
        TWO,
        "General's chat is Space 2's"
    );

    assert_eq!(
        create_mono(&app, &one, "made-by-general", Some("general-chat")),
        TWO,
        "R15: the window shows Space 1, General is in Space 2"
    );
    assert_eq!(
        save(&store, "made-by-general-chat", None, Some(FIRST_SPACE)),
        TWO,
        "its chat joins it"
    );
    assert_eq!(
        first_spawn(&store, "made-by-general-chat", Some(FIRST_SPACE)),
        TWO_LOGIN,
        "R15: the first spawn uses its row's login"
    );

    assert_eq!(
        create_mono(&app, &one, "user-one", None),
        FIRST_SPACE,
        "R10: a user's Mono in window one"
    );
    assert_eq!(
        create_mono(&app, &two, "user-two", None),
        TWO,
        "R10: a user's Mono in window two"
    );
    assert_eq!(
        create_mono(&app, &two, "unknown-caller", Some("nobody")),
        TWO,
        "a caller with no row or spawn is the window's"
    );

    first_spawn(&store, "unsaved-agent", Some(TWO));
    assert_eq!(
        create_mono(&app, &one, "by-unsaved", Some("unsaved-agent")),
        TWO,
        "an agent whose chat is not saved yet"
    );
}
