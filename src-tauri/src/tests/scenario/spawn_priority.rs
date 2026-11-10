//! `harness_spawn` demotes the session it starts, not the `--version` probe
//! it waits on. A demoted probe shares the agents' disk throttle on macOS,
//! and under load it missed its 15 s cap (ADE-259: "srt timed out").

use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};

/// The niceness a stand-in CLI wrote to `path`.
fn nice(path: &std::path::Path) -> i32 {
    let text = wait_for(path);
    text.trim()
        .parse()
        .unwrap_or_else(|_| panic!("{}: {text:?}", path.display()))
}

#[test]
fn harness_spawn_demotes_the_session_but_not_the_probe_it_waits_on() {
    let ours = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    assert!(
        ours < 10,
        "the tests run at nice {ours}; a demoted child cannot be told apart"
    );
    let root =
        std::env::temp_dir().join(format!("monocode-scenario-nice-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let probe = root.join("probe-nice");
    let devin = script(
        &root.join("devin"),
        &format!(
            r#"if [ "$1" = --version ]; then
  ps -o ni= -p $$ > '{}'
  echo 'devin 1.4.2'
  exit 0
fi
ps -o ni= -p $$ > nice.part && mv nice.part nice"#,
            probe.display()
        ),
    )
    .to_string_lossy()
    .into_owned();
    let app = mock_app(&root.join("com.monocode.nice"));
    let workdir = root.join("session");
    std::fs::create_dir_all(&workdir).unwrap();
    crate::harness::harness_spawn(
        app.handle().clone(),
        crate::scenario::agent_sandbox::main_window(&app),
        app.state::<crate::harness::HarnessHost>(),
        "session".into(),
        devin.clone(),
        vec!["acp".into()],
        workdir.to_string_lossy().into_owned(),
        None,
        Some("devin".into()),
        Some(devin),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    assert_eq!(nice(&probe), ours, "the --version probe was demoted");
    assert!(
        nice(&workdir.join("nice")) >= 10,
        "the session was not demoted"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
