//! ADE-117: `harness_spawn` with `omp_mcp: "none"` starts omp with a settings
//! overlay that turns every MCP server off, and without it starts omp with
//! exactly the arguments it was given.

use std::path::Path;

use serde_json::{json, Value};
use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};

fn argv(workdir: &Path) -> Vec<String> {
    wait_for(&workdir.join("argv"))
        .split_terminator('\0')
        .map(String::from)
        .collect()
}

#[test]
fn harness_spawn_starts_omp_without_mcp_servers_only_when_asked() {
    let root =
        std::env::temp_dir().join(format!("monocode-scenario-ade117-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    // Resolved, as the app's binary resolver hands back a path.
    let root = root.canonicalize().unwrap();
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("mcp.json"),
        r#"{"mcpServers":{"alpha":{"url":"https://a.invalid"}}}"#,
    )
    .unwrap();
    // JSONC, and a server both files name.
    std::fs::write(
        agent.join(".mcp.json"),
        "{ // user servers\n\"mcpServers\":{\"beta\":{\"command\":\"b\"},\"alpha\":{\"command\":\"a\"}}}",
    )
    .unwrap();
    // A stand-in omp: passes the app's binary checks, answers the two
    // `config` reads as omp 18.2.11 does, and otherwise records its argv in
    // the folder it runs in.
    let omp = script(
        &root.join("omp"),
        &format!(
            r#"[ "$1" = --version ] && {{ echo 18.2.11; exit 0; }}
[ "$1" = --help ] && {{ echo '--mode=<value> text, json, rpc'; exit 0; }}
[ "$1 $2" = "config get" ] && {{ echo '{{"key":"disabledExtensions","value":["skill:made-up","mcp:beta"],"type":"array"}}'; exit 0; }}
[ "$1 $2" = "config path" ] && {{ echo '{}'; exit 0; }}
printf '%s\0' "$@" > argv.part && mv argv.part argv"#,
            agent.display()
        ),
    )
    .to_string_lossy()
    .into_owned();
    let app = mock_app(&root.join("com.monocode.ade117"));
    let spawn = |session: &str, mcp: Option<&str>| {
        let workdir = root.join(session);
        std::fs::create_dir_all(&workdir).unwrap();
        crate::harness::harness_spawn(
            app.handle().clone(),
            app.state::<crate::harness::HarnessHost>(),
            session.into(),
            omp.clone(),
            vec![
                "--mode".into(),
                "rpc".into(),
                "--resume".into(),
                "abc".into(),
            ],
            workdir.to_string_lossy().into_owned(),
            None,
            Some("omp".into()),
            Some(omp.clone()),
            None,
            None,
            mcp.map(String::from),
        )
        .map(|_| workdir)
    };
    let today = ["--mode", "rpc", "--resume", "abc"];

    assert_eq!(argv(&spawn("default", None).unwrap()), today);

    let none = argv(&spawn("none", Some("none")).unwrap());
    assert_eq!(none[0], "--config");
    assert_eq!(none[2..], today);
    let overlay: Value = serde_json::from_str(&std::fs::read_to_string(&none[1]).unwrap()).unwrap();
    assert_eq!(
        overlay,
        json!({
            "disabledExtensions": ["skill:made-up", "mcp:beta", "mcp:alpha"],
            "mcp": { "enableProjectConfig": false }
        })
    );

    assert!(
        spawn("other", Some("all")).is_err(),
        "an unknown choice is refused"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
