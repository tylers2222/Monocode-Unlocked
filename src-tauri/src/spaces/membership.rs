//! Which Space a session or Mono is in (ADE-172). A new row joins its
//! parent's Space, else the Space of the window that saves it (ADE-244); its
//! caller never names it.

use rusqlite::{Connection, OptionalExtension};

/// The Space row `id` is in, or joins when it is new: a saved session's own,
/// else its parent's (the lead that started it, the Mono whose chat it is,
/// or the session its `app-<parent>-<request>` id names, as a Mono chat
/// names its leads and habit runs), else, for an automation run, which has
/// no Mono owner, its automation's project's Space (`project_space`), else
/// `window`, the Space of the window that saves it or acts for it
/// ([`super::window_space`]). A row an agent started through `sessions.start`
/// (it names a lead, or has an `app-<lead>-<request>` id) whose lead is not
/// saved joins the app's active Space instead, as before ADE-244: the agent
/// CLI and MCP have no window. The session and Mono inserts store it; the
/// spawn hook reads it for a session that may not be saved yet.
pub(crate) fn space_for_row(
    conn: &Connection,
    id: &str,
    lead: Option<&str>,
    window: &str,
) -> rusqlite::Result<String> {
    for parent in [id].into_iter().chain(lead).chain(named_parents(id)) {
        let space = conn
            .query_row(
                "SELECT space_id FROM sessions WHERE id = ?1
                 UNION ALL
                 SELECT space_id FROM monos WHERE json_extract(data_json, '$.sessionId') = ?1
                 LIMIT 1",
                [parent],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(space) = space {
            return Ok(space);
        }
    }
    if let Some(project) = automation_project(conn, id)? {
        return super::project_space_in(conn, &project)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()));
    }
    if lead.is_none() && named_parents(id).next().is_none() {
        return Ok(window.to_owned());
    }
    conn.query_row(
        "SELECT active_space_id FROM space_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// The project of the automation session `id` runs for, from saved rows: the
/// newest run that names it (`run_json.sessionId`, saved before the run's
/// first turn) and its automation's folder.
pub(super) fn automation_project(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    let project: Option<Option<String>> = conn
        .query_row(
            "SELECT json_extract(a.definition_json, '$.cwd')
               FROM automation_runs r JOIN automations a ON a.id = r.automation_id
              WHERE json_extract(r.run_json, '$.sessionId') = ?1
              ORDER BY r.created_at DESC
              LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(project.flatten())
}

/// The parents an `app-<parent>-<request>` id may name, longest first: a
/// parent id holds dashes of its own, so each dash may end it.
fn named_parents(id: &str) -> impl Iterator<Item = &str> {
    let rest = id.strip_prefix("app-").unwrap_or_default();
    rest.rmatch_indices('-').map(move |(end, _)| &rest[..end])
}
