//! `harness_spawn` for a non-omp CLI: a Supervised `devin acp` child starts
//! with the ask-before-edits config under the app data folder, a config that
//! cannot be written stops the start, and omp's `mcp: "none"` is refused.

use std::path::Path;

use serde_json::Value;
use tauri::Manager;

use super::agent_sandbox::{main_window, mock_app, script};
use super::omp_mcp::argv;

#[test]
fn harness_spawn_gives_devin_only_the_launch_flags_it_supports() {
    let root =
        std::env::temp_dir().join(format!("monocode-scenario-devin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // A stand-in devin: passes the version check and records its argv in the
    // folder it runs in, for the child and for any probe the app runs.
    let devin = script(
        &root.join("devin"),
        r#"[ "$1" = --version ] && { echo 'devin 1.4.2'; exit 0; }
printf '%s\0' "$@" > argv.part && mv argv.part argv"#,
    )
    .to_string_lossy()
    .into_owned();
    let data = root.join("com.monocode.devin");
    let app = mock_app(&data);
    let window = main_window(&app);
    let spawn = |session: &str, ask_edits: Option<bool>, mcp: Option<&str>| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        crate::harness::harness_spawn(
            app.handle().clone(),
            window.clone(),
            app.state::<crate::harness::HarnessHost>(),
            session.into(),
            devin.clone(),
            vec!["acp".into()],
            workdir.to_string_lossy().into_owned(),
            None,
            Some("devin".into()),
            Some(devin.clone()),
            None,
            ask_edits,
            mcp.map(String::from),
        )
        .map(|_| workdir)
    };

    let supervised = argv(&spawn("supervised", Some(true), None).unwrap());
    assert_eq!(supervised[0], "--config");
    assert_eq!(supervised[2..], ["acp"]);
    let config = Path::new(&supervised[1]);
    assert_eq!(config.parent(), Some(data.join("devin").as_path()));
    let value: Value = serde_json::from_str(&std::fs::read_to_string(config).unwrap()).unwrap();
    assert!(
        value["permissions"]["ask"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule == crate::devin_config::ASK_EDITS_RULE),
        "the config asks before every write: {value}"
    );

    // Something other than a file holds the config's path.
    std::fs::remove_file(config).unwrap();
    std::fs::create_dir(config).unwrap();
    assert!(
        spawn("blocked", Some(true), None).is_err(),
        "a config the app cannot write stops the start"
    );

    assert!(
        spawn("mcp", None, Some("none")).is_err(),
        "mcp \"none\" is refused for a non-omp CLI"
    );
    assert!(
        !root.join("mcp").join("argv").exists(),
        "nothing runs the CLI for an omp-only choice"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
