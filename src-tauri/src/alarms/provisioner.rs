//! Design section 5: who started an agent or gave it its work, and so who
//! hears of its stall. `lineage` reads the rows; `provisioner` decides.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};

use crate::ask_gate::Actor;
use crate::mono_gate::SessionLevel;
use crate::mono_messages::{self, MonoId, SessionId};
use crate::mono_roster::MonoRole;

/// The default root Giga (section 5), when `alarms.root_giga` names none.
const ROOT_GIGA_NAME: &str = "Monocode General";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Provisioner {
    Agent(Actor),
    /// The end of every chain: an OS notification, no outbox row.
    Tyler,
}

impl Provisioner {
    pub(crate) fn stored(&self) -> String {
        match self {
            Provisioner::Agent(actor) => actor.to_string(),
            Provisioner::Tyler => "tyler".into(),
        }
    }
}

/// An agent's place in the hierarchy, as its rows give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lineage {
    Worker {
        lead: SessionId,
    },
    /// A lead, or a session the user started: `starter` is the Mono whose
    /// `sessions.start` (or open ask) it carries.
    Lead {
        starter: Option<MonoId>,
    },
    Mono {
        /// The Giga of the newest open ask the Mono holds.
        asker: Option<MonoId>,
        root_giga: Option<MonoId>,
    },
    Giga,
}

pub(crate) fn provisioner(lineage: Lineage) -> Provisioner {
    match lineage {
        Lineage::Worker { lead } => Provisioner::Agent(Actor::Session(lead)),
        Lineage::Lead {
            starter: Some(mono),
        } => Provisioner::Agent(Actor::Mono(mono)),
        Lineage::Mono { asker, root_giga } => {
            asker.or(root_giga).map_or(Provisioner::Tyler, |giga| {
                Provisioner::Agent(Actor::Mono(giga))
            })
        }
        Lineage::Lead { starter: None } | Lineage::Giga => Provisioner::Tyler,
    }
}

/// A Giga's row, in roster order.
pub(crate) struct Giga {
    pub id: MonoId,
    pub name: String,
}

/// The Giga `setting` names, else the one named "Monocode General", else
/// the first; `gigas` is in roster order.
pub(crate) fn root_giga(setting: Option<&str>, gigas: &[Giga]) -> Option<MonoId> {
    let named = |pick: &dyn Fn(&Giga) -> bool| gigas.iter().find(|giga| pick(giga));
    setting
        .and_then(|id| named(&|giga| giga.id.as_str() == id))
        .or_else(|| named(&|giga| giga.name == ROOT_GIGA_NAME))
        .or(gigas.first())
        .map(|giga| giga.id.clone())
}

fn gigas(conn: &Connection) -> rusqlite::Result<Vec<Giga>> {
    conn.prepare(
        "SELECT id, name FROM monos WHERE role = 'giga' ORDER BY sort_order, created_at, id",
    )?
    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))?
    .filter_map(|row| match row {
        Ok((id, name)) => MonoId::parse(&id).map(|id| Ok(Giga { id, name })),
        Err(error) => Some(Err(error)),
    })
    .collect()
}

/// The Mono id that `actor` names in `mono_asks`, from its stored text.
fn mono_actor(stored: Option<String>) -> Option<MonoId> {
    match stored.as_deref().and_then(Actor::parse) {
        Some(Actor::Mono(mono)) => Some(mono),
        Some(Actor::Session(_)) | None => None,
    }
}

/// The Mono that started `session`, or whose open ask it holds.
fn starter(conn: &Connection, session: &SessionId) -> rusqlite::Result<Option<MonoId>> {
    let started: Option<String> = conn
        .query_row(
            "SELECT from_mono_id FROM mono_messages
              WHERE tool = 'start_session' AND status <> 'rejected'
                AND to_session_id = ?1 AND from_mono_id IS NOT NULL
              ORDER BY created_at DESC LIMIT 1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(mono) = started.as_deref().and_then(MonoId::parse) {
        return Ok(Some(mono));
    }
    newest_asker(conn, &Actor::Session(session.clone()), 2)
}

/// The parent of the newest open ask `child` holds from level `level`.
fn newest_asker(conn: &Connection, child: &Actor, level: i64) -> rusqlite::Result<Option<MonoId>> {
    let parent = conn
        .query_row(
            "SELECT parent_actor FROM mono_asks
              WHERE child_actor = ?1 AND state = 'open' AND parent_level = ?2
              ORDER BY created_wall_ms DESC, id DESC LIMIT 1",
            params![child.to_string(), level],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mono_actor(parent))
}

fn archived(conn: &Connection, session: &SessionId) -> rusqlite::Result<bool> {
    let archived: Option<bool> = conn
        .query_row(
            "SELECT archived FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(archived.unwrap_or(true))
}

pub(crate) fn lineage(
    conn: &Connection,
    actor: &Actor,
    root_setting: Option<&str>,
) -> rusqlite::Result<Lineage> {
    Ok(match actor {
        Actor::Session(session) => session_lineage(conn, session)?,
        Actor::Mono(mono) => match mono_messages::mono_row(conn, mono)?.map(|row| row.role) {
            Some(MonoRole::Giga) => Lineage::Giga,
            Some(MonoRole::Mono) | None => Lineage::Mono {
                asker: newest_asker(conn, actor, 1)?,
                root_giga: root_giga(root_setting, &gigas(conn)?),
            },
        },
    })
}

/// An archived lead hears nothing, so its own provisioner does. A cycle of
/// archived leads (only from bad rows) ends at the session that closes it.
fn session_lineage(conn: &Connection, session: &SessionId) -> rusqlite::Result<Lineage> {
    let mut seen = HashSet::new();
    let mut session = session.clone();
    loop {
        seen.insert(session.clone());
        match mono_messages::session_level(conn, &session)? {
            Some(SessionLevel::Worker { lead }) if !archived(conn, &lead)? => {
                return Ok(Lineage::Worker { lead });
            }
            Some(SessionLevel::Worker { lead }) if !seen.contains(&lead) => session = lead,
            Some(SessionLevel::Worker { .. } | SessionLevel::Lead) | None => break,
        }
    }
    Ok(Lineage::Lead {
        starter: starter(conn, &session)?,
    })
}

/// The Mono that owns `actor` (section 8): itself for a Mono, else the Mono
/// that started its lead, else the Mono whose projects hold its folder.
pub(crate) fn owner_mono(conn: &Connection, actor: &Actor) -> rusqlite::Result<Option<MonoId>> {
    let session = match actor {
        Actor::Mono(mono) => return Ok(Some(mono.clone())),
        Actor::Session(session) => session,
    };
    let lead = match mono_messages::session_level(conn, session)? {
        Some(SessionLevel::Worker { lead }) => lead,
        Some(SessionLevel::Lead) | None => session.clone(),
    };
    if let Some(mono) = starter(conn, &lead)? {
        return Ok(Some(mono));
    }
    let cwd: Option<String> = conn
        .query_row(
            "SELECT cwd FROM sessions WHERE id = ?1",
            [lead.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(folder) = cwd.map(|cwd| crate::mono_gate::path_key(&cwd)) else {
        return Ok(None);
    };
    let monos = conn
        .prepare("SELECT id FROM monos WHERE role = 'mono' ORDER BY sort_order, created_at, id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for mono in monos.iter().filter_map(|id| MonoId::parse(id)) {
        if mono_messages::mono_row(conn, &mono)?.is_some_and(|row| row.projects.contains(&folder)) {
            return Ok(Some(mono));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(id: &str) -> MonoId {
        MonoId::parse(id).unwrap()
    }

    fn giga(id: &str, name: &str) -> Giga {
        Giga {
            id: mono(id),
            name: name.into(),
        }
    }

    // Defect: section 5, one case per row: a stall reaches someone other
    // than the agent that started it or gave it the work.
    #[test]
    fn each_agent_reaches_the_provisioner_section_5_names() {
        let lead = SessionId::parse("lead-1").unwrap();
        assert_eq!(
            provisioner(Lineage::Worker { lead: lead.clone() }),
            Provisioner::Agent(Actor::Session(lead)),
            "worker -> its lead"
        );
        assert_eq!(
            provisioner(Lineage::Lead {
                starter: Some(mono("pr-mono"))
            }),
            Provisioner::Agent(Actor::Mono(mono("pr-mono"))),
            "Mono-started lead -> that Mono"
        );
        assert_eq!(
            provisioner(Lineage::Mono {
                asker: Some(mono("fork-giga")),
                root_giga: Some(mono("general"))
            }),
            Provisioner::Agent(Actor::Mono(mono("fork-giga"))),
            "Mono -> the Giga whose open ask it holds"
        );
        assert_eq!(
            provisioner(Lineage::Mono {
                asker: None,
                root_giga: Some(mono("general"))
            }),
            Provisioner::Agent(Actor::Mono(mono("general"))),
            "Mono with no ask -> the root Giga"
        );
        assert_eq!(provisioner(Lineage::Giga), Provisioner::Tyler, "Giga");
        assert_eq!(
            provisioner(Lineage::Lead { starter: None }),
            Provisioner::Tyler,
            "user-started session"
        );
        assert_eq!(
            provisioner(Lineage::Mono {
                asker: None,
                root_giga: None
            }),
            Provisioner::Tyler,
            "a Mono with no Giga at all"
        );
    }

    // Defect: the root Giga fallback ignores the setting, or picks a Giga
    // other than "Monocode General" when the setting is unset.
    #[test]
    fn the_root_giga_is_the_setting_then_monocode_general_then_the_first() {
        let gigas = [
            giga("first", "Fork Orchestrator"),
            giga("general", "Monocode General"),
            giga("other", "Other"),
        ];
        assert_eq!(root_giga(Some("other"), &gigas), Some(mono("other")));
        assert_eq!(root_giga(Some("deleted"), &gigas), Some(mono("general")));
        assert_eq!(root_giga(None, &gigas), Some(mono("general")));
        assert_eq!(root_giga(None, &gigas[..1]), Some(mono("first")));
        assert_eq!(root_giga(None, &[]), None);
    }

    // Defect (S1 r6 n1): a cycle of archived leads in `lead_session_id`
    // (only from bad rows) recurses until the stack overflows, which aborts
    // the app on every pass.
    #[test]
    fn a_cycle_of_archived_leads_ends_at_tyler() {
        let store = crate::session_store::SessionStore::open_in_memory().unwrap();
        let conn = store.lock_conn().unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at,
               updated_at, lead_session_id, archived)
             VALUES ('self', '/', 'claude', 'm', 'default', 'S', 1, 1, 'self', 1),
                    ('a', '/', 'claude', 'm', 'default', 'S', 1, 1, 'b', 1),
                    ('b', '/', 'claude', 'm', 'default', 'S', 1, 1, 'a', 1),
                    ('lead', '/', 'claude', 'm', 'default', 'S', 1, 1, NULL, 0),
                    ('gone', '/', 'claude', 'm', 'default', 'S', 1, 1, 'lead', 1),
                    ('worker', '/', 'claude', 'm', 'default', 'S', 1, 1, 'gone', 0)",
        )
        .unwrap();
        let session = |id: &str| Actor::Session(SessionId::parse(id).unwrap());
        for id in ["self", "a", "b"] {
            assert_eq!(
                lineage(&conn, &session(id), None).unwrap(),
                Lineage::Lead { starter: None },
                "{id}"
            );
        }
        assert_eq!(
            lineage(&conn, &session("worker"), None).unwrap(),
            Lineage::Worker {
                lead: SessionId::parse("lead").unwrap()
            },
            "an archived lead's own lead hears of its worker"
        );
    }
}
