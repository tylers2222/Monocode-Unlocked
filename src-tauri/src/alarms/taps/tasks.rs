//! Claude's task list from TaskCreate/TaskUpdate, which newer Claude Code
//! builds use in place of TodoWrite (claudeProtocol.ts `isClaudeTaskTool`,
//! `applyClaudeTaskTool`). A call folds in when its result arrives.

use std::collections::HashMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskTool {
    Create,
    Update,
}

impl TaskTool {
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "TaskCreate" => Some(TaskTool::Create),
            "TaskUpdate" => Some(TaskTool::Update),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct TaskList {
    /// Calls waiting for their result, by tool call id.
    calls: HashMap<String, (TaskTool, Value)>,
    /// Each task by Claude's task id: whether it is open.
    open: HashMap<String, bool>,
}

impl TaskList {
    pub(super) fn call(&mut self, id: String, tool: TaskTool, input: Value) {
        self.calls.insert(id, (tool, input));
    }

    /// Folds in the result of call `id`; whether a task is open after it,
    /// when the call was one of the list's and named a task.
    pub(super) fn result(&mut self, id: &str, result: &str) -> Option<bool> {
        let (tool, input) = self.calls.remove(id)?;
        let task = task_id(tool, &input, result)?;
        match (
            tool,
            input.get("status").and_then(Value::as_str).map(str::trim),
        ) {
            (TaskTool::Create, _) => {
                self.open.insert(task, true);
            }
            (TaskTool::Update, Some("deleted")) => {
                self.open.remove(&task);
            }
            (TaskTool::Update, Some(status)) => {
                if let Some(open) = self.open.get_mut(&task) {
                    *open = super::status_open(status);
                }
            }
            (TaskTool::Update, None) => return None,
        }
        Some(self.open.values().any(|open| *open))
    }
}

/// TaskCreate learns its id from the result text ("Task #3 created
/// successfully: ..."), TaskUpdate names it.
fn task_id(tool: TaskTool, input: &Value, result: &str) -> Option<String> {
    match tool {
        TaskTool::Create => {
            let rest = &result[result.find("Task #")? + "Task #".len()..];
            let id: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != ':')
                .collect();
            (!id.is_empty()).then_some(id)
        }
        TaskTool::Update => match input.get("taskId")? {
            Value::Number(id) => Some(id.to_string()),
            Value::String(id) => {
                let id = id.trim().trim_start_matches('#');
                (!id.is_empty()).then(|| id.to_string())
            }
            _ => None,
        },
    }
}
