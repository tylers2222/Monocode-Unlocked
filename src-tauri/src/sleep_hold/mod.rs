//! Sleep hold (alarms design 10 and 11): an agent that wants to wait in the
//! foreground asks its provisioner first. `<exe> hold --harness ...` runs as
//! the harness's pre-tool hook; this module holds the hook payloads it reads
//! and the `hold` namespace messages it exchanges with the app.
pub mod classify;
mod claude_settings;
pub mod client;
mod codex_home;
pub(crate) mod install;
pub(crate) mod service;
pub(crate) mod store;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use classify::{classify, Wait, WaitKind};

/// How long a hold takes an answer, from its request. The client's last
/// wait ends here and the app closes the hold here, so a logged decision
/// is always one the client received (lead ruling, H risk 1).
pub const ANSWER_CUT: Duration = Duration::from_secs(115);
/// The longest `hold.wait`: the client asks for at most this, the app
/// serves at most this inside its 35 s reply window and refuses more.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Omp,
}

impl Harness {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "omp" => Some(Self::Omp),
            _ => None,
        }
    }
}

/// `hold.request` input. The app keys the hold on `tool_use_id`, so a
/// repeated request returns the same hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldRequest {
    pub harness: Harness,
    pub session_hint: Option<String>,
    pub tool_use_id: String,
    pub command: String,
    pub background: bool,
    pub reason: Option<String>,
    pub wait: Wait,
}

/// `hold.request` answer: `{"ok":true,"hold":"sh_K1"}`.
#[derive(Debug, Deserialize)]
pub struct HoldRequested {
    pub hold: String,
}

/// `hold.wait` input: block up to `timeout_seconds` for a decision.
/// `last`: the client sends no wait after this one, so the app closes the
/// hold at its end and no answer lands once the client stopped listening.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldWait<'a> {
    pub hold: &'a str,
    pub timeout_seconds: u64,
    pub last: bool,
}

/// `hold.wait` answer: `{"ok":true,"status":"pending"}` and so on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HoldWaitReply {
    Pending,
    Allow,
    Deny {
        #[serde(default)]
        reason: Option<String>,
    },
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCall {
    pub session_hint: Option<String>,
    pub tool_use_id: String,
    pub command: String,
    pub background: bool,
    pub reason: Option<String>,
}

/// Claude Code and Codex PreToolUse stdin. Codex sends `tool_name: "Bash"`
/// with `tool_input: {command}` for exec_command (codex-rs core
/// tools/handlers/unified_exec/exec_command.rs, hook_runtime.rs).
#[derive(Deserialize)]
struct PreToolUse {
    session_id: Option<String>,
    tool_use_id: Option<String>,
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
    description: Option<String>,
    #[serde(default)]
    run_in_background: bool,
}

/// What the omp extension sends for each bash tool call.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OmpCall {
    session_id: Option<String>,
    tool_call_id: String,
    command: String,
    #[serde(default, rename = "async")]
    run_async: bool,
}

/// The shell call in a hook payload, or `None` for any other tool.
pub fn parse_payload(harness: Harness, stdin: &str) -> Result<Option<ShellCall>, String> {
    let invalid = |error: serde_json::Error| format!("unreadable hook payload: {error}");
    if harness == Harness::Omp {
        let call: OmpCall = serde_json::from_str(stdin).map_err(invalid)?;
        return Ok(Some(ShellCall {
            session_hint: call.session_id,
            tool_use_id: call.tool_call_id,
            command: call.command,
            background: call.run_async,
            reason: None,
        }));
    }
    let payload: PreToolUse = serde_json::from_str(stdin).map_err(invalid)?;
    if payload.tool_name != "Bash" {
        return Ok(None);
    }
    let input: BashInput = serde_json::from_value(payload.tool_input).map_err(invalid)?;
    let tool_use_id = payload
        .tool_use_id
        .ok_or("unreadable hook payload: no tool_use_id")?;
    let claude = harness == Harness::Claude;
    Ok(Some(ShellCall {
        session_hint: payload.session_id,
        tool_use_id,
        command: input.command,
        background: claude && input.run_in_background,
        reason: input.description.filter(|_| claude),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(command: &str, background: bool, reason: Option<&str>) -> Option<ShellCall> {
        Some(ShellCall {
            session_hint: Some("s-1".into()),
            tool_use_id: "tu-1".into(),
            command: command.into(),
            background,
            reason: reason.map(String::from),
        })
    }

    #[test]
    fn claude_bash_payload_gives_command_reason_and_background() {
        // Defect: the Claude reason or run_in_background flag is dropped, so the
        // provisioner sees no reason and backgrounded sleeps are held.
        let stdin = r#"{"session_id":"s-1","transcript_path":"/t","cwd":"/w","permission_mode":"default",
            "hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"tu-1",
            "tool_input":{"command":"sleep 30","description":"Wait for CI","run_in_background":true,"timeout":5000}}"#;
        assert_eq!(
            parse_payload(Harness::Claude, stdin).unwrap(),
            call("sleep 30", true, Some("Wait for CI"))
        );
    }

    #[test]
    fn claude_payload_without_background_runs_in_the_foreground() {
        // Defect: a missing run_in_background reads as an error instead of false.
        let stdin = r#"{"session_id":"s-1","tool_name":"Bash","tool_use_id":"tu-1","tool_input":{"command":"sleep 30"}}"#;
        assert_eq!(
            parse_payload(Harness::Claude, stdin).unwrap(),
            call("sleep 30", false, None)
        );
    }

    #[test]
    fn claude_non_bash_tool_is_not_a_shell_call() {
        // Defect: a Read or Edit hook payload fails to parse and blocks the tool.
        let stdin = r#"{"session_id":"s-1","tool_name":"Read","tool_use_id":"tu-1","tool_input":{"file_path":"/a"}}"#;
        assert_eq!(parse_payload(Harness::Claude, stdin).unwrap(), None);
    }

    #[test]
    fn codex_bash_payload_parses_with_its_field_names() {
        // Defect: Codex's PreToolUse fields (turn_id, model, tool_input.command)
        // do not parse, so every Codex shell call fails closed.
        let stdin = r#"{"session_id":"s-1","turn_id":"t-3","cwd":"/w","transcript_path":null,
            "model":"gpt-5","permission_mode":"bypassPermissions","hook_event_name":"PreToolUse",
            "tool_name":"Bash","tool_use_id":"tu-1","tool_input":{"command":"gh run watch 9"}}"#;
        assert_eq!(
            parse_payload(Harness::Codex, stdin).unwrap(),
            call("gh run watch 9", false, None)
        );
    }

    #[test]
    fn codex_apply_patch_is_not_a_shell_call() {
        // Defect: apply_patch also carries tool_input.command and is held as shell.
        let stdin = r#"{"session_id":"s-1","tool_name":"apply_patch","tool_use_id":"tu-1","tool_input":{"command":"*** Begin Patch"}}"#;
        assert_eq!(parse_payload(Harness::Codex, stdin).unwrap(), None);
    }

    #[test]
    fn omp_payload_gives_call_id_and_async_flag() {
        // Defect: omp's async flag is ignored, so async waits are held.
        let stdin = r#"{"toolCallId":"tu-1","command":"sleep 30","async":true,"sessionId":"s-1","extra":1}"#;
        assert_eq!(
            parse_payload(Harness::Omp, stdin).unwrap(),
            call("sleep 30", true, None)
        );
    }

    #[test]
    fn bash_payload_without_a_tool_use_id_is_an_error() {
        // Defect: a hold with no id cannot be made idempotent; it must not be sent.
        let stdin = r#"{"tool_name":"Bash","tool_input":{"command":"sleep 30"}}"#;
        assert!(parse_payload(Harness::Claude, stdin).is_err());
    }

    #[test]
    fn unreadable_payload_is_an_error() {
        // Defect: garbage stdin is read as "not a shell call" and fails open.
        assert!(parse_payload(Harness::Claude, "not json").is_err());
        assert!(parse_payload(Harness::Omp, r#"{"command":"sleep 1"}"#).is_err());
    }

    #[test]
    fn wait_reply_reads_every_status() {
        // Defect: a deny reason or an unknown status is misread as allow.
        let read = |text: &str| serde_json::from_str::<HoldWaitReply>(text).ok();
        assert_eq!(
            read(r#"{"ok":true,"status":"pending"}"#),
            Some(HoldWaitReply::Pending)
        );
        assert_eq!(
            read(r#"{"ok":true,"status":"allow"}"#),
            Some(HoldWaitReply::Allow)
        );
        assert_eq!(
            read(r#"{"ok":true,"status":"deny","reason":"use CI"}"#),
            Some(HoldWaitReply::Deny {
                reason: Some("use CI".into())
            })
        );
        assert_eq!(
            read(r#"{"ok":true,"status":"timeout"}"#),
            Some(HoldWaitReply::Timeout)
        );
        assert_eq!(read(r#"{"ok":true,"status":"maybe"}"#), None);
    }
}
