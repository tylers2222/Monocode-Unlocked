//! Which Space a session or Mono is in (ADE-172). A new row joins its
//! parent's Space, else the active one; its caller never names it.

use rusqlite::{Connection, OptionalExtension};

/// The Space row `id` is in, or joins when it is new: a saved session's own,
/// else its parent's (the lead that started it, the Mono whose chat it is,
/// or the session its `app-<parent>-<request>` id names, as a Mono chat
/// names its leads and habit runs), else the active Space. The session and
/// Mono inserts store it; the spawn hook reads it for a session that may not
/// be saved yet.
pub(crate) fn space_for_row(
    conn: &Connection,
    id: &str,
    lead: Option<&str>,
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
    conn.query_row(
        "SELECT active_space_id FROM space_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// The parents an `app-<parent>-<request>` id may name, longest first: a
/// parent id holds dashes of its own, so each dash may end it.
fn named_parents(id: &str) -> impl Iterator<Item = &str> {
    let rest = id.strip_prefix("app-").unwrap_or_default();
    rest.rmatch_indices('-').map(move |(end, _)| &rest[..end])
}
