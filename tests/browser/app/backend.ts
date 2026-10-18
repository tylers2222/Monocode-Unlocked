import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Automation } from "../../../src/features/automations/model/automations";
import {
  customMonoName,
  defaultMonoName,
} from "../../../src/features/monos/model/mono";
import { defaultSoul } from "../../../src/features/monos/model/monoFiles";
import type { SessionSummary } from "../../../src/features/sessions/data/sessionStore";

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
  blocks: { role: string; draft?: boolean }[];
  queuedMessages?: unknown[];
  queueStatus?: string;
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
  automations: Automation[];
};

/** What a test seeds. `installBackend` fills in what Rust would add. */
export type FakeSeed = Partial<{
  home: string;
  projects: string[];
  sessions: SessionSummary[];
  monos: MonoSeed[];
  automations: Automation[];
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

/** What a test can reach on `window.appBackend` through `page.evaluate`. */
export type AppBackend = {
  store: FakeStore;
  /** Every command the app sent, in call order. */
  calls: { command: string; args: unknown }[];
  emit: (event: string, payload?: unknown) => Promise<void>;
};

declare global {
  interface Window {
    appBackendSeed?: FakeSeed;
    appBackendHandlers?: Record<string, Handler>;
    /** Exposed by the fixture, so reports outlive a reload. */
    appBackendReport: (report: BackendReport) => Promise<void>;
    appBackend?: AppBackend;
  }
}

const none: Handler = () => null;
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
  "reminder_configure",
  "reminder_register_window",
  "session_checkpoint_begin_turn",
  "session_checkpoint_finish_turn",
  "session_set_in_flight",
  "set_dock_badge",
  "set_traffic_lights_visible",
  "set_window_background_blur",
  "workspace_set_snapshot",
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

/**
 * session_store.rs get_session. The fake store keeps summaries only, so a
 * record holds no messages.
 */
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
  return { ...record, modelSettings: {}, blocks: [], queuedMessages: [] };
}

/**
 * session_store.rs upsert_session: stores the summary and answers with it.
 * The row keeps its createdAt, archived and pinned. Like the list, the
 * answer's `branch` is the repo's, unless the worktree is gone.
 */
const upsertSession: Handler = (store, { session }) => {
  const {
    modelSettings: _modelSettings,
    providerAccountId: _providerAccountId,
    blocks,
    queuedMessages: _queuedMessages,
    queueStatus: _queueStatus,
    contextUsed: _contextUsed,
    contextWindow: _contextWindow,
    workFolder: _workFolder,
    ...summary
  } = session as SessionUpsert;
  const kept = store.sessions.find((row) => row.id === summary.id);
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

const HANDLERS: Record<string, Handler> = {
  ...Object.fromEntries(IGNORED.map((command) => [command, none])),
  ...Object.fromEntries(
    HARNESSES.map((id) => [`harness_resolve_${id}`, notInstalled]),
  ),
  home_dir: (store) => store.home,
  default_cwd: (store) => store.home,
  take_window_transfer: none,
  workspace_get_snapshot: none,
  session_list_in_flight: empty,
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
  monos_list: (store) => ({ revision: 1, monos: store.monos }),
  // mono_transcript.rs mono_session_get: the record and its only page.
  mono_session_get: (store, { sessionId }) => {
    const record = sessionRecord(store, sessionId);
    return (
      record && {
        ...record,
        monoTranscript: { before: null, firstBlockId: null },
      }
    );
  },
  mono_session_upsert: upsertSession,
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
  // fs.rs git_branches_for: a repo on `main` lists `main` as current.
  git_branches: () => ({
    current: BRANCH,
    detached: false,
    branches: [{ name: BRANCH, current: true, remote: null }],
  }),
  git_diff_stats: () => ({ files: 0, additions: 0, deletions: 0 }),
  git_github_repositories: empty,
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
  "plugin:app|bundle_type": none,
  "plugin:updater|check": none,
  "plugin:window|is_maximized": () => false,
};

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
    automations: seed.automations ?? [],
  };
}

/**
 * Replaces the Tauri backend with an in-memory store. A command without a
 * handler, or a handler that fails, is reported to the fixture and rejects, so
 * the test fails instead of the app silently showing an empty state.
 */
export function installBackend(
  seed: FakeSeed = {},
  overrides: Record<string, Handler> = {},
) {
  const store = storeFrom(seed);
  const handlers = { ...HANDLERS, ...overrides };
  localStorage.setItem(
    "monocode.recentProjects",
    JSON.stringify(store.projects.map((path) => ({ path, openedAt: 1 }))),
  );
  const backend: AppBackend = { store, calls: [], emit };
  mockWindows("main");
  mockIPC(
    async (command, args) => {
      backend.calls.push({ command, args });
      const handler = handlers[command];
      if (!handler) {
        void window.appBackendReport({ command, problem: "no handler" });
        throw `Unmocked Tauri command: ${command}`;
      }
      try {
        // Real IPC sends JSON, so the app never holds a reference into the store.
        return structuredClone(
          await handler(store, (args ?? {}) as Record<string, unknown>),
        );
      } catch (error) {
        if (typeof error !== "string")
          void window.appBackendReport({ command, problem: String(error) });
        throw error;
      }
    },
    { shouldMockEvents: true },
  );
  window.appBackend = backend;
}
