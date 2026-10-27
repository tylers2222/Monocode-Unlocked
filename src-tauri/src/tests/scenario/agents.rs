use serde_json::{json, Value};

use super::world::World;
use crate::asks::{self, Call};
use crate::mono_gate::{self, Access, Caller, CallerKind, Speaker};
use crate::mono_messages::{
    self, ActionChecked, CallerArg, LiveSession, MessageId, MonoId, Notices, RequestId, SessionId,
    StartAsk, StartRecorded, Submitted, Tool, ToolError,
};
use crate::mono_roster::{self, MonoCreate, MonoRole};
use crate::session_store::{self, SessionUpsert};

/// A Mono or Giga as real rows: its `monos` row and its chat's `sessions`
/// row, written through the roster's and the store's own write paths.
pub(crate) struct Mono {
    pub(crate) id: MonoId,
    pub(crate) chat: SessionId,
}

/// A submitted `sessions.start` as each side sees it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Started {
    /// The starter's answer.
    pub(crate) message_id: MessageId,
    /// What the started session's first prompt carries: the ask the level
    /// check answered, which the web app renders as R6's `<mono_message>`.
    pub(crate) received: Option<StartAsk>,
}

impl World {
    /// Adds a Mono (or a Giga, by `role`) with chat `<id>-chat`.
    pub(crate) fn add_mono(&self, id: &str, role: MonoRole) -> Mono {
        self.add_mono_in(id, role, &[])
    }

    /// `add_mono` with the Mono's `projects`.
    pub(crate) fn add_mono_in(&self, id: &str, role: MonoRole, projects: &[&str]) -> Mono {
        let chat = format!("{id}-chat");
        let conn = self.conn();
        let mono: MonoCreate = serde_json::from_value(json!({
            "id": id,
            "role": role,
            "data": {"name": id, "sessionId": chat, "projects": projects},
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

    /// `mono` starts lead `id` in `cwd`, as [`World::start`] runs it.
    pub(crate) fn start_lead(&self, mono: &Mono, id: &str, cwd: &str) -> SessionId {
        let input = json!({"prompt": "Lead this", "harness": "codex", "model": "codex:default"});
        let caller = mono_caller(mono, "start");
        self.start(&caller, id, cwd, &format!("start-{id}"), &input)
            .unwrap();
        SessionId::parse(id).unwrap()
    }

    /// `caller`'s submitted `sessions.start` of session `id` in `cwd`, as the
    /// web app runs it: the level check (`mono_app_action_check`), the new
    /// session saved with the lead the check names (`acceptQuickLaunch`)
    /// and the check's ask on its first prompt, then its start row
    /// (`mono_message_record_start`). What each side got, or the refusal.
    pub(crate) fn start(
        &self,
        caller: &Caller,
        id: &str,
        cwd: &str,
        request_id: &str,
        input: &Value,
    ) -> Result<Started, ToolError> {
        let session = SessionId::parse(id).unwrap();
        let request_id = RequestId::parse(request_id).unwrap();
        let action = "sessions.start";
        let now = self.now();
        let checked = mono_messages::check_action(
            &self.conn(),
            caller,
            action,
            None,
            Some(&session),
            &request_id,
            input,
            now,
        );
        let (lead, received) = match checked.unwrap() {
            ActionChecked::Allowed {
                lead_session_id,
                ask,
            } => (lead_session_id, ask),
            ActionChecked::Failed { error } => return Err(error),
        };
        self.add_session(id, cwd, lead.as_ref().map(SessionId::as_str));
        let recorded =
            mono_messages::record_start(&self.conn(), caller, &session, &request_id, input, now);
        match recorded.unwrap() {
            StartRecorded::Recorded { message_id } => Ok(Started {
                message_id,
                received,
            }),
            StartRecorded::Failed { error } => Err(error),
        }
    }

    /// `mono` resets its chat as the app's `resetMonoSession` does: the old
    /// chat is deleted (its messages and token end with it, design 7), the
    /// Mono points at chat `chat`, and the new chat is saved.
    pub(crate) fn reset_mono_chat(&self, mono: &Mono, chat: &str) -> Mono {
        {
            let conn = self.conn();
            session_store::delete_session(&conn, mono.chat.as_str(), session_store::Removal::End)
                .unwrap();
            let update = json!({"id": mono.id.as_str(), "patch": {"sessionId": chat}});
            let update = serde_json::from_value(update).unwrap();
            let update = mono_roster::validate_update(update).unwrap();
            mono_roster::update_many(&conn, &[update]).unwrap();
        }
        Mono {
            id: mono.id.clone(),
            chat: self.add_session(chat, "/", None),
        }
    }

    /// Tool `tool` (`ask`, `ack` or a message tool) from `caller`, through
    /// the command's own dispatch (`asks::call`) and `submit_and_recheck`.
    pub(crate) fn tool(
        &self,
        caller: &Caller,
        request_id: &str,
        tool: &str,
        input: Value,
        live: &[&SessionId],
    ) -> Submitted {
        self.tool_noticed(caller, request_id, tool, input, live).0
    }

    /// `tool`, with the chats its `mono-message-notices` event names.
    pub(crate) fn tool_noticed(
        &self,
        caller: &Caller,
        request_id: &str,
        tool: &str,
        input: Value,
        live: &[&SessionId],
    ) -> (Submitted, Notices) {
        self.submit(caller, request_id, asks::call(tool, &input).unwrap(), live)
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
        let input = mono_gate::parse(tool, &input).into();
        self.submit(&mono_caller(from, turn), request_id, input, &[&to.chat])
            .0
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
        let input = mono_gate::parse(tool, &input).into();
        Ok(self.submit(&caller, request_id, input, live).0)
    }

    fn submit(
        &self,
        caller: &Caller,
        request_id: &str,
        input: Call,
        live: &[&SessionId],
    ) -> (Submitted, Notices) {
        let live: Vec<LiveSession> = live
            .iter()
            .map(|session| LiveSession {
                session_id: session.as_str().into(),
                steerable: true,
                own_project: true,
            })
            .collect();
        mono_messages::submit_and_recheck(
            self.store(),
            || self.running(),
            caller,
            &RequestId::parse(request_id).unwrap(),
            input,
            &live,
            &["codex".to_string()],
            self.now(),
        )
        .unwrap()
    }
}

/// `mono`'s chat calling in turn `turn`.
pub(crate) fn mono_caller(mono: &Mono, turn: &str) -> Caller {
    Caller {
        speaker: Speaker::Mono {
            kind: CallerKind::MonoChat,
            mono_id: mono.id.clone(),
        },
        session_id: mono.chat.clone(),
        turn_id: turn.into(),
    }
}

/// Session `session` in `cwd`, which `sessions.start` started, calling.
pub(crate) fn session_caller(session: &SessionId, cwd: &str) -> Caller {
    Caller {
        speaker: Speaker::Session {
            access: Access::Started,
            cwd: cwd.into(),
        },
        session_id: session.clone(),
        turn_id: "t1".into(),
    }
}
