//! The desktop executable also provides a small, JSON-only control client.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Value};

const USAGE: &str = r#"MonoCode local control — supervise this orchestration run from the lead agent.

Usage: {exe} control ACTION [--json JSON | --input FILE|-] [--request-id ID]

Actions, with the JSON object each one takes:
  list      {}
            The run, every task with its status and latest result, and the
            harness/model IDs you may assign.
  delegate  {"title":"Short title","harness":"<id from list>",
             "model":"<id from list>","prompt":"Self-contained instructions",
             "files":["src/feature"],"dependsOn":["<taskId>"]}
            Queue a worker and return its taskId. "harness" and "model" are
            required; use exact IDs that list returns for this run.
            "files" is the write scope: project-relative paths, where a
            directory covers its descendants and ["."] reserves the whole
            checkout. "dependsOn" holds taskIds that must be reviewed first.
  get       {"taskId":"..."}
            One task, including its latest result.
  wait      {"timeoutSeconds":20}
            Block until a task changes state, or until the timeout (0-25).
            Returns at once when paused, stopped, or nothing is running or queued.
  respond   {"taskId":"...","requestId":7,"decision":"allow"|"deny"}
            Answer an approval an agent is blocked on. Agents never prompt the
            user; list, get and wait report the prompt as that task's
            "needsInput", and it stays stopped until you decide.
  answer    {"taskId":"...","requestId":9,"answers":{"<questionId>":["<optionId>"]}}
            Answer a question an agent asked, or pass "skip":true instead of
            "answers". The question and its options come from needsInput.
  steer     {"taskId":"...","text":"..."}
            Redirect an agent that is still running, without discarding the
            work it has already done. Use this the moment you see it going
            the wrong way; message only lands once it has stopped.
  message   {"taskId":"...","text":"..."}
            Send a stopped worker another turn within its existing scope; it
            keeps its session, checkout and history.
  retry     {"taskId":"...","text":"...","files":["src/feature"]}
            Retry a stopped worker with corrected project-relative write
            scopes. Use this only when the additional files are required.
  cancel    {"taskId":"..."}
            Cancel a task, whether it is running or still queued.
  review    {"taskId":"..."}
            Accept a completed task's result.
  finish    {}
            End the run, once every task is accepted or cancelled.

Usual loop: list -> delegate ... -> wait or get -> steer an agent that drifts,
unblock one with respond or answer -> inspect the changes yourself -> message
for corrections -> review each task -> finish.

When paused, list, get and wait still return the reason and recovery steps.
Do not keep polling or retry mutations. Explain the pause and ask the user to
click Resume in MonoCode. Resume continues interrupted workers in their
retained checkouts. A policy-blocked worker remains stopped until message,
retry or cancel explicitly resolves it.

Output is one JSON line: {"ok":true,"result":...} or {"ok":false,"error":"..."}.
The exit code is 0 only when "ok" is true.

Input must be a JSON object; unknown fields are rejected rather than ignored.
--json takes it inline, --input FILE reads a file, --input - reads stdin.

Every call carries a request ID, and the run applies each ID at most once. A
failed response reports the ID it used whenever the outcome is unknown — a
timeout, say. Retry that exact call with --request-id ID; retrying a delegate
under a fresh ID instead would queue a second worker.

Tasks run inside the MonoCode app, not in this process. Exiting this CLI, or a
failure here, never cancels a task that was already accepted.

MonoCode sets MONOCODE_CONTROL_ENDPOINT and MONOCODE_CONTROL_TOKEN for the lead
agent's process only. They are already in your environment; never print them.
"#;

const ACTIONS: [&str; 12] = [
    "list", "delegate", "get", "steer", "message", "retry", "cancel", "wait", "review", "finish",
    "respond", "answer",
];
const APP_ACTIONS: [&str; 47] = [
    "models.list",
    "sessions.list",
    "sessions.read",
    "sessions.send",
    "sessions.draft",
    "sessions.start",
    "sessions.stop",
    "sessions.archive",
    "sessions.delete",
    "monos.list",
    "monos.get",
    "monos.read",
    "messages.get_monos",
    "messages.send",
    "messages.reply",
    "messages.get",
    "monos.create",
    "monos.update",
    "workspace.status",
    "worktrees.list",
    "worktrees.create",
    "folders.list",
    "folders.move",
    "milestones.list",
    "milestones.add",
    "milestones.update",
    "milestones.remove",
    "milestones.reorder",
    "notes.list",
    "notes.read",
    "notes.write",
    "artifacts.list",
    "artifacts.read",
    "artifacts.write",
    "soul.read",
    "soul.update",
    "memory.read",
    "memory.search",
    "memory.add",
    "memory.replace",
    "memory.remove",
    "habits.list",
    "habits.add",
    "habits.update",
    "habits.run",
    "habits.remove",
    "chat.card",
];
/// Renamed app actions. They still go to the app, which answers
/// `action_removed` with the name to use instead; help does not list them.
const REMOVED_APP_ACTIONS: [&str; 2] = ["monos.configure", "monos.send"];
const APP_USAGE: &str = r##"MonoCode app access — use in a thread enabled by /operator, or in a
session that sessions.start started.

Usage: {exe} app ACTION [--json JSON | --input FILE|-] [--request-id ID]

A Mono works on several projects: add "project":"<path or name>" to the
sessions.*, worktrees.*, folders.* and milestones.* actions to choose which
one. It may be left out when the Mono has a single project.

Levels: a session that a lead session starts with sessions.start is a worker
of that lead. Every other session is a lead. A Mono chat, a habit run, or a
session with app access (/operator, or started by sessions.start) may call
messages.*. A worker may call only messages.*, and it messages only its lead:
sessions.start gets worker_cannot_spawn and its other calls get worker_scope.
A lead messages its own workers and the Monos that have its project; another
lead or a user session gets lead_scope. A Mono messages a lead in one of its
projects and never a worker. Only a worker's lead may message it or use
sessions.send or sessions.draft on it; other callers get steer_via_lead. A
lead or worker always sends wanting_reply false (else session_cannot_wait)
and sends only intent "new" to its lead or a Mono (a steer with reply_to gets
upward_steer_forbidden, one with scope steer_scope_not_allowed). Until
/operator is typed in it, a session that sessions.start started cannot start
a session with a more permissive runtimeMode, and as a lead it may only list
models, sessions and worktrees, create worktrees, start workers, and read,
send to, draft in, stop, archive or delete its own workers; its other
actions, except messages.*, get lead_scope.

Actions:
  models.list    {}  Available providers, models, settings and permission modes.
  sessions.list  {}  Project sessions with IDs, busy status, hasDraft and archived.
  sessions.read  {"sessionId":"...","before":"<turnId>","limit":3,"maxChars":1200}
                  Read up to 3 recent user/assistant exchanges. Tools and
                  reasoning are omitted. Omit before for the newest page;
                  pass nextBefore from a result for older exchanges. maxChars
                  caps each message (200-6000, default 1200).
  sessions.send  {"sessionId":"...","prompt":"...","whenBusy":"steer",
                  "notifyOnComplete":true}
                  Send a prompt to a session in this project. An idle
                  session runs it as a new turn. Without whenBusy, a busy
                  session returns an error. whenBusy:"steer" adds the prompt
                  to the running turn, like Steer in MonoCode. A harness that
                  cannot steer mid-turn returns an error. whenBusy:"followUp"
                  adds it to the session's queue in MonoCode, where the user
                  can edit or remove it. It is sent after the turn ends,
                  unless the queue is paused (for example after Stop). A
                  busy Mono's own chat does not accept whenBusy.
                  The result's delivery is sent, steered or queued. Queued
                  results also have queuedMessageId. omp reads steering
                  between tool calls. Current omp can move a long shell
                  command to the background, so the prompt arrives while
                  that command runs. Reuse --request-id on retries.
                  Optional notifyOnComplete:true asks for a completion
                  report in the calling Mono's chat when the turn that uses
                  the prompt ends. It waits until that Mono is idle.
  sessions.draft {"sessionId":"...","prompt":"..."}
                  Save an unsent draft in an idle project session. Existing
                  drafts are preserved; send or remove one in MonoCode first.
                  Reuse --request-id on retries.
  sessions.start {"prompt":"...","harness":"codex","model":"codex:...",
                  "effort":"high","reveal":false,
                  "workspaceMode":"current","worktreeCwd":"<path>","draft":false,
                  "placement":"right",
                  "besideSessionId":"<visible session ID>"}
                  "harness" and "model" are required: copy exact IDs from
                  models.list. There is no default; a missing or unknown
                  harness or model returns an error. Create a tab with the
                  prompt, or set placement to right or down to split a
                  visible session pane. A split defaults to the
                  calling session; besideSessionId chooses another visible
                  session in this project, including one just created. Set
                  draft:true to save the prompt unsent; no agent turn runs.
                  Otherwise the turn is submitted.
                  Submitted sessions notify the calling Mono by default when
                  this turn completes, fails or is cancelled. The Mono reviews
                  it and reports back once idle. Set notifyOnComplete:false
                  when the user asks not to receive a report. Drafts do not
                  notify; notifyOnComplete:true cannot be combined with draft:true.
                  Sessions monitored during the same Mono turn form one group:
                  results ready within 60 seconds after that turn ends arrive
                  together, and each later session reports when it stops.
                  The Mono gives one combined report for each notice.
                  Rejected launches or follow-ups return a CLI error without
                  a later completion report. When a Mono successfully stops,
                  archives or deletes a monitored session, its pending report
                  for that session is dismissed; acknowledge the action in
                  the current reply. Other sessions' reports are kept.
                  Returns after creation/acceptance, not agent completion;
                  use its ID with folders.move immediately. Omit effort and
                  modelSettings to keep this session's settings when you pick
                  its own model, or the model's defaults otherwise.
                  Set worktreeCwd to a path from
                  worktrees.list to choose a specific existing checkout, or
                  workspaceMode:"worktree" and optional worktreeBase to make
                  a new worktree with an automatic branch name. A session
                  a Giga's chat starts in the MonoCode project runs in the
                  Giga's own gigas/<slug>/ folder and can commit there; pass
                  the project path as worktreeCwd to run at its root. Omit
                  runtimeMode to inherit this
                  session's permission mode; set it to override (see Levels
                  for a session that sessions.start started). Run
                  models.list for allowed IDs. cwd is your project; no attachments.
                  A submitted start also returns message_id, the start's
                  message: pass it as reply_to with intent "steer" in
                  messages.send to change the work it started. A draft has
                  no message_id. A lead session's start makes a worker of
                  that lead; a Mono's start makes a lead. The same start
                  again with the same --request-id answers the same session.
                  The id used again with other input, or for a start whose
                  session was archived or deleted, gets request_id_reused
                  before anything launches. Codes: worker_cannot_spawn,
                  request_id_reused.
  sessions.stop {"sessionId":"..."}
                  Stop a session's current turn and pause its queued messages.
                  The conversation and checkout are kept. Idle sessions are
                  unchanged. Reuse --request-id on retries.
  sessions.archive {"sessionId":"..."}
                  Stop the session if running, save its conversation, and
                  archive it. It can be restored from MonoCode's archive.
                  Open files, terminals and worktrees are kept.
                  Reuse --request-id on retries.
  sessions.delete {"sessionId":"..."}
                  Stop the session if running and permanently delete its
                  saved conversation. Open files, terminals and worktrees
                  are kept. Reuse --request-id on retries.
                  stop, archive and delete cannot target the calling session,
                  Mono chats, habit runs or orchestration workers. Sessions
                  must belong to the chosen project.
  monos.list     {}  Mono or habit only. The Monos in MonoCode, with id,
                  name, role (giga or mono), projects, busy, queued,
                  hasConversation, updatedAt and self (true for you).
  monos.get      {"monoId":"..."}  Mono or habit only. One Mono as monos.list
                  shows it, plus the harness and model its chat runs on (null
                  until it has a chat) and its messageAllowlist. Unknown IDs
                  get code unknown_mono.
  monos.read     {"monoId":"...","before":"<turnId>","limit":3,"maxChars":1200,
                  "work":true}
                  Read another Mono's chat. Paging works as in sessions.read.
                  work:true also lists each turn's reasoning and tool rows
                  in order as "work", each capped by maxChars. A turn keeps
                  its newest rows up to 1 MB; "omittedWork" counts the rest.
                  Codes: unknown_mono, no_conversation (it has no chat yet),
                  invalid_input (a before that is not a turn ID).
  messages.get_monos {}  Every Mono in rail order as {mono_id, mono_name,
                  role, you}; you is true for yourself.
  messages.send  {"message":"...","send_to":"<mono_id or session id>",
                  "wanting_reply":false,"intent":"new"}
                  Send a message to another Mono's chat or to a session in
                  one of your projects, as Levels allows. A Giga may message
                  any regular Mono. Two Gigas, or two regular Monos, need a
                  project they share, or one of them in the other's
                  messageAllowlist (see monos.update); else
                  direction_forbidden. A regular Mono sends a Giga only
                  intent "new" or steers it with reply_to set to an
                  unanswered message that Giga's chat sent it in the last
                  24 hours (else upward_steer_forbidden), while the Giga's
                  chat runs (else target_gone).
                  wanting_reply and intent are required. wanting_reply is
                  true only when you need an answer. The answer arrives later
                  as a new message, so do not poll. A project session cannot
                  answer; send it wanting_reply false. intent "new" starts
                  new work and waits until the receiver's turn ends. intent
                  "steer" changes work a message of yours started: pass
                  reply_to, the message_id your send returned; it reaches the
                  session running that work, and is refused when that work is
                  not running. intent "steer" with "scope":"turn" changes
                  everything the recipient is doing now (a Giga to a regular
                  Mono, or a regular Mono to a lead in one of its projects);
                  it never reaches the recipient's sessions. Never pass both
                  reply_to and scope. Answer a waiting message with
                  messages.reply. continue is for users only. Returns
                  message_id, status (open or delivered; dropped when a steer
                  missed its turn), delivery (submit, queue, steer, or
                  dropped) and hop. Limits for every sender: 6 messages to
                  one recipient in 10 minutes (rate_limited; replies and
                  refused calls do not count), chains of replies and steers
                  4 deep (hop_limit), and 8000 characters (too_large). Codes:
                  invalid_input, request_id_reused, too_large,
                  intent_mismatch, steer_needs_target, intent_not_allowed,
                  worker_scope, unknown_target, self_send,
                  direction_forbidden, steer_via_lead, lead_scope,
                  steer_scope_not_allowed, session_cannot_wait,
                  upward_steer_forbidden, unknown_message,
                  steer_target_mismatch, reply_unsupported_target,
                  habit_cannot_wait, target_gone, nothing_running,
                  target_no_conversation, cannot_steer, hop_limit,
                  rate_limited.
  messages.reply {"message":"...","to":"#1","wanting_reply":false}
                  Answer a <mono_message> that waits for your reply (it says
                  wanting_reply="true" and has a handle such as
                  handle="#1"). Handles count from #1 in each turn and close
                  when the turn ends, or when the sender's chat is reset,
                  archived or deleted. Without to, reply answers the one open
                  handle. to is one handle, or a list of up to 9 different
                  handles such as ["#1","#2"]; each sender gets its own
                  answer. Returns message_ids and answered. Codes:
                  no_open_handle, ambiguous_reply and unknown_handle (with
                  open_handles), handle_closed, hop_limit, target_gone,
                  too_large.
  messages.get   {"handle":"#1"}  The full text of a message you sent or
                  received. handle is its message_id, or
                  a handle such as "#1" from this turn. Call it only when the
                  original is no longer in your context. Returns message with
                  message_id, from, to, intent, reply_to, body, wanting_reply,
                  status, created_at, and handle when the message holds one in
                  this turn. Writes nothing. Codes: invalid_input,
                  unknown_handle, unknown_message (no message you can read
                  has that id).
  monos.create   {"name":"...","harness":"codex","model":"codex:...",
                  "projects":["<path or name>"],"soul":"<Markdown>","giga":false}
                  Giga Mono or its habits only. Create a Mono and its chat on
                  that harness and model. harness and model are required; run
                  models.list for exact IDs. projects must be projects in
                  MonoCode; pass [] for none. soul is optional and becomes its
                  SOUL.md. giga can only be false. name is 1 to 256
                  characters. Returns monoId and settings. Codes: giga_only
                  for other callers, invalid_input.
  monos.update   {"monoId":"...","harness":"codex","model":"codex:...",
                  "name":"...","projects":["<path or name>"],
                  "messageAllowlist":["<mono_id>"]}
                  Giga Mono or its habits only. Change another Mono's chat
                  harness and model, name, projects or messageAllowlist. A
                  model alone must suit its current harness; a harness needs
                  a model. name is 1 to 256 characters. messageAllowlist
                  holds mono_id values of other Monos (from get_monos): this
                  Mono and each Mono in the list may message each other
                  without a shared project; [] clears it. Codes: giga_only
                  for other callers, self_update for yourself, unknown_mono,
                  mono_busy (nothing changed; try again when it is idle),
                  invalid_input.
  workspace.status {}  Mono or its habits only. The shared Giga workspace
                  ~/MonoCode: root, state ready or blocked (with reason),
                  gigaSlug for a Giga, and lastEnsure {at, trigger, created,
                  error}. A Giga writes only in ~/MonoCode/gigas/<gigaSlug>/.
  worktrees.list {}  Working copies in this project, with paths and branches.
  worktrees.create {"branch":"feature/name","base":"HEAD","existing":false}
                  Create a worktree on a named new branch from base (a branch
                  or ref). Set existing:true and omit base to use an existing
                  local branch. Pass the returned path as sessions.start's
                  worktreeCwd to start there.
  folders.list   {}  Folders in your current project.
  folders.move   {"sessionId":"...","folderId":"..."}
                  Or use "newFolderName":"Research" to create a folder.
  milestones.list {}  The project's roadmap: settings {enabled,
                  hiddenStatuses} and milestones in order, each with id,
                  title, note, status, target, order, createdBy, createdAt
                  and updatedAt. It answers while milestones are off.
                  Codes: invalid_input.
  milestones.add {"title":"Dark mode","note":"...","status":"idea",
                  "target":"2026-12-01"}
                  Keep a future goal for the project. status is idea
                  (default), planned, in_progress or done. target is a
                  YYYY-MM-DD date, or "" or null for later. title is 1 to
                  200 characters, note at most 4000. MonoCode records you
                  as the writer; do not send createdBy or by. Returns the
                  new milestone. Codes: invalid_input, milestones_off (the
                  user turned them off; do not retry).
  milestones.update {"id":"...","status":"in_progress","target":null}
                  Change title, note, status or target. Omitted fields stay;
                  target "" or null clears the date. Codes:
                  unknown_milestone (not in this project), invalid_input,
                  milestones_off.
  milestones.remove {"id":"..."}  Delete one. Returns removed. Codes:
                  unknown_milestone, invalid_input, milestones_off.
  milestones.reorder {"ids":["...","..."]}
                  Every id of the project, in the new order. Returns project
                  and milestones in the new order. Codes: stale_order (ids
                  are not exactly the project's; run milestones.list),
                  invalid_input, milestones_off.
  notes.list     {"limit":30,"offset":0}  Titles and short previews only.
  notes.read     {"id":"..."}  Full body of one note.
  notes.write    {"title":"Plan","body":"Markdown","tags":["work"]}
                  Create a note linked to this session and project. Omit title
                  to derive it from the body. Use {"id":"...","body":"..."}
                  to edit an existing note; title and tags are also optional.
                  Omitted fields stay unchanged. Reuse --request-id on retries.
  artifacts.list {"limit":30,"offset":0}  Mono or habit only. Saved artifact titles.
  artifacts.read {"id":"..."}  Full content of one artifact.
  artifacts.write {"kind":"document","title":"PR review",
                   "summary":"Merge queue and blockers",
                   "body":"<complete Markdown>"}
                  Save a document and attach its card below your chat reply.
                  Artifacts are separate from Notes. Currently only kind "document"
                  (Markdown) is supported. Reply briefly; do not
                  repeat the document body in chat. Returns metadata only.
                  To revise, pass {"id":"...","body":"<updated Markdown>"};
                  omitted title stays unchanged. Reuse --request-id on retries.
  soul.read      {}  Mono's own conversation only. Current SOUL.md text and hash.
  soul.update    {"text":"<complete Markdown>","expectedHash":"<hash from soul.read>"}
                  Update your standing instructions only when the user asks.
                  Preserve the other instructions. If the file changed since
                  soul.read, read it again and reapply the requested changes.
                  Habit runs and other sessions cannot change a Mono's soul.
  memory.read    {"topic":"releases"}  Mono only. Without topic:
                  MEMORY.md, how much of it loads, and the topic names.
  memory.search  {"query":"release tags","since":"7d"}
                  Entries across MEMORY.md, topic notes and the archive that
                  share words with query, best first. since is a date or a
                  span (24h, 7d, 2w) and keeps dated entries from then on.
  memory.add     {"fact":"...","topic":"releases","until":"2026-11-01"}
                  Add one dated entry to MEMORY.md, or to a topic file when
                  topic is set. until is optional, for facts that expire.
                  Oldest entries move to the archive when MEMORY.md is full.
  memory.replace {"find":"text of the old entry","fact":"...","topic":"..."}
                  Strike the one entry containing find through and add fact.
  memory.remove  {"find":"text of the entry","topic":"..."}
                  Delete the one entry containing find, for a wrong entry.
  habits.list    {}  Mono only. Your habits: what each does, when it runs
                  next, and how its last run went.
  habits.add     {"name":"Morning CI check","instructions":"...",
                  "schedule":{"kind":"weekdays","time":"09:00"}}
                  Add only after the user agreed to it in this chat. kind is
                  hourly (with "minute"), daily, weekdays or weekly (with
                  "dayOfWeek", 0 = Sunday); time is local 24-hour HH:MM.
                  Each run is its own session in your first project, listed
                  in your overview. It posts to this chat only when it has
                  something worth saying.
  habits.update  {"id":"...","name":"...","instructions":"...",
                  "schedule":{...},"enabled":false}  Change or pause one.
  habits.run     {"id":"..."}  Run one within a minute, to try it out.
  habits.remove  {"id":"..."}
  chat.card      Mono or habit only. Post a card to the Mono's chat:
                  {"type":"pr","repo":"owner/repo","number":123,"note":"..."}
                  {"type":"session","sessionId":"...","note":"..."}
                  {"type":"choices","options":["First choice","Second choice"]}
                  {"type":"habit","name":"...","instructions":"...",
                   "schedule":{"kind":"daily","time":"09:00"}}
                  choices accepts 1–4 options. A habit card is a suggestion;
                  the user must start it before it is scheduled.

The output is one JSON line: {"ok":true,"result":...} or {"ok":false,"error":"..."}.
A failure with "code" and "hint" is definite: read the hint, fix the call once,
and send it as a new call. Use --input - to pass JSON on stdin.
Never print MonoCode credentials.
Keep the same --request-id when retrying a call after an uncertain result.
"##;

/// Quote for the shell the lead agent actually runs commands in, and only when
/// the path needs it. The path is absolute, so a leading slash means a POSIX
/// shell — where a backslash escapes rather than separates, and so is never
/// safe bare.
fn quoted(value: &str) -> String {
    if !value.starts_with('/') {
        return if value.contains([' ', '\t', '"']) {
            format!("\"{}\"", value.replace('"', ""))
        } else {
            value.into()
        };
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._/-:".contains(c))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

pub fn help() -> String {
    let exe = std::env::current_exe()
        .map(|path| quoted(&path.to_string_lossy()))
        .unwrap_or_else(|_| "monocode".into());
    USAGE.replace("{exe}", &exe)
}

pub fn app_help() -> String {
    let exe = std::env::current_exe()
        .map(|path| quoted(&path.to_string_lossy()))
        .unwrap_or_else(|_| "monocode".into());
    APP_USAGE.replace("{exe}", &exe)
}

enum Parsed {
    Help,
    Call(String, Value, String),
}

pub fn run(args: Vec<String>) -> i32 {
    run_mode(args, false)
}

pub fn run_app(args: Vec<String>) -> i32 {
    run_mode(args, true)
}

fn run_mode(args: Vec<String>, app_mode: bool) -> i32 {
    let parsed = match parse_args_for(&args, app_mode) {
        Ok(parsed) => parsed,
        Err(error) => {
            println!("{}", error.response());
            return 1;
        }
    };
    let (action, input, request_id) = match parsed {
        Parsed::Help => {
            println!("{}", if app_mode { app_help() } else { help() });
            return 0;
        }
        Parsed::Call(action, input, request_id) => (action, input, request_id),
    };
    match send(&action, &input, &request_id, app_mode) {
        Ok(mut value) => {
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                println!("{value}");
                return 0;
            }
            value = with_retry_hint(value, &request_id);
            println!("{value}");
            1
        }
        Err(failure) => {
            println!("{}", failure_response(failure, &request_id));
            1
        }
    }
}

fn with_retry_hint(mut value: Value, request_id: &str) -> Value {
    // A denied app call never reached the executor. Repeating the same ID
    // cannot grant access, and suggesting it sends agents in loops.
    // A failure with a code is the app's definite answer to this input.
    if value.get("retryable").and_then(Value::as_bool) == Some(false) || value.get("code").is_some()
    {
        return value;
    }
    if let Some(object) = value.as_object_mut() {
        object
            .entry("requestId")
            .or_insert_with(|| json!(request_id));
        object
            .entry("retryWith")
            .or_insert_with(|| json!(format!("--request-id {request_id}")));
    }
    value
}

struct Failure {
    error: String,
    /// The request was already on the wire, so the run may have applied it.
    sent: bool,
    /// Set when the failure is definite, as an app answer with a code is.
    code: Option<&'static str>,
}
fn unsent(error: impl Into<String>) -> Failure {
    Failure {
        error: error.into(),
        sent: false,
        code: None,
    }
}
fn sent(error: impl Into<String>) -> Failure {
    Failure {
        error: error.into(),
        sent: true,
        code: None,
    }
}

const NO_TOKEN: &str =
    "MONOCODE_APP_TOKEN is not set. Only agent sessions that MonoCode started can call these tools.";

fn failure_response(failure: Failure, request_id: &str) -> Value {
    if let Some(code) = failure.code {
        return json!({"ok": false, "code": code, "hint": failure.error, "error": failure.error});
    }
    // The call may have reached the run even though its answer was lost.
    // Hand back the request ID so a retry cannot duplicate it.
    let mut response = json!({"ok": false, "error": failure.error});
    if failure.sent {
        response["requestId"] = json!(request_id);
        response["retryWith"] = json!(format!("--request-id {request_id}"));
    }
    response
}

/// The endpoint and token this agent process was started with.
fn connection(
    app_mode: bool,
    var: impl Fn(&str) -> Option<String>,
) -> Result<(String, String), Failure> {
    let var = |key: &str| var(key).filter(|value| !value.is_empty());
    if app_mode {
        return match (var("MONOCODE_APP_ENDPOINT"), var("MONOCODE_APP_TOKEN")) {
            (Some(endpoint), Some(token)) => Ok((endpoint, token)),
            _ => Err(Failure {
                code: Some("no_token"),
                ..unsent(NO_TOKEN)
            }),
        };
    }
    let endpoint = var("MONOCODE_CONTROL_ENDPOINT").ok_or_else(|| {
        unsent("No MonoCode connection. Confirm the Orchestrator proposal in MonoCode first.")
    })?;
    let token = var("MONOCODE_CONTROL_TOKEN")
        .ok_or_else(|| unsent("No MonoCode session credential. Start the agent from MonoCode."))?;
    Ok((endpoint, token))
}

fn send(action: &str, input: &Value, request_id: &str, app_mode: bool) -> Result<Value, Failure> {
    let (endpoint, token) = connection(app_mode, |key| std::env::var(key).ok())?;
    send_to(&endpoint, &token, action, input, request_id, app_mode)
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(40);
fn send_to(
    endpoint: &str,
    token: &str,
    action: &str,
    input: &Value,
    request_id: &str,
    app_mode: bool,
) -> Result<Value, Failure> {
    let address: SocketAddr = endpoint
        .parse()
        .map_err(|_| unsent("Invalid MonoCode endpoint"))?;
    if !address.ip().is_loopback() {
        return Err(unsent("MonoCode control only connects to localhost"));
    }
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|error| {
            unsent(format!(
                "Cannot connect to MonoCode at {address}: {error}. The app may have restarted, or this agent's sandbox may be blocking localhost."
            ))
        })?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| unsent(e.to_string()))?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|e| unsent(e.to_string()))?;
    writeln!(
        stream,
        "{}",
        json!({"token":token,"action":action,"input":input,"requestId":request_id,"namespace":if app_mode { "app" } else { "control" }})
    )
    .map_err(|e| sent(e.to_string()))?;
    let mut line = String::new();
    let max_response: u64 = if app_mode { 4_000_000 } else { 2_000_000 };
    BufReader::new(stream)
        .take(max_response + 1)
        .read_line(&mut line)
        .map_err(|e| sent(format!("No reply from MonoCode: {e}")))?;
    if line.len() > max_response as usize {
        return Err(sent("MonoCode response is too large"));
    }
    serde_json::from_str(&line).map_err(|_| sent("MonoCode returned an invalid response"))
}

/// The Mono tools, as MCP (`app-mcp`) and omp host tools both expose them
/// (design 3 and 4). `action` is the app action each one calls. No
/// milestone schema takes `createdBy` or `by`: the app sets the writer.
#[expect(clippy::too_many_lines, reason = "baseline 2026-10")]
pub fn app_tools() -> Value {
    let project = json!({"type": "string", "minLength": 1, "description": "Path or name of one of your projects. May be left out when you have a single project."});
    let id = json!({"type": "string", "minLength": 1, "maxLength": 128});
    let title = json!({"type": "string", "minLength": 1, "maxLength": 200});
    let note = json!({"type": "string", "maxLength": 4000});
    let status = json!({"type": "string", "enum": ["idea", "planned", "in_progress", "done"]});
    let target = json!({"type": ["string", "null"], "pattern": "^(\\d{4}-\\d{2}-\\d{2})?$", "description": "YYYY-MM-DD, or \"\" or null for later."});
    json!([
        {
            "name": "get_monos",
            "action": "messages.get_monos",
            "description": "List every Mono in MonoCode in rail order as {mono_id, mono_name, role, you}. you is true for yourself. Use a mono_id as send_to.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "send_message",
            "action": "messages.send",
            "description": "Send a message to another Mono's chat (send_to is its mono_id) or to a session in one of your projects (its session id). MonoCode decides the sender and the route. Set wanting_reply true only when you need an answer; the answer arrives later as a new message, so do not poll. A project session cannot answer. intent \"new\" starts new work and waits until the receiver's turn ends; intent \"steer\" with reply_to changes the work that message started, while it runs. Messages are limited to 8000 characters. A refusal has code and hint; follow the hint.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {"type": "string", "minLength": 1, "description": "The text to send."},
                    "send_to": {"type": "string", "minLength": 1, "description": "A mono_id from get_monos, or the id of a session in one of your projects."},
                    "wanting_reply": {"type": "boolean", "description": "true only when you need an answer."},
                    "intent": {"type": "string", "enum": ["new", "steer", "reply", "continue"], "description": "new for new work; steer to change work that reply_to started. Answer a waiting message with the reply tool. continue is for users only."},
                    "reply_to": {"type": "string", "minLength": 1, "description": "The message_id (m_…) of a message you sent: steer the work it started. For steer pass reply_to or scope, not both. Refused for new."},
                    "scope": {"type": "string", "enum": ["turn"], "description": "With intent steer and no reply_to: steer everything the recipient is doing now, in its own running turn only. A Giga may steer a regular Mono this way, and a regular Mono a lead in one of its projects. Not with reply_to."}
                },
                "required": ["message", "send_to", "wanting_reply", "intent"],
                "additionalProperties": false
            }
        },
        {
            "name": "reply",
            "action": "messages.reply",
            "description": "Answer the <mono_message> that waits for your reply (it says wanting_reply=\"true\"). MonoCode knows which message you answer. wanting_reply defaults to false.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {"type": "string", "minLength": 1, "description": "Your answer."},
                    "wanting_reply": {"type": "boolean", "description": "true only when you need an answer back. Default false."}
                },
                "required": ["message"],
                "additionalProperties": false
            }
        },
        {
            "name": "get_message",
            "action": "messages.get",
            "description": "Read the full text of a message you sent or received. Call it only when the original is no longer in your context. Returns message_id, from, to, intent, reply_to, body, wanting_reply, status and created_at, and handle when the message holds one in this turn.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "handle": {"type": "string", "minLength": 1, "description": "The message_id, or a handle such as \"#1\" from this turn."}
                },
                "required": ["handle"],
                "additionalProperties": false
            }
        },
        {
            "name": "list_milestones",
            "action": "milestones.list",
            "description": "List the project's milestones (future goals) in order, with settings {enabled, hiddenStatuses}. Each has id, title, note, status, target, order, createdBy, createdAt and updatedAt. It answers while milestones are off.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project},
                "additionalProperties": false
            }
        },
        {
            "name": "add_milestone",
            "action": "milestones.add",
            "description": "Keep a future goal for the project as a milestone. status defaults to idea. MonoCode records you as the writer. Returns the new milestone. Code milestones_off means the user turned milestones off: do not retry.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "title": title,
                    "note": note,
                    "status": status,
                    "target": target
                },
                "required": ["title"],
                "additionalProperties": false
            }
        },
        {
            "name": "update_milestone",
            "action": "milestones.update",
            "description": "Change a milestone's title, note, status or target. Omitted fields stay; target \"\" or null clears the date. Code unknown_milestone means the id is not in this project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "id": id,
                    "title": title,
                    "note": note,
                    "status": status,
                    "target": target
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "remove_milestone",
            "action": "milestones.remove",
            "description": "Delete one milestone. Returns {removed: id}.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project, "id": id},
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "reorder_milestones",
            "action": "milestones.reorder",
            "description": "Set the order of the project's milestones. ids is every id of the project, in the new order. Code stale_order means ids are not exactly the project's: call list_milestones and retry.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project,
                    "ids": {"type": "array", "items": id}
                },
                "required": ["ids"],
                "additionalProperties": false
            }
        }
    ])
}

const MCP_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// `app-mcp`: the Mono messaging tools as a stdio MCP server. The token
/// comes from this process's environment on every call and never leaves it.
pub fn run_app_mcp() -> i32 {
    serve_mcp(std::io::stdin().lock(), std::io::stdout().lock(), |key| {
        std::env::var(key).ok()
    });
    0
}

fn serve_mcp(input: impl BufRead, mut output: impl Write, var: impl Fn(&str) -> Option<String>) {
    for line in input.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = mcp_response(&line, &var) else {
            continue;
        };
        if writeln!(output, "{response}")
            .and_then(|()| output.flush())
            .is_err()
        {
            return;
        }
    }
}

/// The answer to one JSON-RPC line, or `None` for a notification.
fn mcp_response(line: &str, var: &impl Fn(&str) -> Option<String>) -> Option<Value> {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Some(
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
        );
    };
    let invalid = |id: Value| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "Invalid Request"}});
    let Some(object) = message.as_object() else {
        return Some(invalid(Value::Null));
    };
    // A notification (no id) gets no answer.
    let id = object.get("id")?.clone();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(invalid(id));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(mcp_initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": mcp_tools()})),
        "tools/call" => mcp_call(&params, var),
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

fn mcp_initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|version| MCP_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(MCP_PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "monocode", "version": env!("CARGO_PKG_VERSION")}
    })
}

fn mcp_tools() -> Vec<Value> {
    let Value::Array(tools) = app_tools() else {
        return Vec::new();
    };
    tools
        .into_iter()
        .map(|mut tool| {
            if let Some(tool) = tool.as_object_mut() {
                tool.remove("action");
            }
            tool
        })
        .collect()
}

fn mcp_call(params: &Value, var: &impl Fn(&str) -> Option<String>) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let tools = app_tools();
    let action = tools
        .as_array()
        .into_iter()
        .flatten()
        .find(|tool| tool["name"] == name)
        .and_then(|tool| tool["action"].as_str())
        .ok_or_else(|| (-32602, format!("Unknown tool: {name}")))?;
    let input = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(input @ Value::Object(_)) => input.clone(),
        Some(_) => return Err((-32602, "arguments must be an object".into())),
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let value = call_app(action, &input, &request_id, var);
    let ok = value.get("ok").and_then(Value::as_bool) == Some(true);
    Ok(json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
        "isError": !ok
    }))
}

/// One app call, under one request id this server owns. When the outcome is
/// unknown (the app answered "timed out", or the answer was lost after the
/// request was sent) the call is sent again with the same id, which returns
/// the first outcome. If every attempt is unknown the model is told not to
/// send it again: its own retry would get a new id and could deliver twice.
fn call_app(
    action: &str,
    input: &Value,
    request_id: &str,
    var: &impl Fn(&str) -> Option<String>,
) -> Value {
    let (endpoint, token) = match connection(true, var) {
        Ok(connection) => connection,
        Err(failure) => return failure_response(failure, request_id),
    };
    // Once an attempt reached the app, the outcome stays unknown: a later
    // attempt that cannot connect does not prove the first was not applied.
    let mut reached = false;
    for _ in 0..=crate::control::TOOL_CALL_RETRIES {
        match send_to(&endpoint, &token, action, input, request_id, true) {
            Ok(value)
                if value.get("error").and_then(Value::as_str)
                    == Some(crate::control::REQUEST_TIMED_OUT) =>
            {
                reached = true;
            }
            Ok(value) => return value,
            Err(failure) if failure.sent => reached = true,
            Err(_) if reached => break,
            Err(failure) => return failure_response(failure, request_id),
        }
    }
    crate::control::outcome_unknown(action)
}

fn read_capped(mut source: impl Read) -> Result<String, String> {
    let mut raw = String::new();
    source
        .by_ref()
        .take(262_145)
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 262_144 {
        return Err("Input exceeds 256 KiB".into());
    }
    Ok(raw)
}

/// Why the CLI refused a call before sending it. `code` is set when the app
/// would have answered the same call with that code.
struct ParseError {
    code: Option<&'static str>,
    message: String,
}

impl ParseError {
    fn response(&self) -> Value {
        match self.code {
            Some(code) => {
                json!({"ok": false, "code": code, "hint": self.message, "error": self.message})
            }
            None => json!({"ok": false, "error": self.message}),
        }
    }
}

impl From<String> for ParseError {
    fn from(message: String) -> Self {
        ParseError {
            code: None,
            message,
        }
    }
}

impl From<&str> for ParseError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

#[cfg(test)]
impl From<ParseError> for String {
    fn from(error: ParseError) -> Self {
        error.message
    }
}

#[cfg(test)]
fn parse_args(args: &[String]) -> Result<Parsed, ParseError> {
    parse_args_for(args, false)
}

/// In app mode every refusal carries a code: `unknown_action` for the action,
/// `invalid_input` for the input, flags and request ID.
fn parse_args_for(args: &[String], app_mode: bool) -> Result<Parsed, ParseError> {
    parse_call(args, app_mode).map_err(|mut error| {
        if app_mode && error.code.is_none() {
            error.code = Some("invalid_input");
        }
        error
    })
}

fn parse_call(args: &[String], app_mode: bool) -> Result<Parsed, ParseError> {
    let is_help = |value: &str| matches!(value, "help" | "--help" | "-h");
    let Some(action) = args.first() else {
        return Ok(Parsed::Help);
    };
    if is_help(action) {
        return Ok(Parsed::Help);
    }
    let action = action.clone();
    let allowed = if app_mode {
        APP_ACTIONS.contains(&action.as_str()) || REMOVED_APP_ACTIONS.contains(&action.as_str())
    } else {
        ACTIONS.contains(&action.as_str())
    };
    if !allowed {
        let names = if app_mode {
            APP_ACTIONS.join(", ")
        } else {
            ACTIONS.join(", ")
        };
        return Err(ParseError {
            code: app_mode.then_some("unknown_action"),
            message: format!(
                "Unknown action: {action}. Use one of: {names}. Run {} --help.",
                if app_mode { "app" } else { "control" }
            ),
        });
    }
    let mut input = None;
    let mut request_id = uuid::Uuid::new_v4().to_string();
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        // `control delegate --help` should explain the command, not fail.
        if is_help(flag) {
            return Ok(Parsed::Help);
        }
        if !flag.starts_with("--") {
            return Err(format!(
                "Unexpected argument: {flag}. Pass the JSON object as --json '<JSON>'."
            )
            .into());
        }
        let value = args.get(index + 1).ok_or_else(|| {
            ParseError::from(format!(
                "Missing value for {flag}. Run {} --help for the argument list.",
                if app_mode { "app" } else { "control" }
            ))
        })?;
        match flag.as_str() {
            "--request-id" => request_id = value.clone(),
            "--json" | "--input" => {
                if input.is_some() {
                    return Err("Supply only one input".into());
                }
                let raw = if flag == "--json" {
                    if value.len() > 262_144 {
                        return Err("Input exceeds 256 KiB".into());
                    }
                    value.clone()
                } else if value == "-" {
                    read_capped(std::io::stdin())?
                } else {
                    read_capped(
                        std::fs::File::open(value).map_err(|e| format!("--input {value}: {e}"))?,
                    )?
                };
                let parsed: Value = serde_json::from_str(&raw)
                    .map_err(|e| format!("Invalid JSON: {e}. Pass one JSON object, e.g. --json '{{\"taskId\":\"...\"}}'."))?;
                if !parsed.is_object() {
                    return Err("Input must be a JSON object".into());
                }
                input = Some(parsed);
            }
            _ => {
                return Err(format!(
                    "Unknown option: {flag}. Supported: --json, --input, --request-id."
                )
                .into())
            }
        }
        index += 2;
    }
    if request_id.is_empty() || request_id.len() > 128 {
        return Err("Invalid request ID".into());
    }
    if app_mode
        && !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("App request IDs may contain only letters, digits, - and _".into());
    }
    Ok(Parsed::Call(
        action,
        input.unwrap_or_else(|| json!({})),
        request_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    fn call(values: &[&str]) -> Result<(String, Value, String), String> {
        match parse_args(&args(values))? {
            Parsed::Call(action, input, id) => Ok((action, input, id)),
            Parsed::Help => Err("help".into()),
        }
    }
    #[test]
    fn validates_inputs_without_invoking_a_shell() {
        let (_, input, id) = call(&[
            "delegate",
            "--json",
            r#"{"prompt":"$(touch nope) `hello`\nnext"}"#,
            "--request-id",
            "retry-1",
        ])
        .unwrap();
        assert_eq!(id, "retry-1");
        assert_eq!(input["prompt"], "$(touch nope) `hello`\nnext");
        assert!(call(&["delegate", "--json", "[]"]).is_err());
        assert!(call(&["delegate", "--json", "{}", "--json", "{}"]).is_err());
        assert!(call(&["unknown"]).is_err());
    }
    #[test]
    fn explains_help_and_malformed_invocations() {
        assert!(matches!(parse_args(&args(&[])), Ok(Parsed::Help)));
        assert!(matches!(parse_args(&args(&["--help"])), Ok(Parsed::Help)));
        // Agents commonly probe a subcommand for its own usage text.
        assert!(matches!(
            parse_args(&args(&["delegate", "--help"])),
            Ok(Parsed::Help)
        ));
        assert!(call(&["get", r#"{"taskId":"x"}"#])
            .unwrap_err()
            .contains("--json"));
        assert!(call(&["get", "--json"]).unwrap_err().contains("--help"));
        assert!(call(&["get", "--taskId", "x"])
            .unwrap_err()
            .contains("Unknown option"));
        assert!(call(&["get", "--json", "{taskId}"])
            .unwrap_err()
            .contains("Invalid JSON"));
    }
    #[test]
    fn help_names_every_action_and_the_real_executable() {
        let text = help();
        for action in ACTIONS {
            assert!(text.contains(action), "help omits {action}");
        }
        assert!(!text.contains("{exe}"));
        assert!(text.contains("--request-id"));
    }
    #[test]
    fn quotes_the_control_path_only_when_the_shell_needs_it() {
        assert_eq!(
            quoted("/Applications/MonoCode.app/Contents/MacOS/monocode"),
            "/Applications/MonoCode.app/Contents/MacOS/monocode"
        );
        assert_eq!(quoted("/Users/a b/MonoCode"), "'/Users/a b/MonoCode'");
        assert_eq!(quoted("C:\\Tools\\monocode.exe"), "C:\\Tools\\monocode.exe");
        assert_eq!(
            quoted("C:\\Program Files\\MonoCode\\monocode.exe"),
            "\"C:\\Program Files\\MonoCode\\monocode.exe\""
        );
        // A backslash escapes in a POSIX shell, so bare would rewrite the path.
        assert_eq!(quoted("/Users/a\\b/MonoCode"), "'/Users/a\\b/MonoCode'");
        assert_eq!(quoted("/Users/it's/MonoCode"), r"'/Users/it'\''s/MonoCode'");
    }
    #[test]
    fn app_mode_exposes_only_app_actions_and_safe_request_ids() {
        assert!(matches!(
            parse_args_for(&args(&["notes.list"]), true),
            Ok(Parsed::Call(_, _, _))
        ));
        for action in [
            "sessions.read",
            "sessions.send",
            "sessions.draft",
            "sessions.stop",
            "sessions.archive",
            "sessions.delete",
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action, "--json", r#"{"sessionId":"other"}"#]), true),
                Ok(Parsed::Call(_, _, _))
            ));
            assert!(app_help().contains(action));
            assert!(parse_args_for(&args(&[action]), false).is_err());
        }
        assert!(app_help().contains("draft:true"));
        assert!(app_help().contains(r#""whenBusy":"steer""#));
        assert!(app_help().contains(r#"whenBusy:"followUp""#));
        assert!(app_help().contains("inherit this"));
        assert!(parse_args_for(&args(&["delegate"]), true).is_err());
        assert!(
            parse_args_for(&args(&["sessions.start", "--request-id", "bad/id"]), true).is_err()
        );
        assert!(app_help().contains("notes.read"));
        assert!(app_help().contains("notes.write"));
        for action in [
            "worktrees.list",
            "worktrees.create",
            "artifacts.list",
            "artifacts.read",
            "artifacts.write",
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action]), true),
                Ok(Parsed::Call(_, _, _))
            ));
            assert!(app_help().contains(action));
        }
        assert!(app_help().contains(r#""kind":"document""#));
        assert!(app_help().contains("Artifacts are separate from Notes"));
    }

    #[test]
    fn inactive_app_turn_does_not_suggest_retries() {
        let denied = with_retry_hint(json!({"ok":false,"retryable":false}), "id-1");
        assert!(denied.get("retryWith").is_none());
        assert!(denied.get("requestId").is_none());
        let uncertain = with_retry_hint(json!({"ok":false,"error":"timeout"}), "id-1");
        assert_eq!(uncertain["retryWith"], "--request-id id-1");
    }

    #[test]
    fn app_mode_accepts_chat_cards_and_documents_every_type() {
        for input in [
            r#"{"type":"pr","repo":"owner/repo","number":123}"#,
            r#"{"type":"session","sessionId":"other"}"#,
            r#"{"type":"choices","options":["Review","Ship"]}"#,
            r#"{"type":"habit","name":"Check CI","instructions":"Check CI","schedule":{"kind":"daily","time":"09:00"}}"#,
        ] {
            assert!(matches!(
                parse_args_for(&args(&["chat.card", "--json", input]), true),
                Ok(Parsed::Call(action, parsed_input, _))
                    if action == "chat.card"
                        && parsed_input == serde_json::from_str::<Value>(input).unwrap()
            ));
        }
        assert!(parse_args_for(&args(&["chat.card"]), false).is_err());
        let help = app_help();
        assert!(help.contains("chat.card"));
        for kind in ["pr", "session", "choices", "habit"] {
            assert!(help.contains(&format!(r#""type":"{kind}""#)));
        }
    }

    #[test]
    fn app_mode_exposes_soul_actions_and_documents_requested_updates() {
        for (action, input) in [
            ("soul.read", r#"{}"#),
            (
                "soul.update",
                r##"{"text":"# Soul\n","expectedHash":"old-hash"}"##,
            ),
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action, "--json", input]), true),
                Ok(Parsed::Call(parsed_action, parsed_input, _))
                    if parsed_action == action
                        && parsed_input == serde_json::from_str::<Value>(input).unwrap()
            ));
            assert!(parse_args_for(&args(&[action]), false).is_err());
            assert!(app_help().contains(action));
        }
        assert!(app_help().contains("only when the user asks"));
        assert!(app_help().contains("expectedHash"));
        assert!(app_help().contains("Habit runs and other sessions cannot change"));
    }

    #[test]
    fn app_mode_exposes_giga_mono_actions_and_documents_who_may_use_them() {
        for (action, input) in [
            (
                "monos.create",
                r#"{"name":"Scout","harness":"codex","model":"codex:test","projects":["app"],"soul":"Be brief.","giga":false}"#,
            ),
            (
                "monos.update",
                r#"{"monoId":"other","harness":"codex","model":"codex:test","name":"Scout","projects":[]}"#,
            ),
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action, "--json", input]), true),
                Ok(Parsed::Call(parsed_action, parsed_input, _))
                    if parsed_action == action
                        && parsed_input == serde_json::from_str::<Value>(input).unwrap()
            ));
            assert!(parse_args_for(&args(&[action]), false).is_err());
            assert!(app_help().contains(action));
        }
        assert!(app_help().contains("Giga Mono or its habits only"));
    }

    #[test]
    fn app_mode_exposes_workspace_status_for_monos() {
        assert!(matches!(
            parse_args_for(&args(&["workspace.status", "--json", "{}"]), true),
            Ok(Parsed::Call(action, _, _)) if action == "workspace.status"
        ));
        assert!(parse_args_for(&args(&["workspace.status"]), false).is_err());
        assert!(app_help().contains("workspace.status {}  Mono or its habits only"));
    }

    #[test]
    fn app_mode_sends_mono_messaging_actions_and_forwards_the_removed_monos_send() {
        for (action, input) in [
            ("monos.list", r#"{}"#),
            ("monos.get", r#"{"monoId":"other"}"#),
            (
                "monos.read",
                r#"{"monoId":"other","before":"turn-1","limit":3,"maxChars":1200}"#,
            ),
            ("messages.get_monos", r#"{}"#),
            (
                "messages.send",
                r#"{"message":"Check CI","send_to":"other","wanting_reply":true,"intent":"steer","reply_to":"m_aaaaaaaaaaaa"}"#,
            ),
            ("messages.reply", r#"{"message":"CI is green"}"#),
            ("messages.get", r##"{"handle":"#1"}"##),
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action, "--json", input]), true),
                Ok(Parsed::Call(parsed_action, parsed_input, _))
                    if parsed_action == action
                        && parsed_input == serde_json::from_str::<Value>(input).unwrap()
            ));
            assert!(parse_args_for(&args(&[action]), false).is_err());
            // "messages.get" is a prefix of "messages.get_monos", so each
            // entry is matched with the space that follows its name.
            assert!(app_help().contains(&format!("  {action} ")), "{action}");
        }
        // The app answers monos.send with action_removed and the new name.
        assert!(matches!(
            parse_args_for(&args(&["monos.send", "--json", r#"{"monoId":"x"}"#]), true),
            Ok(Parsed::Call(action, _, _)) if action == "monos.send"
        ));
        assert!(!app_help().contains("monos.send"));
    }
    #[test]
    fn app_mode_sends_milestones_actions_and_help_lists_each() {
        for (action, input) in [
            ("milestones.list", r#"{"project":"app"}"#),
            (
                "milestones.add",
                r#"{"title":"Dark mode","target":"2026-12-01"}"#,
            ),
            (
                "milestones.update",
                r#"{"id":"m","status":"done","target":null}"#,
            ),
            ("milestones.remove", r#"{"id":"m"}"#),
            ("milestones.reorder", r#"{"ids":["b","a"]}"#),
        ] {
            assert!(matches!(
                parse_args_for(&args(&[action, "--json", input]), true),
                Ok(Parsed::Call(parsed_action, parsed_input, _))
                    if parsed_action == action
                        && parsed_input == serde_json::from_str::<Value>(input).unwrap()
            ));
            assert!(parse_args_for(&args(&[action]), false).is_err());
            let help = app_help();
            let entry = &help[help.find(&format!("\n  {action} {{")).unwrap() + 1..];
            let mut lines = entry.lines();
            let first = lines.next().unwrap_or_default();
            let block: Vec<&str> = std::iter::once(first)
                .chain(lines.take_while(|line| line.starts_with("   ")))
                .collect();
            assert!(block.join(" ").contains("invalid_input"), "{action}");
        }
        let help = app_help();
        assert!(help.contains("folders.* and milestones.* actions"));
        assert!(help.contains("do not send createdBy or by"));
    }

    // Design 3.4 and 6.6. Defect: a model that reads only the help meets a
    // refusal code the help never names, such as a level code, or a code
    // added to the gate later without help text.
    #[test]
    fn app_help_names_every_level_code() {
        let help = app_help();
        let missing: Vec<&str> = crate::mono_messages::RejectCode::ALL
            .iter()
            .map(|code| code.as_str())
            .filter(|code| !help.contains(code))
            .collect();
        assert!(missing.is_empty(), "help omits {missing:?}");
        // Review r1 N3: the send limits, as string literals of the help
        // text (6 per recipient in 10 minutes, 4 deep, 8000 characters).
        let words = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for limit in [
            "6 messages to one recipient in 10 minutes",
            "4 deep (hop_limit)",
            "8000 characters (too_large)",
        ] {
            assert!(words.contains(limit), "help omits {limit:?}");
        }
    }

    #[test]
    fn monos_configure_is_renamed_to_monos_update_and_delete_is_never_an_action() {
        let help = app_help();
        assert!(help.contains("monos.update"));
        assert!(help.contains("monos.get"));
        assert!(!help.contains("monos.configure"));
        // The app answers the old name with action_removed and its new name.
        assert!(matches!(
            parse_args_for(&args(&["monos.configure", "--json", r#"{"monoId":"x"}"#]), true),
            Ok(Parsed::Call(action, _, _)) if action == "monos.configure"
        ));
        let Err(refused) = parse_args_for(&args(&["monos.delete"]), true) else {
            panic!("monos.delete was accepted");
        };
        let response = refused.response();
        assert_eq!(response["code"], "unknown_action");
        assert_eq!(response["hint"], response["error"]);
    }

    #[test]
    fn app_mode_input_refusals_carry_invalid_input() {
        for case in [
            &["monos.get", "--json", "{bad"][..],
            &["monos.get", "--json", "[]"],
            &["monos.get", "--request-id", "a b"],
            &["monos.get", "--json"],
        ] {
            let Err(refused) = parse_args_for(&args(case), true) else {
                panic!("{case:?} was accepted");
            };
            let response = refused.response();
            assert_eq!(response["code"], "invalid_input", "{case:?}");
            assert_eq!(response["hint"], response["error"], "{case:?}");
        }
        let hint = |case: &[&str]| match parse_args_for(&args(case), true) {
            Err(refused) => refused.response()["hint"].as_str().unwrap().to_string(),
            Ok(_) => panic!("{case:?} was accepted"),
        };
        assert!(hint(&["monos.get", "--json"]).contains("Run app --help"));
        assert!(hint(&["monos.get", "--input", "/nonexistent/file.json"])
            .starts_with("--input /nonexistent/file.json: "));
        // The control CLI keeps its uncoded errors.
        let Err(refused) = parse_args_for(&args(&["get", "--json", "{bad"]), false) else {
            panic!("bad JSON was accepted");
        };
        assert!(refused.response().get("code").is_none());
    }

    #[test]
    fn a_coded_failure_does_not_suggest_retrying_the_same_call() {
        let coded = with_retry_hint(
            json!({"ok":false,"code":"giga_only","hint":"Only a Giga","error":"Only a Giga"}),
            "id-1",
        );
        assert!(coded.get("retryWith").is_none());
        assert!(coded.get("requestId").is_none());
    }

    #[test]
    fn an_app_call_without_its_endpoint_or_token_answers_no_token() {
        let env = |endpoint: Option<&str>, token: Option<&str>| {
            let endpoint = endpoint.map(String::from);
            let token = token.map(String::from);
            move |key: &str| match key {
                "MONOCODE_APP_ENDPOINT" => endpoint.clone(),
                "MONOCODE_APP_TOKEN" => token.clone(),
                _ => None,
            }
        };
        for (endpoint, token) in [
            (None, Some("secret")),
            (Some("127.0.0.1:1"), None),
            (Some("127.0.0.1:1"), Some("")),
        ] {
            let Err(failure) = connection(true, env(endpoint, token)) else {
                panic!("{endpoint:?} {token:?} connected");
            };
            let response = failure_response(failure, "id-1");
            assert_eq!(
                response,
                json!({"ok": false, "code": "no_token", "hint": NO_TOKEN, "error": NO_TOKEN}),
                "{endpoint:?} {token:?}"
            );
        }
        assert!(connection(true, env(Some("127.0.0.1:1"), Some("secret"))).is_ok());
        // The orchestration control CLI keeps its uncoded errors.
        let Err(failure) = connection(false, env(None, None)) else {
            panic!("control connected without a token");
        };
        assert!(failure_response(failure, "id-1").get("code").is_none());
    }

    const TOKEN: &str = "tok3n-never-printed";

    /// A loopback app that answers each connection with the next reply and
    /// returns the requests it read. It stops once no call comes for a second.
    fn fake_app(replies: Vec<Value>) -> (String, std::thread::JoinHandle<Vec<Value>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let accept = move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while std::time::Instant::now() < deadline {
                if let Ok((stream, _)) = listener.accept() {
                    stream.set_nonblocking(false).unwrap();
                    return Some(stream);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        };
        let app = std::thread::spawn(move || {
            replies
                .into_iter()
                .map_while(|reply| {
                    let stream = accept()?;
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    writeln!(&stream, "{reply}").unwrap();
                    Some(serde_json::from_str(&line).unwrap())
                })
                .collect()
        });
        (endpoint, app)
    }

    /// Run `app-mcp` over these JSON-RPC lines; every output line must hide the token.
    fn mcp(endpoint: Option<&str>, lines: &[Value]) -> Vec<Value> {
        let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
        let endpoint = endpoint.map(String::from);
        let mut output = Vec::new();
        serve_mcp(input.as_bytes(), &mut output, |key| match key {
            "MONOCODE_APP_ENDPOINT" => endpoint.clone(),
            "MONOCODE_APP_TOKEN" => endpoint.as_ref().map(|_| TOKEN.to_string()),
            _ => None,
        });
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains(TOKEN), "the token reached the output");
        output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn rpc(id: u64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn send_call(id: u64) -> Value {
        rpc(
            id,
            "tools/call",
            json!({"name": "send_message", "arguments": {"message": "Hi", "send_to": "mono-b", "wanting_reply": false, "intent": "new"}}),
        )
    }

    // Defect: an MCP client cannot start (unsupported version, missing
    // capabilities) or sees tools that accept fields the gate refuses.
    #[test]
    fn app_mcp_initializes_and_lists_the_nine_strict_tools() {
        let out = mcp(
            None,
            &[
                rpc(1, "initialize", json!({"protocolVersion": "2025-03-26"})),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                rpc(2, "initialize", json!({"protocolVersion": "1999-01-01"})),
                rpc(3, "tools/list", json!({})),
                rpc(4, "resources/list", json!({})),
                json!("not a request"),
                json!({"jsonrpc": "2.0", "id": 5}),
            ],
        );
        assert_eq!(out.len(), 6, "a notification was answered: {out:?}");
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(out[0]["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "monocode");
        assert_eq!(out[1]["result"]["protocolVersion"], "2025-06-18");
        let tools = out[2]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_monos",
                "send_message",
                "reply",
                "get_message",
                "list_milestones",
                "add_milestone",
                "update_milestone",
                "remove_milestone",
                "reorder_milestones"
            ]
        );
        for tool in tools {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
            assert!(tool.get("action").is_none(), "{tool}");
        }
        // PR I2. Defect: the schema still offers action_type, or a client
        // refuses continue and reply itself, so the gate never writes the
        // intent_not_allowed row an eval reads.
        let send = &tools[1]["inputSchema"];
        assert_eq!(
            send["required"],
            json!(["message", "send_to", "wanting_reply", "intent"])
        );
        assert_eq!(
            send["properties"]["intent"]["enum"],
            json!(["new", "steer", "reply", "continue"])
        );
        assert_eq!(send["properties"]["reply_to"]["type"], "string");
        assert!(send["properties"].get("action_type").is_none(), "{send}");
        // Whole-turn. Defect: MCP clients cannot send scope, or must send it.
        assert_eq!(send["properties"]["scope"]["enum"], json!(["turn"]));
        // PR I3. Defect: get_message is missing, takes another field, or lets
        // a model leave out the reference.
        let get = &tools[3]["inputSchema"];
        assert_eq!(get["required"], json!(["handle"]));
        assert_eq!(get["properties"]["handle"]["type"], "string");
        assert_eq!(get["properties"].as_object().unwrap().len(), 1);
        // PR I3 review r1 n1. Defect: a model that sees only the MCP schema
        // does not know the answer can carry handle.
        let described = tools[3]["description"].as_str().unwrap();
        assert!(
            described.contains("and handle when the message holds one in this turn"),
            "{described}"
        );
        assert_eq!(out[3]["id"], 4);
        assert_eq!(out[3]["error"]["code"], -32601);
        assert_eq!(out[4]["error"]["code"], -32600);
        assert_eq!(out[4]["id"], Value::Null);
        assert_eq!(out[5]["error"]["code"], -32600);
        assert_eq!(out[5]["id"], 5);
    }

    // Defect: a tool call reaches the app as the wrong action or namespace,
    // without its input, or comes back without the app's answer.
    #[test]
    fn app_mcp_sends_one_app_request_per_tool_call_and_returns_its_answer() {
        let answer = json!({"ok": true, "message_id": "m_abcdefghijkl", "status": "delivered", "delivery": "submit", "hop": 0});
        let (endpoint, app) = fake_app(vec![answer.clone()]);
        let out = mcp(Some(&endpoint), &[send_call(1)]);
        let requests = app.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["action"], "messages.send");
        assert_eq!(requests[0]["namespace"], "app");
        assert_eq!(requests[0]["token"], TOKEN);
        assert_eq!(
            requests[0]["input"],
            json!({"message": "Hi", "send_to": "mono-b", "wanting_reply": false, "intent": "new"})
        );
        let result = &out[0]["result"];
        assert_eq!(result["structuredContent"], answer);
        assert_eq!(result["isError"], false);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            answer
        );
        let refused =
            json!({"ok": false, "code": "self_send", "hint": "You cannot message yourself."});
        let (endpoint, app) = fake_app(vec![refused.clone()]);
        let out = mcp(Some(&endpoint), &[send_call(2)]);
        app.join().unwrap();
        assert_eq!(out[0]["result"]["isError"], true);
        assert_eq!(out[0]["result"]["structuredContent"], refused);
    }

    // M-4. Defect: a milestone tool calls another action (a remove that runs
    // as a reorder, an update that adds a row), drops the model's input,
    // offers a field the app refuses (createdBy or by would let a model pick
    // the writer), requires the wrong fields, or cannot clear a date or set
    // a status.
    #[test]
    fn app_mcp_milestone_tools_call_their_action_with_a_strict_schema() {
        let cases = [
            (
                "list_milestones",
                "milestones.list",
                &["project"][..],
                &[][..],
                json!({"project": "app"}),
            ),
            (
                "add_milestone",
                "milestones.add",
                &["note", "project", "status", "target", "title"][..],
                &["title"][..],
                json!({"title": "Dark mode", "note": "Later", "status": "planned", "target": null}),
            ),
            (
                "update_milestone",
                "milestones.update",
                &["id", "note", "project", "status", "target", "title"][..],
                &["id"][..],
                json!({"id": "m1", "status": "done", "target": "2026-12-01"}),
            ),
            (
                "remove_milestone",
                "milestones.remove",
                &["id", "project"][..],
                &["id"][..],
                json!({"id": "m1"}),
            ),
            (
                "reorder_milestones",
                "milestones.reorder",
                &["ids", "project"][..],
                &["ids"][..],
                json!({"ids": ["m2", "m1"]}),
            ),
        ];
        let listed = mcp(None, &[rpc(1, "tools/list", json!({}))]);
        let tools = listed[0]["result"]["tools"].as_array().unwrap();
        let schema = |name: &str| -> &Value {
            &tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"]
        };
        for (name, _, fields, required, _) in &cases {
            let schema = schema(name);
            let mut keys: Vec<&str> = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort();
            assert_eq!(keys, *fields, "{name}");
            let listed_required = schema.get("required").cloned().unwrap_or(json!([]));
            assert_eq!(listed_required, json!(required), "{name}");
        }
        for name in ["add_milestone", "update_milestone"] {
            let properties = &schema(name)["properties"];
            assert_eq!(
                properties["target"]["type"],
                json!(["string", "null"]),
                "{name}"
            );
            assert_eq!(
                properties["status"]["enum"],
                json!(["idea", "planned", "in_progress", "done"]),
                "{name}"
            );
        }
        let answer = json!({"ok": true, "result": {}});
        let (endpoint, app) = fake_app(vec![answer.clone(); cases.len()]);
        let calls: Vec<Value> = cases
            .iter()
            .zip(1..)
            .map(|((name, _, _, _, input), id)| {
                rpc(id, "tools/call", json!({"name": name, "arguments": input}))
            })
            .collect();
        let out = mcp(Some(&endpoint), &calls);
        let requests = app.join().unwrap();
        assert_eq!(requests.len(), cases.len());
        for (((name, action, _, _, input), request), out) in cases.iter().zip(&requests).zip(&out) {
            assert_eq!(request["action"], *action, "{name}");
            assert_eq!(request["input"], *input, "{name}");
            assert_eq!(out["result"]["structuredContent"], answer, "{name}");
        }
    }

    // M-9 R1. Defect: a client that enforces the tool schema refuses target
    // "" (Later, as the page means it) before the call reaches the app, or
    // lets an ambiguous or partial date through.
    #[test]
    fn app_mcp_target_schema_accepts_empty_for_later() {
        let tools = app_tools();
        for name in ["add_milestone", "update_milestone"] {
            let tool = tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            let target = &tool["inputSchema"]["properties"]["target"];
            let pattern = regex::Regex::new(target["pattern"].as_str().unwrap()).unwrap();
            for ok in ["", "2026-12-12"] {
                assert!(pattern.is_match(ok), "{name} refuses {ok:?}");
            }
            for bad in ["12/11/2026", "2026-1-1", "Dec 12, 2026", " ", "2026-12-12 "] {
                assert!(!pattern.is_match(bad), "{name} allows {bad:?}");
            }
            assert_eq!(target["type"], json!(["string", "null"]), "{name}");
        }
    }

    // Defect: a call the app answered "timed out" is dropped, or retried
    // under a new request id and so applied twice.
    #[test]
    fn app_mcp_retries_a_timed_out_call_with_the_same_request_id() {
        let timed_out = json!({"ok": false, "error": crate::control::REQUEST_TIMED_OUT});
        let (endpoint, app) = fake_app(vec![timed_out.clone(), json!({"ok": true, "monos": []})]);
        let out = mcp(
            Some(&endpoint),
            &[rpc(1, "tools/call", json!({"name": "get_monos"}))],
        );
        let requests = app.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["requestId"], requests[1]["requestId"]);
        assert_eq!(out[0]["result"]["isError"], false);
        let (endpoint, app) = fake_app(vec![timed_out.clone(); 4]);
        let out = mcp(Some(&endpoint), &[send_call(2)]);
        let requests = app.join().unwrap();
        assert_eq!(requests.len(), 3, "more than two retries");
        assert!(requests
            .iter()
            .all(|r| r["requestId"] == requests[0]["requestId"]));
        assert_eq!(out[0]["result"]["isError"], true);
        let answer = &out[0]["result"]["structuredContent"];
        assert_eq!(answer["code"], "outcome_unknown");
        assert!(!answer
            .to_string()
            .contains("Retry with the same request ID"));
        // The hint must never invite a resend: a resend gets a new id.
        let hint = answer["hint"].as_str().unwrap();
        assert!(hint.contains("may already be delivered"), "{hint}");
        assert!(hint.contains("Do not send it again"), "{hint}");
        assert!(!hint.to_lowercase().contains("send again"), "{hint}");
        let (endpoint, app) = fake_app(vec![json!({"ok": true}), json!({"ok": true})]);
        mcp(Some(&endpoint), &[send_call(3), send_call(4)]);
        let requests = app.join().unwrap();
        assert_ne!(
            requests[0]["requestId"], requests[1]["requestId"],
            "two tool calls shared a request id"
        );
    }

    // PR I3 review r1 m3. Defect: over MCP, a get_message whose every
    // attempt timed out tells the model not to call it again.
    #[test]
    fn app_mcp_tells_a_timed_out_get_message_to_read_again() {
        let timed_out = json!({"ok": false, "error": crate::control::REQUEST_TIMED_OUT});
        let (endpoint, app) = fake_app(vec![timed_out; 3]);
        let out = mcp(
            Some(&endpoint),
            &[rpc(
                1,
                "tools/call",
                json!({"name": "get_message", "arguments": {"handle": "#1"}}),
            )],
        );
        assert_eq!(app.join().unwrap().len(), 3);
        let answer = &out[0]["result"]["structuredContent"];
        assert_eq!(answer["code"], "outcome_unknown");
        let hint = answer["hint"].as_str().unwrap();
        assert!(hint.contains("call get_message again"), "{hint}");
    }

    // Defect: the app applies a call but answers too late; app-mcp sends it
    // again under a new request id (so the app delivers it twice), or hands
    // the model a timeout it would answer with a second send.
    #[test]
    fn app_mcp_does_not_deliver_a_late_call_twice() {
        // The first answer is either the app's "timed out" or lost on the way.
        for late in [
            Some(json!({"ok": false, "error": crate::control::REQUEST_TIMED_OUT})),
            None,
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = listener.local_addr().unwrap().to_string();
            // The app applies the first request of an id, but its answer
            // does not arrive; the same id again returns the stored outcome.
            let first = late.clone();
            let app = std::thread::spawn(move || {
                let mut applied = std::collections::HashMap::<String, Value>::new();
                listener.set_nonblocking(true).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    let Ok((stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let id = request["requestId"].as_str().unwrap().to_string();
                    let reply = match applied.get(&id) {
                        Some(outcome) => {
                            let mut outcome = outcome.clone();
                            outcome["duplicate"] = json!(true);
                            Some(outcome)
                        }
                        None => {
                            applied.insert(id, json!({"ok": true, "message_id": "m_abcdefghijkl"}));
                            first.clone()
                        }
                    };
                    if let Some(reply) = reply {
                        writeln!(&stream, "{reply}").unwrap();
                    }
                }
                applied.len()
            });
            let out = mcp(Some(&endpoint), &[send_call(1)]);
            assert_eq!(app.join().unwrap(), 1, "delivered twice after {late:?}");
            assert_eq!(out[0]["result"]["isError"], false, "after {late:?}");
            assert_eq!(
                out[0]["result"]["structuredContent"]["message_id"],
                "m_abcdefghijkl"
            );
        }
    }

    /// codex's `tool_timeout_sec` for `monocode`; `codexMcpServer` in
    /// `core/monoTools.ts` sends the same number.
    const CODEX_TOOL_TIMEOUT: Duration = Duration::from_secs(180);

    // Defect: one tools/call can outlast codex's tool timeout (a silent app
    // makes every attempt wait out connect, write and read), so codex gives
    // up first, the model sees no outcome_unknown and sends the message again.
    #[test]
    fn app_mcp_worst_case_ends_well_before_codex_gives_up() {
        let worst_case = (CONNECT_TIMEOUT + WRITE_TIMEOUT + READ_TIMEOUT)
            * (crate::control::TOOL_CALL_RETRIES as u32 + 1);
        assert!(
            worst_case + Duration::from_secs(30) <= CODEX_TOOL_TIMEOUT,
            "app-mcp may take {worst_case:?}; codex waits {CODEX_TOOL_TIMEOUT:?}"
        );
    }

    // Defect: attempt 1 reached the app and its answer was lost, attempt 2
    // cannot connect, and the model gets a plain connection error, which it
    // answers with a second send.
    #[test]
    fn app_mcp_answers_outcome_unknown_when_a_retry_cannot_connect_after_a_sent_attempt() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let app = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            // No answer, and the app is gone before the retry.
        });
        let out = mcp(Some(&endpoint), &[send_call(1)]);
        app.join().unwrap();
        let answer = &out[0]["result"]["structuredContent"];
        assert_eq!(answer["code"], "outcome_unknown", "{answer}");
    }

    // Defect: without MONOCODE_APP_TOKEN the server crashes or answers a
    // protocol error the model cannot act on.
    #[test]
    fn app_mcp_without_a_token_answers_a_no_token_tool_error() {
        let out = mcp(None, &[send_call(1)]);
        assert_eq!(out[0]["result"]["isError"], true);
        assert_eq!(out[0]["result"]["structuredContent"]["code"], "no_token");
        assert_eq!(out[0]["result"]["structuredContent"]["hint"], NO_TOKEN);
    }
}
