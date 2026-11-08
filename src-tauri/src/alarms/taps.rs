//! Design section 8, `agent_activity` taps: what each harness's stdio says
//! of a session's progress, tools, dialogs and todo list. A `Tap` lives
//! with one spawned child; the stdout reader and `harness_write` feed it
//! lines and write the `Snapshot` it hands back. The frames are the ones
//! the front end parses (`src/integrations/harness/providers/*`).

mod tasks;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use serde_json::Value;

use super::state::DialogKind;
use tasks::{TaskList, TaskTool};

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

    /// A frame the front end drops as noise, so it is not progress either:
    /// Claude's `keep_alive` (claude.ts:644) can keep coming while its API
    /// stream is stalled.
    fn is_noise(self, line: &str) -> bool {
        match self {
            Harness::Claude => {
                line.contains("\"keep_alive\"")
                    && serde_json::from_str::<Value>(line)
                        .is_ok_and(|frame| text(&frame, "/type") == Some("keep_alive"))
            }
            Harness::Codex | Harness::Pi => false,
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
    /// A Claude TaskCreate/TaskUpdate call; its result folds it in.
    TaskCall {
        id: String,
        tool: TaskTool,
        input: Value,
    },
    TaskResult {
        id: String,
        text: String,
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

pub(super) fn status_open(status: &str) -> bool {
    !matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "complete" | "done" | "cancelled" | "canceled" | "skipped"
    )
}

fn todos_open(input: Option<&Value>) -> Option<Event> {
    let todos = input?.get("todos")?.as_array()?;
    let open = todos.iter().any(|todo| {
        status_open(
            todo.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    });
    Some(Event::Todos { open })
}

/// A tool_result's text: a string, or its text blocks.
fn result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| text(part, "/text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
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

/// The tool calls of a Claude `assistant` frame: each runs until its
/// result, a todo or task call updates the list, ExitPlanMode proposes a plan.
fn claude_tool_uses(frame: &Value) -> Vec<Event> {
    frame
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| text(block, "/type") == Some("tool_use"))
        .flat_map(|block| {
            let name = text(block, "/name").unwrap_or_default();
            let id = text(block, "/id");
            let start = id.map(|id| Event::ToolStart(id.into()));
            let todos = is_todo_tool(name)
                .then(|| todos_open(block.get("input")))
                .flatten();
            let task = TaskTool::parse(name)
                .zip(id)
                .map(|(tool, id)| Event::TaskCall {
                    id: id.into(),
                    tool,
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                });
            let plan = (name == "ExitPlanMode").then_some(Event::PlanProposed);
            start.into_iter().chain(todos).chain(task).chain(plan)
        })
        .collect()
}

/// The tool results of a Claude `user` frame; a task call's result folds it.
fn claude_tool_results(frame: &Value) -> Vec<Event> {
    frame
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| text(block, "/type") == Some("tool_result"))
        .filter_map(|block| {
            let id = text(block, "/tool_use_id")?;
            let failed = block.get("is_error") == Some(&Value::Bool(true));
            let result = (!failed).then(|| Event::TaskResult {
                id: id.into(),
                text: result_text(block),
            });
            Some(std::iter::once(Event::ToolEnd(id.into())).chain(result))
        })
        .flatten()
        .collect()
}

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
        "assistant" if !subagent => claude_tool_uses(frame),
        "stream_event" if !subagent => {
            let block = frame.pointer("/event/content_block");
            match (
                text(frame, "/event/type"),
                block.and_then(|block| text(block, "/type")),
                block.and_then(|block| text(block, "/id")),
            ) {
                // A server or MCP tool's result is no `tool_result`, so no
                // end would match its start.
                (Some("content_block_start"), Some("tool_use"), Some(id)) => {
                    vec![Event::ToolStart(id.into())]
                }
                _ => Vec::new(),
            }
        }
        "user" if !subagent => claude_tool_results(frame),
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
    pub background: bool,
    /// When the last background tool ended.
    pub background_ended_at: Option<i64>,
    pub todo_open: bool,
    pub dialog: Option<TapDialog>,
}

impl Snapshot {
    /// Equal but for the progress time, which alone may wait for a flush.
    fn same_but_progress(&self, other: &Snapshot) -> bool {
        (
            self.harness,
            self.tool_running,
            self.background,
            self.background_ended_at,
            self.todo_open,
            &self.dialog,
        ) == (
            other.harness,
            other.tool_running,
            other.background,
            other.background_ended_at,
            other.todo_open,
            &other.dialog,
        )
    }
}

#[derive(Debug)]
pub(crate) struct Tap {
    harness: Harness,
    dialogs: Vec<TapDialog>,
    tools: HashSet<String>,
    todo_open: bool,
    tasks: TaskList,
    plan_since: Option<i64>,
    background_ended_at: Option<i64>,
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
            tasks: TaskList::default(),
            plan_since: None,
            background_ended_at: None,
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
        // Every stdout line but noise is output; on stdin only an answer.
        let output = pipe == Pipe::Stdout && !self.harness.is_noise(line);
        if output || !events.is_empty() {
            self.last_progress_at = now;
        }
        for event in events {
            self.apply(event, now);
        }
        self.due(now)
    }

    /// The child exited at `now`: its tools and background tasks end with
    /// it (S1 r4 F2). The row to write, when one is due.
    pub(crate) fn exited(&mut self, now: i64) -> Option<Snapshot> {
        if self.background() {
            self.background_ended_at = Some(now);
        }
        self.tools.clear();
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
                if self.tools.remove(&id) && id.starts_with(TASK) && !self.background() {
                    self.background_ended_at = Some(now);
                }
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
            Event::TaskCall { id, tool, input } => self.tasks.call(id, tool, input),
            Event::TaskResult { id, text } => {
                if let Some(open) = self.tasks.result(&id, &text) {
                    self.todo_open = open;
                }
            }
            Event::PlanProposed => self.plan_since = Some(now),
        }
    }

    fn background(&self) -> bool {
        self.tools.iter().any(|id| id.starts_with(TASK))
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
            tool_running: self.tools.iter().any(|id| !id.starts_with(TASK)),
            background: self.background(),
            background_ended_at: self.background_ended_at,
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
