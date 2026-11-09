//! Which Space a session or Mono is in (ADE-172, I11). A new row joins its
//! parent's Space, else the Space of the window that saves it (ADE-244); an
//! agent's action uses the agent's Space. Its caller never names it.

use rusqlite::{params, Connection, OptionalExtension};

/// The Space row `id` is in, or joins when it is new: a saved session's own,
/// else its parent's (the lead that started it, the lead whose orchestration run
/// lists it, the Mono whose chat it is, or the session its
/// `app-<parent>-<request>` id names, as a Mono chat names its leads and habit
/// runs), else, for an automation run, which has
/// no Mono owner, its automation's project's Space (`project_space`), else
/// `window`, the Space of the window that saves it or acts for it
/// ([`super::window_space`]), else, for a window that shows no Space yet,
/// the app's active Space. A session or parent with no row yet is in the Space
/// of its first spawn ([`remember_first_spawn`]), so an agent's `sessions.start`
/// from an unsaved lead joins the lead's Space whatever its window shows now.
/// The session and Mono inserts store it; the spawn hook reads it for a
/// session that may not be saved yet.
pub(crate) fn space_for_row(
    conn: &Connection,
    id: &str,
    lead: Option<&str>,
    window: Option<&str>,
) -> rusqlite::Result<String> {
    let run_lead = crate::session_store::worker_parent(conn, id)?;
    let parents = [id]
        .into_iter()
        .chain(lead)
        .chain(run_lead.as_deref())
        .chain(named_parents(id));
    for parent in parents {
        let space = conn
            .query_row(
                "SELECT space_id FROM sessions WHERE id = ?1
                 UNION ALL
                 SELECT space_id FROM monos WHERE json_extract(data_json, '$.sessionId') = ?1
                 UNION ALL
                 SELECT space_id FROM temp.first_spawn_spaces WHERE session_id = ?1
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
    if let Some(window) = window {
        return Ok(window.to_owned());
    }
    conn.query_row(
        "SELECT active_space_id FROM space_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// Pins an unsaved session to the Space its own first child starts in, so its
/// row, and a row its agent starts, join that Space even if the window has
/// switched by then. A helper does not pin: it is judged by the window that
/// calls. A window that shows no Space has none to pin. It lasts as long as the
/// app runs; a saved row needs no pin.
pub(crate) fn remember_first_spawn(
    conn: &Connection,
    session: &str,
    window: &str,
) -> rusqlite::Result<()> {
    let session = session.split('#').next().unwrap_or(session);
    let space = space_for_row(conn, session, None, Some(window))?;
    conn.execute(
        "INSERT OR IGNORE INTO temp.first_spawn_spaces (session_id, space_id)
         SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
        params![session, space],
    )?;
    Ok(())
}

/// The Space an action of `caller`, an agent's session, runs in (G8, R15): the
/// caller's, whatever the window it comes through shows. With no caller the
/// action is the window's.
pub(crate) fn acting_space(
    conn: &Connection,
    caller: Option<&str>,
    window: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    match caller {
        Some(caller) => space_for_row(conn, caller, None, window).map(Some),
        None => Ok(window.map(str::to_owned)),
    }
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
