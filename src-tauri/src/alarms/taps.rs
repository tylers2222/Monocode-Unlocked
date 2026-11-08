//! Design section 8, `agent_activity` taps: what each harness's stdio says
//! of a session's progress, tools, dialogs and todo list. A `Tap` lives
//! with one spawned child; the stdout reader and `harness_write` feed it
//! lines and write the `Snapshot` it hands back. The frames are the ones
//! the front end parses (`src/integrations/harness/providers/*`).

use std::collections::HashSet;

use serde_json::Value;

use super::state::DialogKind;

/// Progress alone is written at most this often; any other change at once.
const FLUSH_EVERY_MS: i64 = 5_000;
const DIALOG_TEXT_CHARS: usize = 300;

/// The harness families whose stdio this build reads. omp and Pi share one
/// RPC protocol (`pi/piFamily.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Harness {
    Claude,
    Codex,
    Pi,
}

impl Harness {
    pub(crate) fn parse(provider: &str) -> Option<Self> {
        match provider {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "omp" | "pi" => Some(Harness::Pi),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Pi => "pi",
        }
    }

    /// Substrings every stdout frame this tap reads carries, so the reader
    /// parses only those lines (token deltas are most of the stream).
    fn stdout_markers(self) -> &'static [&'static str] {
        match self {
            Harness::Claude => &[
                "control_",
                "tool_use",
                "tool_result",
                "\"result\"",
                "\"init\"",
                "task_",
            ],
            Harness::Codex => &[
                "requestApproval",
                "requestUserInput",
                "elicitation",
                "serverRequest/resolved",
                "item/started",
                "item/completed",
                "turn/",
            ],
            Harness::Pi => &[
                "extension_ui_request",
                "tool_execution_",
                "agent_start",
                "agent_end",
                "agent_settled",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    TurnStart,
    TurnEnd,
    ToolStart(String),
    ToolEnd(String),
    DialogOpen {
        id: String,
        kind: DialogKind,
        text: String,
    },
    DialogClose(String),
    Todos {
        open: bool,
    },
    PlanProposed,
}

fn text<'a>(frame: &'a Value, path: &str) -> Option<&'a str> {
    frame.pointer(path).and_then(Value::as_str)
}

/// A JSON-RPC id as one string: Codex sends numbers and strings.
fn rpc_id(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

fn clip(text: &str) -> String {
    text.chars().take(DIALOG_TEXT_CHARS).collect()
}

/// What a tool's input shows in a dialog line: a command, else the input.
fn tool_text(tool: &str, input: Option<&Value>) -> String {
    let detail = input.map_or(String::new(), |input| {
        input
            .get("command")
            .and_then(Value::as_str)
            .map_or_else(|| input.to_string(), str::to_string)
    });
    clip(format!("{tool} {detail}").trim_end())
}

/// `taskList.ts` `isTodoToolName` and `normalizeStatus`: a list is open
/// while one item is not completed or cancelled.
fn is_todo_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "todowrite" | "todo_write" | "writetodos" | "updatetodos"
    )
}

fn todos_open(input: Option<&Value>) -> Option<Event> {
    let todos = input?.get("todos")?.as_array()?;
    let open = todos.iter().any(|todo| {
        !matches!(
            todo.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "completed" | "complete" | "done" | "cancelled" | "canceled" | "skipped"
        )
    });
    Some(Event::Todos { open })
}

/// `isTerminalAgentTaskStatus` in `claudeProtocol.ts`.
fn task_finished(status: Option<&str>) -> bool {
    matches!(
        status.map(str::to_ascii_lowercase).as_deref(),
        Some("completed" | "failed" | "killed" | "stopped")
    )
}

/// A Claude background task (shell, monitor, subagent) is a running tool
/// that outlives its turn's `result` (`maybeFinishTurn`, claude.ts).
const TASK: &str = "task:";

fn claude_stdout(frame: &Value) -> Vec<Event> {
    let kind = text(frame, "/type").unwrap_or_default();
    let subagent = text(frame, "/parent_tool_use_id").is_some_and(|id| !id.is_empty());
    match kind {
        "control_request" | "sdk_control_request" => {
            let subtype = text(frame, "/request/subtype");
            let id = text(frame, "/request_id").or_else(|| text(frame, "/request/request_id"));
            let tool = text(frame, "/request/tool_name").unwrap_or_default();
            match (subtype, id) {
                // The app denies ExitPlanMode at once and keeps the plan itself.
                (Some("can_use_tool" | "permission"), Some(id)) if tool != "ExitPlanMode" => {
                    let input = frame
                        .pointer("/request/input")
                        .or_else(|| frame.pointer("/request/tool_input"));
                    vec![Event::DialogOpen {
                        id: id.into(),
                        kind: if tool == "AskUserQuestion" {
                            DialogKind::Question
                        } else {
                            DialogKind::Permission
                        },
                        text: tool_text(tool, input),
                    }]
                }
                _ => Vec::new(),
            }
        }
        "control_cancel_request" => text(frame, "/request_id")
            .or_else(|| text(frame, "/request/request_id"))
            .map(|id| vec![Event::DialogClose(id.into())])
            .unwrap_or_default(),
        "assistant" if !subagent => frame
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|block| text(block, "/type") == Some("tool_use"))
            .flat_map(|block| {
                let name = text(block, "/name").unwrap_or_default();
                let start = text(block, "/id").map(|id| Event::ToolStart(id.into()));
                let todos = is_todo_tool(name)
                    .then(|| todos_open(block.get("input")))
                    .flatten();
                let plan = (name == "ExitPlanMode").then_some(Event::PlanProposed);
                start.into_iter().chain(todos).chain(plan)
            })
            .collect(),
        "stream_event" if !subagent => {
            let block = frame.pointer("/event/content_block");
            match (
                text(frame, "/event/type"),
                block.and_then(|block| text(block, "/type")),
                block.and_then(|block| text(block, "/id")),
            ) {
                (
                    Some("content_block_start"),
                    Some("tool_use" | "server_tool_use" | "mcp_tool_use"),
                    Some(id),
                ) => vec![Event::ToolStart(id.into())],
                _ => Vec::new(),
            }
        }
        "user" if !subagent => frame
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|block| text(block, "/type") == Some("tool_result"))
            .filter_map(|block| text(block, "/tool_use_id").map(|id| Event::ToolEnd(id.into())))
            .collect(),
        "result" if !subagent => vec![Event::TurnEnd],
        "system" => {
            let task = text(frame, "/task_id").map(|id| format!("{TASK}{id}"));
            match (text(frame, "/subtype"), task) {
                (Some("init"), _) if !subagent => vec![Event::TurnStart],
                (Some("task_started"), Some(task)) => vec![Event::ToolStart(task)],
                (Some("task_updated"), Some(task))
                    if task_finished(text(frame, "/patch/status")) =>
                {
                    vec![Event::ToolEnd(task)]
                }
                (Some("task_notification"), Some(task))
                    if frame.get("ambient") != Some(&Value::Bool(true)) =>
                {
                    vec![Event::ToolEnd(task)]
                }
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// `codexProtocol.ts`: the item types that are tool calls.
fn is_codex_tool(item: &Value) -> bool {
    matches!(
        text(item, "/type"),
        Some(
            "commandExecution"
                | "fileChange"
                | "webSearch"
                | "mcpToolCall"
                | "collabAgentToolCall"
                | "subAgentActivity"
        )
    )
}

fn codex_stdout(frame: &Value) -> Vec<Event> {
    let method = text(frame, "/method").unwrap_or_default();
    let item = frame.pointer("/params/item");
    let item_id = item.and_then(|item| text(item, "/id"));
    let dialog = match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "mcpServer/elicitation/request" => Some(DialogKind::Permission),
        "item/tool/requestUserInput" => Some(DialogKind::Question),
        _ => None,
    };
    if let (Some(kind), Some(id)) = (dialog, frame.get("id").and_then(rpc_id)) {
        let detail = text(frame, "/params/command")
            .or_else(|| text(frame, "/params/reason"))
            .or_else(|| text(frame, "/params/message"))
            .unwrap_or_default();
        return vec![Event::DialogOpen {
            id,
            kind,
            text: clip(format!("{method} {detail}").trim_end()),
        }];
    }
    match (method, item_id) {
        ("serverRequest/resolved", _) => frame
            .pointer("/params/requestId")
            .and_then(rpc_id)
            .map(|id| vec![Event::DialogClose(id)])
            .unwrap_or_default(),
        ("item/started", Some(id)) if item.is_some_and(is_codex_tool) => {
            vec![Event::ToolStart(id.into())]
        }
        ("item/completed", Some(id)) if item.is_some_and(is_codex_tool) => {
            vec![Event::ToolEnd(id.into())]
        }
        ("turn/plan/updated", _) => {
            let open = frame
                .pointer("/params/plan")
                .and_then(Value::as_array)
                .is_some_and(|plan| {
                    plan.iter()
                        .any(|step| text(step, "/status") != Some("completed"))
                });
            vec![Event::Todos { open }]
        }
        ("turn/started", _) => vec![Event::TurnStart],
        ("turn/completed" | "turn/aborted", _) => vec![Event::TurnEnd],
        _ => Vec::new(),
    }
}

fn pi_stdout(frame: &Value) -> Vec<Event> {
    let id = text(frame, "/id");
    let tool = text(frame, "/toolCallId");
    match (text(frame, "/type").unwrap_or_default(), id, tool) {
        ("extension_ui_request", Some(id), _) => {
            let kind = match text(frame, "/method") {
                Some("confirm") => DialogKind::Permission,
                Some("select" | "input" | "editor") => DialogKind::Question,
                Some("cancel") => return vec![Event::DialogClose(id.into())],
                _ => return Vec::new(),
            };
            let line = [text(frame, "/title"), text(frame, "/message")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(": ");
            vec![Event::DialogOpen {
                id: id.into(),
                kind,
                text: clip(&line),
            }]
        }
        ("tool_execution_start", _, Some(tool)) => {
            let todos = text(frame, "/toolName")
                .is_some_and(is_todo_tool)
                .then(|| todos_open(frame.get("args")))
                .flatten();
            std::iter::once(Event::ToolStart(tool.into()))
                .chain(todos)
                .collect()
        }
        ("tool_execution_end", _, Some(tool)) => vec![Event::ToolEnd(tool.into())],
        ("agent_start", _, _) => vec![Event::TurnStart],
        ("agent_end", _, _) if frame.get("willRetry") != Some(&Value::Bool(true)) => {
            vec![Event::TurnEnd]
        }
        ("agent_settled", _, _) => vec![Event::TurnEnd],
        _ => Vec::new(),
    }
}

/// The app's answers on stdin: each closes the dialog with its id.
fn stdin_events(harness: Harness, frame: &Value) -> Vec<Event> {
    let id = match harness {
        Harness::Claude => (text(frame, "/type") == Some("control_response"))
            .then(|| text(frame, "/response/request_id").map(str::to_string))
            .flatten(),
        // A response has an id and no method; a request of the app's own has both.
        Harness::Codex => frame
            .get("method")
            .is_none()
            .then(|| frame.get("id").and_then(rpc_id))
            .flatten(),
        Harness::Pi => (text(frame, "/type") == Some("extension_ui_response"))
            .then(|| text(frame, "/id").map(str::to_string))
            .flatten(),
    };
    id.map(|id| vec![Event::DialogClose(id)])
        .unwrap_or_default()
}

/// The open dialog the row shows: the oldest one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TapDialog {
    pub kind: DialogKind,
    pub request_id: Option<String>,
    pub text: String,
    pub since: i64,
}

/// The tap's columns of `agent_activity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub harness: Harness,
    pub last_progress_at: i64,
    pub tool_running: bool,
    pub todo_open: bool,
    pub dialog: Option<TapDialog>,
}

impl Snapshot {
    /// Equal but for the progress time, which alone may wait for a flush.
    fn same_but_progress(&self, other: &Snapshot) -> bool {
        Snapshot {
            last_progress_at: 0,
            ..self.clone()
        } == Snapshot {
            last_progress_at: 0,
            ..other.clone()
        }
    }
}

#[derive(Debug)]
pub(crate) struct Tap {
    harness: Harness,
    dialogs: Vec<TapDialog>,
    tools: HashSet<String>,
    todo_open: bool,
    plan_since: Option<i64>,
    last_progress_at: i64,
    written: Option<(Snapshot, i64)>,
}

/// Which pipe a line went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pipe {
    Stdout,
    Stdin,
}

impl Tap {
    pub(crate) fn new(harness: Harness) -> Self {
        Self {
            harness,
            dialogs: Vec::new(),
            tools: HashSet::new(),
            todo_open: false,
            plan_since: None,
            last_progress_at: 0,
            written: None,
        }
    }

    /// Reads one line at active time `now`; the row to write, when one is due.
    pub(crate) fn line(&mut self, pipe: Pipe, line: &str, now: i64) -> Option<Snapshot> {
        let read = match pipe {
            Pipe::Stdout => self
                .harness
                .stdout_markers()
                .iter()
                .any(|marker| line.contains(marker)),
            Pipe::Stdin => true,
        };
        let events = read
            .then(|| serde_json::from_str::<Value>(line).ok())
            .flatten()
            .map(|frame| match (pipe, self.harness) {
                (Pipe::Stdout, Harness::Claude) => claude_stdout(&frame),
                (Pipe::Stdout, Harness::Codex) => codex_stdout(&frame),
                (Pipe::Stdout, Harness::Pi) => pi_stdout(&frame),
                (Pipe::Stdin, harness) => stdin_events(harness, &frame),
            })
            .unwrap_or_default();
        // Every stdout line is output; on stdin only an answer is progress.
        if pipe == Pipe::Stdout || !events.is_empty() {
            self.last_progress_at = now;
        }
        for event in events {
            self.apply(event, now);
        }
        self.due(now)
    }

    fn apply(&mut self, event: Event, now: i64) {
        match event {
            Event::TurnStart => {
                self.dialogs.clear();
                self.plan_since = None;
                self.end_turn_tools();
            }
            Event::TurnEnd => {
                self.dialogs.clear();
                self.end_turn_tools();
            }
            Event::ToolStart(id) => {
                self.tools.insert(id);
            }
            Event::ToolEnd(id) => {
                self.tools.remove(&id);
            }
            Event::DialogOpen { id, kind, text } => {
                if !self
                    .dialogs
                    .iter()
                    .any(|dialog| dialog.request_id.as_deref() == Some(&id))
                {
                    self.dialogs.push(TapDialog {
                        kind,
                        request_id: Some(id),
                        text,
                        since: now,
                    });
                }
            }
            Event::DialogClose(id) => self
                .dialogs
                .retain(|dialog| dialog.request_id.as_deref() != Some(&id)),
            Event::Todos { open } => self.todo_open = open,
            Event::PlanProposed => self.plan_since = Some(now),
        }
    }

    /// A turn's tool calls end with it; background tasks do not.
    fn end_turn_tools(&mut self) {
        self.tools.retain(|id| id.starts_with(TASK));
    }

    fn snapshot(&self) -> Snapshot {
        let plan = self.plan_since.map(|since| TapDialog {
            kind: DialogKind::Plan,
            request_id: None,
            text: "Plan waiting for approval".into(),
            since,
        });
        Snapshot {
            harness: self.harness,
            last_progress_at: self.last_progress_at,
            tool_running: !self.tools.is_empty(),
            todo_open: self.todo_open,
            dialog: self.dialogs.first().cloned().or(plan),
        }
    }

    fn due(&self, now: i64) -> Option<Snapshot> {
        let snapshot = self.snapshot();
        match &self.written {
            Some((written, at)) if *written == snapshot => None,
            Some((written, at))
                if written.same_but_progress(&snapshot) && now - at < FLUSH_EVERY_MS =>
            {
                None
            }
            _ => Some(snapshot),
        }
    }

    /// `snapshot` is in the row as of `now`.
    pub(crate) fn wrote(&mut self, snapshot: Snapshot, now: i64) {
        self.written = Some((snapshot, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `lines` at one second apart from `start`, marking every row
    /// written; the last row written.
    fn feed(tap: &mut Tap, start: i64, lines: &[(Pipe, &str)]) -> Option<Snapshot> {
        let mut last = None;
        for (at, (pipe, line)) in (start..).step_by(1_000).zip(lines) {
            if let Some(row) = tap.line(*pipe, line, at) {
                tap.wrote(row.clone(), at);
                last = Some(row);
            }
        }
        last
    }

    fn dialog(row: &Snapshot) -> Option<(DialogKind, Option<&str>, i64)> {
        row.dialog
            .as_ref()
            .map(|d| (d.kind, d.request_id.as_deref(), d.since))
    }

    // Defect: a Claude permission or question dialog is not seen, the
    // app's control_response does not clear it, or AskUserQuestion reads
    // as a permission (frames: claudeProtocol.ts:311, 387-425).
    #[test]
    fn claude_dialogs_open_on_can_use_tool_and_close_on_the_matching_response() {
        let mut tap = Tap::new(Harness::Claude);
        let row = feed(
            &mut tap,
            1_000,
            &[(
                Pipe::Stdout,
                r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf target"},"tool_use_id":"toolu_1"}}"#,
            )],
        )
        .unwrap();
        assert_eq!(
            dialog(&row),
            Some((DialogKind::Permission, Some("req_1"), 1_000))
        );
        assert_eq!(row.dialog.unwrap().text, "Bash rm -rf target");
        let row = feed(
            &mut tap,
            2_000,
            &[
                (
                    Pipe::Stdout,
                    r#"{"type":"control_request","request_id":"req_2","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[]}}}"#,
                ),
                (
                    Pipe::Stdin,
                    r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{"behavior":"allow"}}}"#,
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            dialog(&row),
            Some((DialogKind::Question, Some("req_2"), 2_000))
        );
        let row = feed(
            &mut tap,
            5_000,
            &[(
                Pipe::Stdout,
                r#"{"type":"control_cancel_request","request_id":"req_2"}"#,
            )],
        )
        .unwrap();
        assert_eq!(dialog(&row), None, "a withdrawn request is answered");
    }

    // Defect: ExitPlanMode's instant deny reads as a permission dialog, or
    // the proposed plan is not kept until the next turn starts.
    #[test]
    fn a_claude_plan_waits_from_exit_plan_mode_until_the_next_turn() {
        let mut tap = Tap::new(Harness::Claude);
        let row = feed(
            &mut tap,
            1_000,
            &[
                (
                    Pipe::Stdout,
                    r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_p","name":"ExitPlanMode","input":{"plan":"Do it"}}]}}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"control_request","request_id":"req_p","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{}}}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_p"}]}}"#,
                ),
                (Pipe::Stdout, r#"{"type":"result","subtype":"success"}"#),
            ],
        )
        .unwrap();
        assert_eq!(dialog(&row), Some((DialogKind::Plan, None, 1_000)));
        assert!(!row.tool_running);
        let row = feed(
            &mut tap,
            9_000,
            &[(Pipe::Stdout, r#"{"type":"system","subtype":"init"}"#)],
        )
        .unwrap();
        assert_eq!(dialog(&row), None);
    }

    // Defect: a Claude tool, or a background task that outlives `result`,
    // is not counted as running (frames: claudeProtocol.ts:557-892,
    // claude.ts:1263-1300).
    #[test]
    fn claude_tools_and_background_tasks_run_until_their_end_frames() {
        let mut tap = Tap::new(Harness::Claude);
        let started = feed(
            &mut tap,
            0,
            &[
                (
                    Pipe::Stdout,
                    r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"toolu_1","name":"Bash"}}}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"system","subtype":"task_started","task_id":"t1"}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1"}]}}"#,
                ),
                (Pipe::Stdout, r#"{"type":"result","subtype":"success"}"#),
            ],
        )
        .unwrap();
        assert!(started.tool_running, "the background task still runs");
        let ended = feed(
            &mut tap,
            10_000,
            &[(
                Pipe::Stdout,
                r#"{"type":"system","subtype":"task_updated","task_id":"t1","patch":{"status":"completed"}}"#,
            )],
        )
        .unwrap();
        assert!(!ended.tool_running);
    }

    // Defect: a TodoWrite list with an unfinished item is not open work, or
    // a finished one still is.
    #[test]
    fn a_claude_todo_list_is_open_while_an_item_is_not_done() {
        let mut tap = Tap::new(Harness::Claude);
        let todo = |status: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_{status}","name":"TodoWrite","input":{{"todos":[{{"content":"A","status":"completed"}},{{"content":"B","status":"{status}"}}]}}}}]}}}}"#
            )
        };
        let open = feed(&mut tap, 0, &[(Pipe::Stdout, &todo("in_progress"))]).unwrap();
        assert!(open.todo_open);
        let done = feed(&mut tap, 10_000, &[(Pipe::Stdout, &todo("done"))]).unwrap();
        assert!(!done.todo_open);
    }

    // Defect: Codex approval and question requests are missed, the app's
    // `{id, result}` answer or `serverRequest/resolved` does not clear
    // them, or numeric and string ids mix (codexProtocol.ts:1435-1474,
    // codex.ts:903-918, 1390, 1445).
    #[test]
    fn codex_server_requests_are_dialogs_until_answered_by_id() {
        let mut tap = Tap::new(Harness::Codex);
        let row = feed(
            &mut tap,
            0,
            &[(
                Pipe::Stdout,
                r#"{"id":92,"method":"item/commandExecution/requestApproval","params":{"threadId":"t1","itemId":"cmd_1","command":"git push origin main"}}"#,
            )],
        )
        .unwrap();
        assert_eq!(dialog(&row), Some((DialogKind::Permission, Some("92"), 0)));
        let row = feed(
            &mut tap,
            1_000,
            &[
                (
                    Pipe::Stdin,
                    r#"{"id":92,"method":"turn/start","params":{}}"#,
                ),
                (Pipe::Stdin, r#"{"id":92,"result":{"decision":"decline"}}"#),
            ],
        )
        .unwrap();
        assert_eq!(dialog(&row), None, "only a response clears it");
        let row = feed(
            &mut tap,
            10_000,
            &[(
                Pipe::Stdout,
                r#"{"id":"q1","method":"item/tool/requestUserInput","params":{"itemId":"i1","isBlocking":true,"questions":[]}}"#,
            )],
        )
        .unwrap();
        assert_eq!(dialog(&row).map(|d| d.0), Some(DialogKind::Question));
        let row = feed(
            &mut tap,
            11_000,
            &[(
                Pipe::Stdout,
                r#"{"method":"serverRequest/resolved","params":{"threadId":"t1","requestId":"q1"}}"#,
            )],
        )
        .unwrap();
        assert_eq!(dialog(&row), None);
    }

    // Defect: Codex tool items or an unfinished plan are not seen
    // (codexProtocol.ts:468-534).
    #[test]
    fn codex_tool_items_run_and_a_plan_with_a_step_left_is_open() {
        let mut tap = Tap::new(Harness::Codex);
        let row = feed(
            &mut tap,
            0,
            &[
                (
                    Pipe::Stdout,
                    r#"{"method":"item/started","params":{"item":{"id":"cmd_1","type":"commandExecution","status":"inProgress"}}}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"method":"item/started","params":{"item":{"id":"msg_1","type":"agentMessage"}}}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"method":"turn/plan/updated","params":{"turnId":"turn_1","plan":[{"step":"Inspect","status":"completed"},{"step":"Implement","status":"inProgress"}]}}"#,
                ),
            ],
        )
        .unwrap();
        assert!(row.tool_running && row.todo_open);
        let row = feed(
            &mut tap,
            10_000,
            &[(
                Pipe::Stdout,
                r#"{"method":"item/completed","params":{"item":{"id":"cmd_1","type":"commandExecution","status":"completed"}}}"#,
            )],
        )
        .unwrap();
        assert!(!row.tool_running, "a message item is not a tool");
    }

    // Defect: an omp/Pi dialog needing a reply is missed, a fire-and-forget
    // status is taken for one, or the reply does not clear it
    // (piProtocol.ts:323-363, piFamily.ts:794-802).
    #[test]
    fn pi_extension_requests_needing_a_reply_are_dialogs() {
        let mut tap = Tap::new(Harness::Pi);
        let row = feed(
            &mut tap,
            0,
            &[
                (
                    Pipe::Stdout,
                    r#"{"type":"extension_ui_request","id":"s1","method":"setStatus","statusKey":"x","statusText":"FULL"}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"extension_ui_request","id":"c1","method":"confirm","title":"Review","message":"Open docs?"}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"tool_execution_start","toolCallId":"spawn","toolName":"task","args":{}}"#,
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            dialog(&row),
            Some((DialogKind::Permission, Some("c1"), 1_000))
        );
        assert_eq!(row.dialog.as_ref().unwrap().text, "Review: Open docs?");
        assert!(row.tool_running);
        let row = feed(
            &mut tap,
            10_000,
            &[
                (
                    Pipe::Stdin,
                    r#"{"type":"extension_ui_response","id":"c1","confirmed":true}"#,
                ),
                (
                    Pipe::Stdout,
                    r#"{"type":"tool_execution_end","toolCallId":"spawn","isError":false}"#,
                ),
            ],
        )
        .unwrap();
        assert_eq!(dialog(&row), None);
        assert!(!row.tool_running);
    }

    // Defect: the tap writes the row on every line, or holds back a change
    // other than progress (design 8: at most every 5 s, at once on a
    // dialog, tool or turn change).
    #[test]
    fn progress_alone_flushes_every_five_seconds_and_a_change_at_once() {
        let mut tap = Tap::new(Harness::Claude);
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#;
        let first = tap.line(Pipe::Stdout, delta, 0).unwrap();
        tap.wrote(first, 0);
        assert_eq!(tap.line(Pipe::Stdout, delta, 4_999), None);
        let tool = r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"toolu_9"}}}"#;
        let now = tap.line(Pipe::Stdout, tool, 4_999).unwrap();
        assert!(now.tool_running);
        tap.wrote(now, 4_999);
        assert_eq!(tap.line(Pipe::Stdout, delta, 9_998), None);
        let later = tap.line(Pipe::Stdout, delta, 9_999).unwrap();
        assert_eq!(later.last_progress_at, 9_999);
        let unwritten = tap.line(Pipe::Stdout, delta, 10_000).unwrap();
        assert_eq!(unwritten.last_progress_at, 10_000, "a failed write retries");
    }
}
