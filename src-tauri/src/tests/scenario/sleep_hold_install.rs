//! Alarms P2: what a spawn's sleep hold install came to reaches its
//! `agent_activity` row in order, a respawn starts it over, and an omp
//! session whose extension stays silent into a turn is missing its hold.

use serde_json::json;

use super::sleep_hold::{chain, hold_state_of, ping, PASS};
use super::world::World;
use crate::mono_messages::SessionId;
use crate::sleep_hold::install::{HoldState, PING_GRACE_MS};

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;

const SILENT: &str = "the omp hold extension did not report in";

/// A `provider` child of `session` spawned now with sleep hold `hold`, read
/// by the tap `harness_spawn` gives it.
fn spawn(world: &World, session: &SessionId, provider: &str, hold: Option<HoldState>) {
    let alarms = &world.store().alarms;
    let tap = alarms.tap(session.as_str(), Some(provider));
    alarms.spawned(session.as_str(), tap.as_ref(), hold);
}

// Defect (H risk 3): a respawn keeps the last child's ping, so a new omp
// whose extension never loaded reads as installed; or a ping that comes
// before the spawn's pass is wiped by the spawn's reset.
#[test]
fn an_omp_respawn_waits_for_its_own_ping() {
    let mut world = World::new("hold-respawn");
    let chain = chain(&world, "hr");
    let awaiting = || Some(HoldState::AwaitingPing);
    spawn(&world, &chain.worker, "omp", awaiting());
    world.alarm_turn_started(&chain.worker, "turn-1");
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(PING_GRACE_MS + PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );

    spawn(&world, &chain.worker, "omp", awaiting());
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "still in its grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: an omp session idle before its first turn reads as missing,
// though the app takes a ping only inside a turn.
#[test]
fn an_omp_session_has_its_grace_from_its_first_turn() {
    let mut world = World::new("hold-first-turn");
    let chain = chain(&world, "hf");
    spawn(&world, &chain.worker, "omp", Some(HoldState::AwaitingPing));
    world.advance(5 * 60 * SECOND);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "idle, never asked"
    );
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "in the turn's grace"
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );
}

// Defect: a Claude or Codex spawn's install result is not what the row
// shows, so a dropped hook never reaches the provisioner.
#[test]
fn a_spawn_records_how_its_hold_went_in() {
    let mut world = World::new("hold-spawn-state");
    let chain = chain(&world, "hs");
    let missing = HoldState::Missing("hooks disabled".into());
    spawn(&world, &chain.worker, "claude", Some(missing));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some("hooks disabled".into()))
    );
    spawn(&world, &chain.worker, "codex", Some(HoldState::Installed));
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );
    spawn(&world, &chain.worker, "cursor", None);
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (None, None),
        "no hold for this harness"
    );
}

/// Each `hold_missing` episode of `session`, oldest first: its close reason.
fn hold_episodes(world: &World, session: &SessionId) -> Vec<Option<String>> {
    world
        .conn()
        .prepare(
            "SELECT close_reason FROM alarm_episodes
              WHERE actor = ?1 AND cause = 'hold_missing' ORDER BY opened_active_ms, id",
        )
        .unwrap()
        .query_map([format!("session:{}", session.as_str())], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// Defect: a session whose hold did not go in is never reported, is
// reported again each pass, or its alarm outlives the hold going in.
#[test]
fn a_missing_hold_alarms_its_provisioner_once_per_spawn() {
    let mut world = World::new("hold-missing-alarm");
    let chain = chain(&world, "hm");
    let disabled = || Some(HoldState::Missing("hooks disabled".into()));
    spawn(&world, &chain.worker, "claude", disabled());
    world.advance(10 * PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    let text: String = world
        .conn()
        .query_row(
            "SELECT p.text FROM alarm_prompts p JOIN alarm_episodes e ON e.id = p.episode_id
              WHERE e.actor = ?1",
            [format!("session:{}", chain.worker.as_str())],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        text.contains(
            "runs without the sleep hold (hooks disabled). Its foreground waits are not held."
        ),
        "{text}"
    );

    spawn(&world, &chain.worker, "claude", Some(HoldState::Installed));
    world.advance(PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into())],
        "closed once the hold is in"
    );
    world.advance(10 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker).len(),
        1,
        "none while installed"
    );

    spawn(&world, &chain.worker, "claude", disabled());
    world.advance(10 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into()), None],
        "the next spawn without the hold is told again, once"
    );
}

// Defect: an omp session whose extension never reports in is not
// reported, or one that reports in late keeps its alarm.
#[test]
fn a_silent_omp_extension_alarms_and_its_late_ping_closes_the_alarm() {
    let mut world = World::new("hold-missing-omp");
    let chain = chain(&world, "ho");
    spawn(&world, &chain.worker, "omp", Some(HoldState::AwaitingPing));
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        Vec::<Option<String>>::new()
    );
    world.advance(3 * PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    world.advance(2 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("work_cleared".into())]
    );
}

// Defect: a hold_missing episode holds the agent's one episode while it
// really stalls, or the spawn's missing hold is told again once the stall
// it gave way to is over.
#[test]
fn a_stall_supersedes_the_missing_hold_alarm_and_the_spawn_is_not_told_again() {
    let mut world = World::new("hold-missing-stall");
    let chain = chain(&world, "hx");
    spawn(
        &world,
        &chain.worker,
        "codex",
        Some(HoldState::Missing("flags refused".into())),
    );
    world.advance(PASS);
    assert_eq!(hold_episodes(&world, &chain.worker), vec![None]);
    // The worker holds its lead's open ask: idle with open work for N.
    world.advance(15 * MINUTE);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("superseded".into())]
    );
    let open: String = world
        .conn()
        .query_row(
            "SELECT cause FROM alarm_episodes WHERE actor = ?1 AND closed_active_ms IS NULL",
            [format!("session:{}", chain.worker.as_str())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open, "idle_open_work");
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(4 * PASS);
    assert_eq!(
        hold_episodes(&world, &chain.worker),
        vec![Some("superseded".into())],
        "the stall is over; this spawn was told already"
    );
}

/// `session` gets a new omp child, waiting for its extension's ping.
fn omp_spawned(world: &World, session: &SessionId) {
    spawn(world, session, "omp", Some(HoldState::AwaitingPing));
}

// Defect (P2 r1 m1): a ping in the last seconds of the grace waits in the
// queue for the next pass while the tick already judged the session
// silent, so its row reads missing.
#[test]
fn a_ping_at_the_end_of_its_grace_is_never_judged_silent() {
    let mut world = World::new("hold-late-ping");
    let chain = chain(&world, "hl");
    // Spawn between two passes, so the grace ends between them too.
    while world.active_ms() % PASS != 2 * SECOND {
        world.advance(SECOND);
    }
    omp_spawned(&world, &chain.worker);
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(PING_GRACE_MS - SECOND);
    assert_eq!(
        ping(&world, &chain.worker),
        json!({"ok": true, "recorded": true})
    );
    for _ in 0..2 * PASS / SECOND {
        world.advance(SECOND);
        assert_ne!(
            hold_state_of(&world, &chain.worker).0.as_deref(),
            Some("missing"),
            "at {}",
            world.active_ms()
        );
    }
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None)
    );
}

// Defect (P2 r1 fable m2): an omp session whose turns all end inside 30 s
// is never judged, so an extension that never loaded reads as pending for
// good; or one that pinged in its short turn reads as missing.
#[test]
fn a_short_turn_with_no_ping_leaves_the_hold_missing() {
    let mut world = World::new("hold-short-turn");
    let chain = chain(&world, "hq");
    omp_spawned(&world, &chain.worker);
    world.alarm_turn_started(&chain.worker, "turn-1");
    world.advance(10 * SECOND);
    world.alarm_turn_ended(&chain.worker, "turn-1");
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("missing".into()), Some(SILENT.into()))
    );

    omp_spawned(&world, &chain.worker);
    world.alarm_turn_started(&chain.worker, "turn-2");
    world.advance(SECOND);
    ping(&world, &chain.worker);
    world.advance(SECOND);
    world.alarm_turn_ended(&chain.worker, "turn-2");
    world.advance(PASS);
    assert_eq!(
        hold_state_of(&world, &chain.worker),
        (Some("installed".into()), None),
        "pinged inside its short turn"
    );
}

/// A canonical scratch folder for one spawn scenario.
#[cfg(unix)]
fn spawn_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("monocode-p2-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

// Defect (P2 r1 M2): in the agent sandbox omp cannot read the hold's
// extension or its timeout overlay in the app data folder, and stops at
// the overlay before any session starts; or srt opens more of the app
// data folder than the folder those two files are alone in.
#[test]
#[cfg(unix)]
fn an_omp_spawn_in_the_agent_sandbox_may_read_only_its_hold_folder() {
    use super::agent_sandbox::{main_window, mock_app, script, stand_in_srt};
    use tauri::Manager;

    let root = spawn_root("srt");
    let data = root.join("com.monocode.p2srt");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("agent-sandbox-on"), b"").unwrap();
    stand_in_srt(&root.join("srt"));
    let omp = script(
        &root.join("omp"),
        r#"[ "$1" = --version ] && { echo 18.2.11; exit 0; }
[ "$1" = --help ] && { echo '--mode=<value> text, json, rpc'; exit 0; }
printf '%s\0' "$@" > argv.part && mv argv.part argv"#,
    )
    .to_string_lossy()
    .into_owned();
    let app = mock_app(&data);
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    crate::harness::harness_spawn(
        app.handle().clone(),
        main_window(&app),
        app.state::<crate::harness::HarnessHost>(),
        "p2srt".into(),
        omp.clone(),
        vec!["--mode".into(), "rpc".into()],
        workdir.to_string_lossy().into_owned(),
        None,
        Some("omp".into()),
        Some(omp),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let argv = super::omp_mcp::argv(&workdir);
    assert_eq!((argv[0].as_str(), argv[2].as_str()), ("-e", "--config"));
    let settings = super::agent_sandbox::wait_for(&workdir.join("srt-settings"));
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
    let reads: std::collections::BTreeSet<&str> = settings["filesystem"]["allowRead"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    let hold = data.join("omp/hold");
    for file in [&argv[1], &argv[3]] {
        assert_eq!(std::path::Path::new(file).parent(), Some(hold.as_path()));
    }
    assert_eq!(
        reads,
        [hold.to_str().unwrap()].into_iter().collect(),
        "the hold's folder, nothing else"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

// Defect (P2 r1 B1): an app-spawned Codex runs on a home without the
// trusted hook (app-server runs no session-flag hook), its databases move
// into the hold's home, or the login's own config is written.
#[test]
#[cfg(unix)]
fn a_codex_spawn_runs_on_the_holds_home_over_its_login() {
    use super::agent_sandbox::{main_window, mock_app, script};
    use tauri::Manager;

    let root = spawn_root("codex");
    let data = root.join("com.monocode.p2codex");
    let app = mock_app(&data);
    let codex = script(
        &root.join("codex"),
        r#"[ "$1" = --version ] && { echo codex-cli 0.165.0; exit 0; }
printf '%s\n' "$CODEX_HOME" "$CODEX_SQLITE_HOME" > env.part && mv env.part env"#,
    )
    .to_string_lossy()
    .into_owned();
    let account = crate::harness::HarnessAccount {
        provider: "codex".into(),
        id: "p2-login".into(),
    };
    let login = crate::harness::provider_account_dir(app.handle(), "codex", Some(&account.id))
        .unwrap()
        .unwrap();
    std::fs::write(login.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
    std::fs::write(login.join("auth.json"), "{}").unwrap();
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    crate::harness::harness_spawn(
        app.handle().clone(),
        main_window(&app),
        app.state::<crate::harness::HarnessHost>(),
        "p2codex".into(),
        codex.clone(),
        vec!["app-server".into()],
        workdir.to_string_lossy().into_owned(),
        Some(account),
        Some("codex".into()),
        Some(codex),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let env = super::agent_sandbox::wait_for(&workdir.join("env"));
    let [home, sqlite] = <[&str; 2]>::try_from(env.lines().collect::<Vec<_>>()).unwrap();
    let home = std::path::Path::new(home);
    assert!(
        home.starts_with(crate::codex_mono_store::homes(&data).unwrap()),
        "{home:?}"
    );
    assert_eq!(std::path::Path::new(sqlite), login);
    assert_eq!(
        std::fs::read_link(home.join("auth.json")).unwrap(),
        login.join("auth.json")
    );
    let config: toml::Table = std::fs::read_to_string(home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(config["model"].as_str(), Some("gpt-5"));
    let key = format!("{}:pre_tool_use:0:0", home.join("config.toml").display());
    assert!(
        config["hooks"]["state"][&key]["trusted_hash"].is_str(),
        "{config:?}"
    );
    assert_eq!(
        std::fs::read_to_string(login.join("config.toml")).unwrap(),
        "model = \"gpt-5\"\n"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
