//! `harness_spawn` demotes the session it starts (nice, and the disk throttle
//! on macOS), not the `--version` probe it waits on. A demoted probe shares
//! the agents' disk throttle, and under load it missed its 15 s cap (ADE-259:
//! "srt timed out").

use std::path::Path;

use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};

const REPORT: &str = "MONOCODE_POLICY_REPORT";

#[cfg(target_os = "macos")]
const IOPOL_THROTTLE: i32 = 3;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getiopolicy_np(kind: i32, scope: i32) -> i32;
}

/// This process's nice and its process-wide disk policy (-1 off macOS).
fn policy() -> (i32, i32) {
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    #[cfg(target_os = "macos")]
    let io = unsafe { getiopolicy_np(0, 0) };
    #[cfg(not(target_os = "macos"))]
    let io = -1;
    (nice, io)
}

/// What the stand-in CLI's child reported to `path`.
fn reported(path: &Path) -> (i32, i32) {
    let text = wait_for(path);
    let mut fields = text.split_whitespace().map(|field| field.parse().ok());
    match (fields.next().flatten(), fields.next().flatten()) {
        (Some(nice), Some(io)) => (nice, io),
        _ => panic!("{}: {text:?}", path.display()),
    }
}

/// Run by the stand-in CLI as its child, which inherits the CLI's policy:
/// writes that policy to the file `REPORT` names.
#[test]
fn policy_report() {
    let Some(file) = std::env::var_os(REPORT) else {
        return;
    };
    let (nice, io) = policy();
    let part = Path::new(&file).with_extension("part");
    std::fs::write(&part, format!("{nice} {io}")).unwrap();
    std::fs::rename(part, file).unwrap();
}

#[test]
fn harness_spawn_demotes_the_session_but_not_the_probe_it_waits_on() {
    let ours = policy();
    assert!(
        ours.0 < 10,
        "the tests run at nice {}; a demoted child cannot be told apart",
        ours.0
    );
    #[cfg(target_os = "macos")]
    assert_ne!(ours.1, IOPOL_THROTTLE, "the tests run disk-throttled");
    let root =
        std::env::temp_dir().join(format!("monocode-scenario-nice-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let probe = root.join("probe-policy");
    let devin = script(
        &root.join("devin"),
        &format!(
            r#"report() {{ {REPORT}="$1" '{}' --exact scenario::spawn_priority::policy_report >/dev/null 2>&1; }}
if [ "$1" = --version ]; then
  report '{}'
  echo 'devin 1.4.2'
  exit 0
fi
report "$PWD/policy""#,
            std::env::current_exe().unwrap().display(),
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

    assert_eq!(reported(&probe), ours, "the --version probe was demoted");
    let (nice, io) = reported(&workdir.join("policy"));
    assert!(nice >= 10, "the session runs at nice {nice}: not demoted");
    #[cfg(target_os = "macos")]
    assert_eq!(io, IOPOL_THROTTLE, "the session's disk is not throttled");
    #[cfg(not(target_os = "macos"))]
    let _ = io;
    std::fs::remove_dir_all(&root).unwrap();
}
