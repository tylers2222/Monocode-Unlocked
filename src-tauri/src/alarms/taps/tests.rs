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

/// A Claude task tool call and its result, as claudeLive.test.ts
/// `emitTaskTool` sends them.
fn task_tool(id: &str, name: &str, input: &str, result: &str) -> [(Pipe, String); 2] {
    [
        (
            Pipe::Stdout,
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}]}}}}"#
            ),
        ),
        (
            Pipe::Stdout,
            format!(
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","content":"{result}"}}]}}}}"#
            ),
        ),
    ]
}

// Defect (review m2): a Claude build that keeps its list with
// TaskCreate/TaskUpdate (claudeProtocol.ts:962-1020) ends a turn with
// tasks left and the detector sees no open work, or a list whose last
// task completed or was deleted still reads open.
#[test]
fn a_claude_task_list_is_open_while_a_created_task_is_not_done() {
    let mut tap = Tap::new(Harness::Claude);
    let calls = [
        task_tool(
            "toolu_c1",
            "TaskCreate",
            r#"{"subject":"Write tests","description":"Cover the parser"}"#,
            "Task #1 created successfully: Write tests",
        ),
        task_tool(
            "toolu_c2",
            "TaskCreate",
            r#"{"subject":"Ship it","description":"Open the PR"}"#,
            "Task #2 created successfully: Ship it",
        ),
        task_tool(
            "toolu_u1",
            "TaskUpdate",
            r#"{"taskId":"1","status":"completed"}"#,
            "Updated task #1 status",
        ),
    ];
    let lines: Vec<(Pipe, &str)> = calls
        .iter()
        .flatten()
        .map(|(pipe, line)| (*pipe, line.as_str()))
        .collect();
    let open = feed(&mut tap, 0, &lines).unwrap();
    assert!(open.todo_open, "task #2 is still pending");
    let [create, done] = task_tool(
        "toolu_u2",
        "TaskUpdate",
        r##"{"taskId":"#2","status":"deleted"}"##,
        "Updated task #2",
    );
    let done = feed(
        &mut tap,
        20_000,
        &[(create.0, &create.1), (done.0, &done.1)],
    )
    .unwrap();
    assert!(!done.todo_open, "#1 completed and #2 deleted");
}

// Defect (review m1): Claude's keep_alive, which the front end drops
// (claude.ts:644), moves the progress time, so a turn stalled on its API
// stream is never hung.
#[test]
fn a_claude_keep_alive_is_not_progress() {
    let mut tap = Tap::new(Harness::Claude);
    let init = tap
        .line(Pipe::Stdout, r#"{"type":"system","subtype":"init"}"#, 0)
        .unwrap();
    tap.wrote(init, 0);
    assert_eq!(
        tap.line(Pipe::Stdout, r#"{"type":"keep_alive"}"#, 60_000),
        None
    );
    let row = tap
        .line(Pipe::Stdout, r#"{"type":"assistant"}"#, 120_000)
        .unwrap();
    assert_eq!(row.last_progress_at, 120_000, "the next real frame is");
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
