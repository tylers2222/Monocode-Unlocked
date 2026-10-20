use serde_json::{json, Value};

use super::world::World;
use crate::mono_gate::{self, Caller, CallerKind, Speaker};
use crate::mono_messages::{
    self, CallerArg, LiveSession, MonoId, RequestId, SessionId, Submitted, Tool,
};
use crate::mono_roster::{self, MonoCreate, MonoRole};
use crate::session_store::{self, SessionUpsert};

/// A Mono or Giga as real rows: its `monos` row and its chat's `sessions`
/// row, written through the roster's and the store's own write paths.
pub(crate) struct Mono {
    pub(crate) id: MonoId,
    pub(crate) chat: SessionId,
}

impl World {
    /// Adds a Mono (or a Giga, by `role`) with chat `<id>-chat`.
    pub(crate) fn add_mono(&self, id: &str, role: MonoRole) -> Mono {
        let chat = format!("{id}-chat");
        let conn = self.conn();
        let mono: MonoCreate = serde_json::from_value(json!({
            "id": id,
            "role": role,
            "data": {"name": id, "sessionId": chat, "projects": []},
            "sortOrder": 0,
        }))
        .unwrap();
        mono_roster::create(&conn, &mono_roster::validate_create(mono).unwrap()).unwrap();
        let session: SessionUpsert = serde_json::from_value(json!({
            "id": chat,
            "cwd": "/",
            "harness": "codex",
            "model": "codex:default",
            "modelSettings": {},
            "runtimeMode": "auto",
            "title": id,
            "blocks": [],
        }))
        .unwrap();
        session_store::upsert_session(&conn, &session).unwrap();
        Mono {
            id: MonoId::parse(id).unwrap(),
            chat: SessionId::parse(&chat).unwrap(),
        }
    }

    /// Adds a project session in `cwd` through the store's write path. A
    /// worker names its `lead`, which the store keeps on insert only.
    pub(crate) fn add_session(&self, id: &str, cwd: &str, lead: Option<&str>) -> SessionId {
        let session: SessionUpsert = serde_json::from_value(json!({
            "id": id,
            "cwd": cwd,
            "harness": "codex",
            "model": "codex:default",
            "modelSettings": {},
            "runtimeMode": "auto",
            "title": id,
            "blocks": [],
            "leadSessionId": lead,
        }))
        .unwrap();
        session_store::upsert_session(&self.conn(), &session).unwrap();
        SessionId::parse(id).unwrap()
    }

    /// `messages.send` or `messages.reply` from `from`'s chat in turn `turn`,
    /// through `submit_and_recheck`, the path `mono_message_submit` takes.
    /// `to` is the chat the window lists as live (steerable codex).
    pub(crate) fn call(
        &self,
        from: &Mono,
        turn: &str,
        request_id: &str,
        tool: Tool,
        input: Value,
        to: &Mono,
    ) -> Submitted {
        let caller = Caller {
            speaker: Speaker::Mono {
                kind: CallerKind::MonoChat,
                mono_id: from.id.clone(),
            },
            session_id: from.chat.clone(),
            turn_id: turn.into(),
        };
        self.submit(&caller, request_id, tool, input, &[&to.chat])
    }

    /// A call from a session, with `caller` as the web app sends it
    /// (`mono_message_submit`'s `caller` argument), parsed as the command
    /// parses it. `Err` is the command's error for a caller it refuses.
    pub(crate) fn session_call(
        &self,
        caller: Value,
        request_id: &str,
        tool: Tool,
        input: Value,
        live: &[&SessionId],
    ) -> Result<Submitted, String> {
        let caller = serde_json::from_value::<CallerArg>(caller)
            .map_err(|error| error.to_string())?
            .parse()?;
        Ok(self.submit(&caller, request_id, tool, input, live))
    }

    fn submit(
        &self,
        caller: &Caller,
        request_id: &str,
        tool: Tool,
        input: Value,
        live: &[&SessionId],
    ) -> Submitted {
        let live: Vec<LiveSession> = live
            .iter()
            .map(|session| LiveSession {
                session_id: session.as_str().into(),
                steerable: true,
                own_project: true,
            })
            .collect();
        let (submitted, _) = mono_messages::submit_and_recheck(
            self.store(),
            || self.running(),
            caller,
            &RequestId::parse(request_id).unwrap(),
            mono_gate::parse(tool, &input),
            &live,
            &["codex".to_string()],
            self.now(),
        )
        .unwrap();
        submitted
    }
}
