import { emit } from "@tauri-apps/api/event";
import {
  mockConvertFileSrc,
  mockIPC,
  mockWindows,
} from "@tauri-apps/api/mocks";
import type { Approval } from "../../../src/features/approvals/model/approvals";
import type { Verdict } from "../../../src/features/approvals/model/boardDecision";
import type { Automation } from "../../../src/features/automations/model/automations";
import {
  MILESTONES_CHANGED,
  type Milestone,
  type MilestonePatch,
  type MilestoneSettings,
  type MilestoneStatus,
  type NewMilestone,
} from "../../../src/features/milestones/model/milestones";
import {
  customMonoName,
  defaultMonoName,
} from "../../../src/features/monos/model/mono";
import { defaultSoul } from "../../../src/features/monos/model/monoFiles";
import type { SessionSummary } from "../../../src/features/sessions/data/sessionStore";
import type {
  Block,
  HarnessId,
} from "../../../src/features/sessions/model/session";

/** A Mono as Rust `MonoRow` sends it (src-tauri/src/mono_roster.rs:101-110). */
export type MonoRow = {
  id: string;
  name: string;
  role: "giga" | "mono";
  data: Record<string, unknown>;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
};

/** A seeded Mono. Rust derives `name` from `data` (mono_roster.rs `display_name`). */
export type MonoSeed = Pick<MonoRow, "id" | "role"> & {
  data: { name?: string; mascot: string; [key: string]: unknown };
} & Partial<Pick<MonoRow, "sortOrder" | "createdAt" | "updatedAt">>;

/** The fields Rust `SessionSummary` always sends (session_store.rs:141-178). */
export type SessionRow = SessionSummary &
  Required<
    Pick<
      SessionSummary,
      | "sidebarHidden"
      | "worktreeRemoved"
      | "additions"
      | "deletions"
      | "archived"
      | "pinned"
      | "draft"
    >
  >;

/** What `session_upsert` saves (session_store.rs SessionUpsert). */
type SessionUpsert = SessionSummary & {
  modelSettings: Record<string, string>;
  providerAccountId?: string;
  blocks: Block[];
  queuedMessages?: unknown[];
  queueStatus?: string;
  queuePause?: string;
  contextUsed?: number;
  contextWindow?: number;
  workFolder?: string;
};

/** Everything the fake backend knows, in the shapes Rust sends. */
export type FakeStore = {
  home: string;
  /** Rail projects. The app keeps these in localStorage, not behind IPC. */
  projects: string[];
  sessions: SessionRow[];
  monos: MonoRow[];
  /** mono_roster.rs `Roster.revision`: one more after each roster write. */
  rosterRevision: number;
  /**
   * Each session's saved blocks: session_store.rs `blocks_json`, or
   * mono_transcript.rs `mono_blocks` for a Mono's chat.
   */
  blocks: Record<string, Block[]>;
  /** session_store.rs `work_folder`: on the record, not the summary. */
  workFolders: Record<string, string>;
  /**
   * session_store.rs `queued_messages_json`, `queue_status` and
   * `queue_pause`: on the record, not the summary.
   */
  queues: Record<
    string,
    Pick<SessionUpsert, "queuedMessages" | "queueStatus" | "queuePause">
  >;
  automations: Automation[];
  /** milestones.rs rows of every project. */
  milestones: Milestone[];
  /** milestone_settings rows by project path; no row means on, nothing hidden. */
  milestoneSettings: Record<string, MilestoneSettings>;
  approvals: Approval[];
  /** sessionStore.ts `saveWorkspaceSnapshot`: the open tabs, as last saved. */
  workspace: unknown;
  /** session_store.rs `workspace_snapshot.updated_at`: when `workspace` was read. */
  workspaceReadAt?: number;
  /** session_store.rs `in_flight_sessions`: what the last window had running. */
  inFlight: { sessionId: string; cwd: string }[];
  /** mono_completions.rs rows by request id: the results the window owes. */
  completions: Record<string, FakeCompletion>;
  /** mono_completion_deliveries: the groups already delivered. */
  deliveredGroups: string[];
};

type FakeCompletion = {
  origin: { monoId: string };
  request: { requestId: string; sessionId: string };
  notice?: string;
};

/** mono_completions.rs settle: the save of Mono chat `chat` delivers each
 * notice its queue or transcript holds, and deletes that notice's rows. */
function settleCompletions(
  store: FakeStore,
  chat: { id: string; queuedMessages?: unknown[]; blocks: { appRequestId?: string }[] },
) {
  const held = new Set([
    ...(chat.queuedMessages ?? []).flatMap((message) =>
      message && typeof message === "object" && "id" in message ? [message.id] : [],
    ),
    ...chat.blocks.map((block) => block.appRequestId),
  ]);
  for (const row of Object.values(store.completions)) {
    if (row.origin.monoId !== chat.id || !row.notice || !held.has(row.notice)) continue;
    if (!store.deliveredGroups.includes(row.notice)) store.deliveredGroups.push(row.notice);
    delete store.completions[row.request.requestId];
  }
}

/** What a test seeds. `installBackend` fills in what Rust would add. */
export type FakeSeed = Partial<{
  home: string;
  projects: string[];
  sessions: SessionSummary[];
  monos: MonoSeed[];
  automations: Automation[];
  milestones: Milestone[];
  milestoneSettings: Record<string, MilestoneSettings>;
  approvals: Approval[];
  /** Saved state a real database already holds when the app starts. */
  blocks: Record<string, Block[]>;
  workFolders: Record<string, string>;
  workspace: unknown;
  inFlight: { sessionId: string; cwd: string }[];
}>;

/**
 * Answers one command. It may be async. Throw a string to reject as a Rust
 * `Err(String)` does; anything else thrown is a harness error.
 */
export type Handler = (
  store: FakeStore,
  args: Record<string, unknown>,
) => unknown;

/** A harness problem that the fixture fails the test with. */
export type BackendReport = { command: string; problem: string };

/**
 * A harness `cannotSteer` can turn into one that refuses steers. Not codex:
 * `ensureCodexRegistered` registers its own adapter again whenever the
 * registry holds another object, so the app would undo the copy.
 */
export type SteerlessHarness = Exclude<HarnessId, "codex">;

/** What a test can reach on `window.appBackend` through `page.evaluate`. */
export type AppBackend = {
  store: FakeStore;
  /** Every command the app sent, in call order, with any non-null answer or Rust-style refusal. */
  calls: {
    command: string;
    args: unknown;
    reply?: unknown;
    refused?: string;
  }[];
  emit: (event: string, payload?: unknown) => Promise<void>;
  /** Hands the store to the next page load, as Rust's database outlives a reload. */
  keepStore: () => void;
};

declare global {
  interface Window {
    appBackendSeed?: FakeSeed;
    appBackendHandlers?: Record<string, Handler>;
    appBackendCannotSteer?: SteerlessHarness[];
    /** Exposed by the fixture, so reports outlive a reload. */
    appBackendReport: (report: BackendReport) => Promise<void>;
    appBackend?: AppBackend;
  }
}

const none: Handler = () => null;

/** milestones.rs `Status` in declaration order, which its derived `Ord` sorts by. */
const RUST_STATUS_ORDER: readonly MilestoneStatus[] = [
  "idea",
  "planned",
  "in_progress",
  "done",
];

/** milestones.rs settings: no row means on, nothing hidden. */
function settingsOf(store: FakeStore, project: string): MilestoneSettings {
  return (
    store.milestoneSettings[project] ?? { enabled: true, hiddenStatuses: [] }
  );
}
const empty: Handler = () => [];
const notInstalled: Handler = () => {
  throw "not found on PATH";
};

/** Fire-and-forget commands whose answer the app ignores. */
const IGNORED = [
  "automation_runs_recover",
  "autosave_set_enabled",
  "control_loaded_sessions",
  "control_window_booted",
  "harness_kill",
  "harness_kill_all",
  "keybindings_set_overrides",
  "plugin:webview|set_webview_zoom",
  "plugin:window|set_title",
  "quick_composer_set_enabled",
  "quit_ready",
  "reminder_configure",
  "reminder_register_window",
  "session_checkpoint_begin_turn",
  "session_checkpoint_finish_turn",
  "session_set_in_flight",
  "set_dock_badge",
  "set_traffic_lights_visible",
  "set_window_background_blur",
];

const HARNESSES = [
  "antigravity",
  "claude",
  "codex",
  "cursor",
  "devin",
  "fx",
  "grok",
  "hermes",
  "omp",
  "opencode",
  "pi",
];

/** Every project is a git repo on this branch. */
const BRANCH = "main";

/** mono.rs content_hash: the SHA-256 of a file's text, in hex. */
async function contentHash(text: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(text),
  );
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

/** session_store.rs get_session: the record and its saved blocks. */
function sessionRecord(store: FakeStore, sessionId: unknown) {
  const row = store.sessions.find((session) => session.id === sessionId);
  if (!row) return null;
  const {
    additions: _additions,
    deletions: _deletions,
    archived: _archived,
    pinned: _pinned,
    draft: _draft,
    repo: _repo,
    orchestration: _orchestration,
    ...record
  } = row;
  const workFolder = store.workFolders[row.id];
  return {
    ...record,
    ...(workFolder ? { workFolder } : {}),
    modelSettings: {},
    blocks: store.blocks[row.id] ?? [],
    queuedMessages: [],
    ...store.queues[row.id],
  };
}

/**
 * session_store.rs upsert_session: stores the summary, the blocks and the
 * work folder it is given, and answers with the summary. The row keeps its
 * createdAt, archived and pinned. Like the list, the answer's `branch` is the
 * repo's, unless the worktree is gone.
 */
const upsertSession: Handler = (store, { session }) => {
  const {
    modelSettings: _modelSettings,
    providerAccountId: _providerAccountId,
    blocks,
    queuedMessages,
    queueStatus,
    queuePause,
    contextUsed: _contextUsed,
    contextWindow: _contextWindow,
    workFolder,
    ...summary
  } = session as SessionUpsert;
  const kept = store.sessions.find((row) => row.id === summary.id);
  store.blocks[summary.id] = blocks;
  store.queues[summary.id] = { queuedMessages, queueStatus, queuePause };
  if (workFolder) store.workFolders[summary.id] = workFolder;
  else delete store.workFolders[summary.id];
  const row = sessionRow({
    ...summary,
    createdAt: kept?.createdAt ?? Date.now(),
    updatedAt: Date.now(),
    archived: kept?.archived ?? false,
    pinned: kept?.pinned ?? false,
    draft: blocks.some((block) => block.role === "user" && block.draft),
  });
  store.sessions = [
    ...store.sessions.filter((other) => other.id !== row.id),
    row,
  ];
  const { branch: _branch, ...answer } = row;
  return row.worktreeRemoved ? answer : { ...answer, branch: BRANCH };
};

// milestones.rs check_title, check_note and check_target, with their refusals.
function checkMilestone(
  fields: Pick<Milestone, "title" | "note" | "status" | "target">,
) {
  const title = fields.title.trim();
  if ([...title].length < 1 || [...title].length > 200)
    throw "invalid_input: title must be 1 to 200 characters";
  if ([...fields.note].length > 4000)
    throw "invalid_input: note must be 4000 characters or fewer";
  const day = /^\d{4}-\d{2}-\d{2}$/.test(fields.target ?? "")
    ? new Date(`${fields.target}T00:00:00Z`)
    : null;
  if (
    fields.target !== null &&
    (!day ||
      Number.isNaN(day.getTime()) ||
      day.toISOString().slice(0, 10) !== fields.target)
  )
    throw "invalid_input: target must be a real YYYY-MM-DD date";
  return {
    title,
    note: fields.note,
    status: fields.status,
    target: fields.target,
  };
}

const HANDLERS: Record<string, Handler> = {
  ...Object.fromEntries(IGNORED.map((command) => [command, none])),
  ...Object.fromEntries(
    HARNESSES.map((id) => [`harness_resolve_${id}`, notInstalled]),
  ),
  home_dir: (store) => store.home,
  default_cwd: (store) => store.home,
  // The page is one window, so its own row is the only one.
  workspace_get_snapshot: (store) => (store.workspace ? [store.workspace] : []),
  // session_store.rs snapshot_supersedes: a snapshot read before the stored
  // one loses, unless the stored time is after now (the clock stepped back).
  workspace_set_snapshot: (store, { snapshot, readAt }) => {
    const stored = store.workspaceReadAt;
    const incoming = readAt as number;
    if (stored === undefined || incoming >= stored || stored > Date.now()) {
      store.workspace = snapshot;
      store.workspaceReadAt = incoming;
    }
    return null;
  },
  session_list_in_flight: (store) => store.inFlight,
  // mono_completions.rs. One window, so it holds every row it claims. The
  // claim first delivers what each Mono chat's save holds, and drops the rows
  // of a Mono that has no chat.
  mono_completions_claim: (store) => {
    const chats = new Set(store.sessions.map((session) => session.id));
    for (const id of chats)
      settleCompletions(store, {
        id,
        queuedMessages: store.queues[id]?.queuedMessages,
        blocks: store.blocks[id] ?? [],
      });
    for (const row of Object.values(store.completions))
      if (!chats.has(row.origin.monoId))
        delete store.completions[row.request.requestId];
    return Object.values(store.completions);
  },
  // A row in a notice stays until a Mono chat save holds that notice.
  mono_completions_hold: (store, { saved }) => {
    store.completions = Object.fromEntries(
      [
        ...Object.values(store.completions).filter((row) => row.notice),
        ...(saved as FakeCompletion[]),
      ].map((row) => [row.request.requestId, row]),
    );
    return null;
  },
  mono_completions_dismiss: (store, { monoId, sessionId }) => {
    for (const row of Object.values(store.completions))
      if (row.origin.monoId === monoId && row.request.sessionId === sessionId)
        delete store.completions[row.request.requestId];
    return null;
  },
  // session_store.rs list_by_project: ORDER BY updated_at DESC, id ASC, and
  // `branch` is the stored one, else the repo's, unless the worktree is gone.
  session_list_by_project: (store, { cwd }) =>
    store.sessions
      .filter((session) => session.cwd === cwd)
      .sort(
        (a, b) =>
          b.updatedAt - a.updatedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
      )
      .map(({ branch, ...session }) =>
        session.worktreeRemoved
          ? session
          : { ...session, branch: branch || BRANCH },
      ),
  session_get: (store, { sessionId }) => sessionRecord(store, sessionId),
  session_upsert: upsertSession,
  // checkpoint.rs status: no turn recorded a change.
  session_checkpoint_status: () => ({ files: [] }),
  monos_list: (store) => ({
    revision: store.rosterRevision,
    monos: store.monos,
  }),
  // mono_roster.rs update_many: a JSON merge patch on `data`, then the name.
  monos_update: (store, { updates }) => {
    for (const update of updates as MonoUpdate[]) {
      const mono = store.monos.find((row) => row.id === update.id);
      if (!mono) throw `Unknown Mono: ${update.id}`;
      if (update.patch) mono.data = mergePatch(mono.data, update.patch);
      mono.name =
        customMonoName(mono.data.name as string | undefined) ??
        defaultMonoName(mono.data.mascot as string);
      if (update.role) mono.role = update.role;
      if (update.sortOrder !== undefined) mono.sortOrder = update.sortOrder;
    }
    if ((updates as MonoUpdate[]).length) store.rosterRevision += 1;
    // decide.rs requeue (#125 r6): a waiting decision goes to the Giga's new
    // chat, and the board hears approvals-changed once.
    let requeued = false;
    for (const card of store.approvals) {
      const giga = store.monos.find((mono) => mono.id === card.requestedBy.id);
      if (
        card.status.state === "decided" &&
        card.status.undelivered === "waiting" &&
        giga?.data.sessionId
      ) {
        card.status = { ...card.status, undelivered: null };
        requeued = true;
      }
    }
    if (requeued) void window.appBackend!.emit("approvals-changed", null);
    return { revision: store.rosterRevision, monos: store.monos };
  },
  // The level rules and start rows live in Rust (appActionCheck.fixtures.json
  // pins them); every gated call the specs make is allowed, starts no worker
  // and carries no start ask, and a start's row is one fixed id.
  mono_app_action_check: () => ({
    outcome: "allowed",
    leadSessionId: null,
    ask: null,
  }),
  mono_message_record_start: () => ({
    outcome: "recorded",
    messageId: "m_aaaaaaaaaaaa",
  }),
  // mono_transcript.rs mono_session_get: the record and its only page.
  mono_session_get: (store, { sessionId }) => {
    const record = sessionRecord(store, sessionId);
    return (
      record && {
        ...record,
        monoTranscript: {
          before: null,
          firstBlockId: record.blocks[0]?.id ?? null,
        },
      }
    );
  },
  // mono_transcript.rs upsert_with_mode: the app sends only the blocks after
  // an anchor; they replace what was saved after it.
  mono_session_upsert: (store, args) => {
    const { session, afterBlockId, fromBlockId, blocksChanged, appendOnly } =
      args as MonoUpsert;
    const saved = store.blocks[session.id] ?? [];
    let kept = saved;
    if (blocksChanged && !appendOnly) {
      const anchor = afterBlockId ?? fromBlockId;
      const at = anchor ? saved.findIndex((block) => block.id === anchor) : 0;
      if (at < 0) throw "Query returned no rows";
      kept = saved.slice(0, afterBlockId ? at + 1 : at);
    }
    const blocks = blocksChanged ? [...kept, ...session.blocks] : saved;
    const summary = upsertSession(store, { session: { ...session, blocks } });
    settleCompletions(store, { ...session, blocks });
    return summary;
  },
  // mono.rs load: SOUL.md as the app seeds it, an empty memory, no topics.
  mono_load: async (store, { mono }) => {
    const dir = `${store.home}/Library/Application Support/com.monocode.desktop/monos/${mono}`;
    const soul = defaultSoul();
    return {
      id: mono,
      dir,
      soul,
      soulHash: await contentHash(soul),
      memory: "",
      memoryHash: await contentHash(""),
      memoryPath: `${dir}/MEMORY.md`,
      topics: [],
    };
  },
  // mono.rs read: a file the Mono has not written.
  mono_read: async () => ({ text: null, hash: await contentHash("") }),
  automations_list: (store) => store.automations,
  milestones_list: (store, { projectPath }) => ({
    settings: settingsOf(store, projectPath as string),
    milestones: store.milestones
      .filter((milestone) => milestone.projectPath === projectPath)
      .sort((a, b) => a.order - b.order || a.createdAt - b.createdAt),
  }),
  // milestones.rs update for the `user` writer (never refused when off), with
  // its checks and refusals; it emits CHANGED as Rust does after a write.
  milestones_update: (store, { projectPath, id, patch }) => {
    const row = store.milestones.find(
      (milestone) =>
        milestone.id === id && milestone.projectPath === projectPath,
    );
    if (!row) throw "unknown_milestone";
    Object.assign(
      row,
      checkMilestone({ ...row, ...(patch as MilestonePatch) }),
      {
        updatedAt: Date.now(),
      },
    );
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath });
    return row;
  },
  // milestones.rs reorder: `ids` must be exactly the project's ids, else the
  // bare code stale_order; then sort_order follows `ids`. Emits CHANGED.
  milestones_reorder: (store, { projectPath, ids }) => {
    const mine = store.milestones.filter(
      (milestone) => milestone.projectPath === projectPath,
    );
    const wanted = [...(ids as string[])].sort().join();
    if (
      wanted !==
      mine
        .map((milestone) => milestone.id)
        .sort()
        .join()
    )
      throw "stale_order";
    (ids as string[]).forEach((id, order) => {
      mine.find((milestone) => milestone.id === id)!.order = order;
    });
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath });
    return [...mine].sort((a, b) => a.order - b.order);
  },
  // milestones.rs remove for the `user` writer; emits CHANGED as Rust does.
  milestones_remove: (store, { projectPath, id }) => {
    const at = store.milestones.findIndex(
      (milestone) =>
        milestone.id === id && milestone.projectPath === projectPath,
    );
    if (at < 0) throw "unknown_milestone";
    store.milestones.splice(at, 1);
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath });
    return null;
  },
  // milestones.rs set_settings: a partial patch; hidden statuses without
  // duplicates, in the order of Rust's `Status` enum. Emits CHANGED to every window.
  milestone_settings_set: (store, { projectPath, patch }) => {
    const current = settingsOf(store, projectPath as string);
    const next = { ...current, ...(patch as Partial<MilestoneSettings>) };
    next.hiddenStatuses = [...new Set(next.hiddenStatuses)].sort(
      (a, b) => RUST_STATUS_ORDER.indexOf(a) - RUST_STATUS_ORDER.indexOf(b),
    );
    store.milestoneSettings[projectPath as string] = next;
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath });
    return next;
  },
  // session_store.rs rebase_project: sessions, milestones and settings move to
  // the new path; CHANGED for both paths.
  session_rebase_project: (store, { fromCwd, toCwd }) => {
    const [from, to] = [fromCwd as string, toCwd as string];
    for (const row of [...store.sessions, ...store.milestones]) {
      if ("cwd" in row && row.cwd === from) row.cwd = to;
      if ("projectPath" in row && row.projectPath === from)
        row.projectPath = to;
    }
    if (store.milestoneSettings[from]) {
      store.milestoneSettings[to] = store.milestoneSettings[from];
      delete store.milestoneSettings[from];
    }
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath: from });
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath: to });
    return null;
  },
  // milestones.rs add: defaults, the same checks, the next order in the project.
  // An agent (not `user`) is refused with milestones_off while the project is off.
  milestones_add: (store, { projectPath, milestone }) => {
    const input = milestone as NewMilestone & {
      createdBy: Milestone["createdBy"];
    };
    if (
      input.createdBy !== "user" &&
      !settingsOf(store, projectPath as string).enabled
    )
      throw "milestones_off";
    const mine = store.milestones.filter(
      (row) => row.projectPath === projectPath,
    );
    const row: Milestone = {
      ...checkMilestone({ note: "", status: "idea", target: null, ...input }),
      id: `00000000-0000-4000-8000-${String(store.milestones.reduce((n, m) => Math.max(n, Number(m.id.slice(24)) || 0), 0) + 1).padStart(12, "0")}`,
      projectPath: projectPath as string,
      order: Math.max(-1, ...mine.map((other) => other.order)) + 1,
      createdBy: input.createdBy,
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };
    store.milestones.push(row);
    void window.appBackend!.emit(MILESTONES_CHANGED, { projectPath });
    return row;
  },
  approvals_open_counts: (store) => {
    const byProject: Record<string, number> = {};
    for (const { projectPath, status } of store.approvals)
      if (status.state === "open")
        byProject[projectPath] = (byProject[projectPath] ?? 0) + 1;
    return {
      total: Object.values(byProject).reduce((sum, open) => sum + open, 0),
      byProject,
    };
  },
  // approvals.rs `list`: ORDER BY status <> 'open', open and not blocking,
  // coalesce(decided_at, withdrawn_at, created_at) DESC, number DESC.
  approvals_list: (store, { projectPath }) => {
    const rank = ({ status, urgency, createdAt, number }: Approval) => [
      Number(status.state !== "open"),
      Number(status.state === "open" && urgency !== "blocking"),
      -(status.state === "decided"
        ? status.decidedAt
        : status.state === "withdrawn"
          ? status.withdrawnAt
          : createdAt),
      -number,
    ];
    return store.approvals
      .filter((card) => card.projectPath === projectPath)
      .sort((a, b) => {
        const [x, y] = [rank(a), rank(b)];
        return x.map((key, index) => key - y[index]).find(Boolean) ?? 0;
      });
  },
  // decide.rs `decide` (ADE-52): its refusals, then the CHANGED event.
  approvals_decide: (store, { id, verdict, note, chosen }) => {
    const card = store.approvals.find((row) => row.id === id);
    if (!card) throw "unknown_approval";
    if (card.status.state !== "open") throw "approval_not_open";
    const labels = card.options.map((option) => option.label);
    const picked = chosen as string[];
    if (
      new Set(picked).size !== picked.length ||
      picked.some((label) => !labels.includes(label))
    )
      throw "invalid_input: chosen options must be distinct labels of the card's options";
    if (verdict === "approved" && labels.length > 0 && picked.length === 0)
      throw "invalid_input: approving a card with options needs at least one chosen option";
    const text = note === null ? null : String(note).trim();
    if (text === "") throw "invalid_input: decision note must not be blank";
    // decide.rs decision_text: the message the Giga gets, counted in characters.
    const body = `The user decided approval card APR-${card.number} "${card.heading}".\ncard_id: ${card.id}\ndecision: ${String(verdict)}\nchosen_options: ${JSON.stringify(picked)}${text === null ? "" : `\ncomment: ${text}`}`;
    const chars = [...body].length;
    if (chars > 8000) throw `decision_too_long:${chars}:8000`;
    const filer = card.requestedBy.id;
    if (filer && !store.monos.find((mono) => mono.id === filer)?.data.sessionId)
      throw "filer_has_no_chat";
    card.status = {
      state: "decided",
      verdict: verdict as Verdict,
      decidedAt: Date.now(),
      note: text,
      chosen: picked,
      undelivered: null,
    };
    void window.appBackend!.emit("approvals-changed", {
      projectPath: card.projectPath,
    });
    return card;
  },
  automation_runs_list: empty,
  control_load: none,
  control_owns_session: () => false,
  harness_runtime_binary_paths: () => ({}),
  harness_update_check_claim: () => false,
  list_external_editors: empty,
  // control.rs: the app's own executable.
  app_cli_path: () => "/Applications/MonoCode.app/Contents/MacOS/monocode",
  // account_identity.rs and rate_limits.rs while the CLI is not signed in.
  provider_account_identity: none,
  fetch_claude_usage: () => ({
    status: "unavailable",
    httpStatus: null,
    body: null,
    error: "Claude not signed in",
  }),
  // pi_usage.rs while Pi is not signed in.
  fetch_pi_usage: () => ({
    status: "unavailable",
    message: "Pi not signed in",
  }),
  fetch_opencode_go_usage: () => ({
    status: "unavailable",
    httpStatus: null,
    body: null,
    error: "OpenCode Go not connected",
  }),
  list_skills: empty,
  reminder_list: empty,
  reminder_take_open: none,
  quick_composer_take: none,
  github_monocode_star_status: () => "unavailable",
  giga_workspace_ensure: (store) => ({
    root: `${store.home}/MonoCode`,
    state: "ready",
    created: [],
  }),
  // giga_workspace.rs: no snapshot, no files changed, nothing to commit.
  giga_workspace_bind: none,
  giga_workspace_turn_start: none,
  giga_workspace_turn_files: empty,
  giga_workspace_turn_end: () => ({}),
  // fs.rs git_branches_for: a repo on `main` lists `main` as current.
  git_branches: () => ({
    current: BRANCH,
    detached: false,
    branches: [{ name: BRANCH, current: true, remote: null }],
  }),
  git_diff_stats: () => ({ files: 0, additions: 0, deletions: 0 }),
  git_github_repositories: empty,
  // fs.rs git_github_*: `gh` installed and signed in, with nothing to list.
  git_github_status: () => ({
    connected: true,
    installed: true,
    authenticated: true,
  }),
  git_github_work_items: empty,
  // fs.rs git_github_repo: a project with no GitHub remote.
  git_github_repo: () => {
    throw "No GitHub remote";
  },
  git_github_work_item_details: () => ({
    body: "",
    author: "",
    authorAvatarUrl: "",
    baseRefName: "",
    headRefName: "",
    reviewDecision: "",
  }),
  git_github_work_item_thread: () => ({
    comments: [],
    commits: [],
    truncated: false,
    reviewDecision: "",
    baseRefName: "",
    headRefName: "",
  }),
  git_github_pr_checks: () => ({ headOid: "", checks: [] }),
  // session_store.rs list_linked: threads with a linked work item, no Asks.
  session_list_linked: (store) =>
    store.sessions.filter((session) => session.linkedWorkItem),
  // worktrees.rs git_worktrees: the main worktree first, default_root beside it.
  git_worktrees: (store, { cwd }) => ({
    worktrees: [
      {
        path: cwd,
        branch: BRANCH,
        head: "0000000000000000000000000000000000000000",
        isMain: true,
        locked: false,
        prunable: false,
        missing: false,
        dirty: false,
        unpushed: 0,
        // worktrees.rs session_ids: live sessions whose worktree_cwd, else
        // cwd, is inside the worktree.
        sessionIds: store.sessions
          .filter((session) => {
            const path = session.worktreeCwd || session.cwd;
            return (
              !session.worktreeRemoved &&
              (path === cwd || path.startsWith(`${cwd}/`))
            );
          })
          .map((session) => session.id),
      },
    ],
    defaultRoot: `${cwd}-worktrees`,
  }),
  linear_status: () => ({ connected: false }),
  jira_status: () => ({ connected: false, site: "", email: "" }),
  gitlab_status: () => ({ connected: false, url: "" }),
  azure_devops_status: () => ({ connected: false, url: "", organization: "" }),
  list_project_files: empty,
  notes_list: empty,
  resolve_project_location: (_, { path }) => ({ path, identity: path }),
  quick_composer_prepare: () => false,
  "plugin:app|version": () => "0.0.0-test",
  "plugin:app|identifier": () => "com.monocode.desktop",
  "plugin:app|bundle_type": none,
  "plugin:updater|check": none,
  "plugin:window|is_maximized": () => false,
  "plugin:window|is_fullscreen": () => false,
  // Upstream's phone pairing (Settings > Mobile), off on this fake backend.
  mobile_status: () => ({
    enabled: false,
    listening: false,
    name: "",
    fingerprint: null,
    openRouter: false,
    remoteAddress: "",
    routerStatus: "",
    addresses: [],
    devices: [],
  }),
  // No CPU or memory reading, so the resource pressure notice stays hidden.
  resource_usage: () => ({ cpuPercent: null, memoryPressure: null }),
};

/** mono_roster.rs `MonoUpdate`. */
type MonoUpdate = {
  id: string;
  role?: MonoRow["role"];
  patch?: Record<string, unknown>;
  sortOrder?: number;
};

/** The arguments of mono_transcript.rs `mono_session_upsert`. */
type MonoUpsert = {
  session: SessionUpsert;
  afterBlockId: string | null;
  fromBlockId: string | null;
  blocksChanged: boolean;
  appendOnly: boolean;
};

/** RFC 7396, as mono_roster.rs `merge_patch`: null removes a key. */
function mergePatch<T extends Record<string, unknown>>(
  target: T,
  patch: Record<string, unknown>,
): T {
  const merged: Record<string, unknown> = { ...target };
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete merged[key];
    else if (typeof value === "object" && !Array.isArray(value))
      merged[key] = mergePatch(
        (merged[key] ?? {}) as Record<string, unknown>,
        value as Record<string, unknown>,
      );
    else merged[key] = value;
  }
  return merged as T;
}

function monoRow(seed: MonoSeed, index: number): MonoRow {
  return {
    name: customMonoName(seed.data.name) ?? defaultMonoName(seed.data.mascot),
    sortOrder: index,
    createdAt: 1,
    updatedAt: 1,
    ...seed,
  };
}

function sessionRow(seed: SessionSummary): SessionRow {
  return {
    sidebarHidden: false,
    worktreeRemoved: false,
    additions: 0,
    deletions: 0,
    archived: false,
    pinned: false,
    draft: false,
    ...seed,
  };
}

function storeFrom(seed: FakeSeed): FakeStore {
  return {
    home: seed.home ?? "/Users/test",
    projects: seed.projects ?? [],
    sessions: (seed.sessions ?? []).map(sessionRow),
    monos: (seed.monos ?? []).map(monoRow),
    rosterRevision: 1,
    blocks: seed.blocks ?? {},
    workFolders: seed.workFolders ?? {},
    queues: {},
    automations: seed.automations ?? [],
    milestones: seed.milestones ?? [],
    milestoneSettings: seed.milestoneSettings ?? {},
    approvals: seed.approvals ?? [],
    workspace: seed.workspace ?? null,
    inFlight: seed.inFlight ?? [],
    completions: {},
    deliveredGroups: [],
  };
}

/** Where `keepStore` hands the store to the next page load. */
const KEPT_STORE = "appBackendStore";

/**
 * Replaces the Tauri backend with an in-memory store. A command without a
 * handler, or a handler that fails, is reported to the fixture and rejects, so
 * the test fails instead of the app silently showing an empty state.
 */
export function installBackend(
  seed: FakeSeed = {},
  overrides: Record<string, Handler> = {},
) {
  const kept = sessionStorage.getItem(KEPT_STORE);
  sessionStorage.removeItem(KEPT_STORE);
  const store: FakeStore = kept ? JSON.parse(kept) : storeFrom(seed);
  const handlers = { ...HANDLERS, ...overrides };
  localStorage.setItem(
    "monocode.recentProjects",
    JSON.stringify(store.projects.map((path) => ({ path, openedAt: 1 }))),
  );
  const backend: AppBackend = {
    store,
    calls: [],
    emit,
    keepStore: () => sessionStorage.setItem(KEPT_STORE, JSON.stringify(store)),
  };
  mockWindows("main");
  // Project logos and image previews load through the asset protocol.
  mockConvertFileSrc("macos");
  mockIPC(
    async (command, args) => {
      const call: AppBackend["calls"][number] = { command, args };
      backend.calls.push(call);
      const handler = handlers[command];
      if (!handler) {
        void window.appBackendReport({ command, problem: "no handler" });
        throw `Unmocked Tauri command: ${command}`;
      }
      try {
        // Real IPC sends JSON, so the app never holds a reference into the store.
        const reply = structuredClone(
          await handler(store, (args ?? {}) as Record<string, unknown>),
        );
        if (reply != null) call.reply = structuredClone(reply);
        return reply;
      } catch (error) {
        if (typeof error === "string") call.refused = error;
        else void window.appBackendReport({ command, problem: String(error) });
        throw error;
      }
    },
    { shouldMockEvents: true },
  );
  window.appBackend = backend;
}
