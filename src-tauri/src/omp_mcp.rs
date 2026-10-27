//! A session started with `mcp: "none"` runs omp without MCP servers (ADE-117).
//!
//! omp 18.2.11 has no flag or env var that turns MCP off for one process. Its
//! settings overlay (`--config <file>`) can: discovery drops every item whose
//! extension id is in `disabledExtensions` (`mcp:<server name>`), and
//! `mcp.enableProjectConfig: false` drops every project-level MCP config. An
//! overlay array replaces the user's, so the overlay starts from omp's own
//! effective `disabledExtensions`.

use std::path::Path;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

/// The `--config` arguments that start `omp` with no MCP servers.
pub(crate) fn no_mcp_args<R: Runtime>(
    app: &AppHandle<R>,
    omp: &str,
    workdir: &Path,
) -> Result<Vec<String>, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("omp");
    let path = crate::private_file::write_once(&dir, "no-mcp", &no_mcp_overlay(omp, workdir)?)?;
    Ok(vec!["--config".into(), path.to_string_lossy().into_owned()])
}

fn no_mcp_overlay(omp: &str, workdir: &Path) -> Result<String, String> {
    let setting: Value = serde_json::from_str(&omp_stdout(
        omp,
        workdir,
        &["config", "get", "disabledExtensions", "--json"],
    )?)
    .map_err(|e| format!("omp config get disabledExtensions: {e}"))?;
    let disabled = setting["value"]
        .as_array()
        .ok_or("omp config get disabledExtensions: no list value")?
        .iter()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    let agent_dir = omp_stdout(omp, workdir, &["config", "path"])?;
    Ok(overlay(
        disabled,
        &user_servers(Path::new(agent_dir.trim())),
    ))
}

/// The user's disabled extensions plus `mcp:<name>` for each server.
fn overlay(mut disabled: Vec<String>, servers: &[String]) -> String {
    for name in servers {
        let id = format!("mcp:{name}");
        if !disabled.contains(&id) {
            disabled.push(id);
        }
    }
    json!({ "disabledExtensions": disabled, "mcp": { "enableProjectConfig": false } }).to_string()
}

/// Server names in the MCP files omp reads from its agent folder.
fn user_servers(agent_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for file in ["mcp.json", ".mcp.json"] {
        let config = crate::mcp::read_json(&agent_dir.join(file));
        if let Some(servers) = config
            .as_ref()
            .and_then(|c| c.get("mcpServers")?.as_object())
        {
            names.extend(servers.keys().cloned());
        }
    }
    names
}

fn omp_stdout(omp: &str, workdir: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let output =
        crate::harness::exec_output(omp, &args, workdir.to_str(), crate::harness::EXEC_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!("omp {} failed: {}", args.join(" "), output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("omp {}: {e}", args.join(" ")))
}
