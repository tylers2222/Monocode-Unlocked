import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Automation } from "../../../src/features/automations/model/automations";
import type { SessionSummary } from "../../../src/features/sessions/data/sessionStore";

/** One Mono row as `monos_list` returns it from `monocode.db`. */
export type MonoRow = {
  id: string;
  role: "giga" | "mono";
  data: Record<string, unknown>;
};

/** Everything the fake backend knows. A test seeds it; commands read it. */
export type FakeStore = {
  home: string;
  /** Rail projects. The app keeps these in localStorage, not behind IPC. */
  projects: string[];
  sessions: SessionSummary[];
  monos: MonoRow[];
  automations: Automation[];
};

/** What a test can reach on `window.appBackend` through `page.evaluate`. */
export type AppBackend = {
  store: FakeStore;
  /** Every command the app sent, in call order. */
  calls: { command: string; args: unknown }[];
  /** Commands the app sent that no handler answers, in call order. */
  unmocked: string[];
  emit: (event: string, payload?: unknown) => Promise<void>;
};

declare global {
  interface Window {
    appBackendSeed?: Partial<FakeStore>;
    appBackend?: AppBackend;
  }
}

type Handler = (store: FakeStore, args: Record<string, unknown>) => unknown;

const none: Handler = () => null;
const empty: Handler = () => [];
const notInstalled: Handler = () => {
  throw new Error("not found on PATH");
};

/** Fire-and-forget commands whose answer the app ignores. */
const IGNORED = [
  "automation_runs_recover",
  "autosave_set_enabled",
  "control_loaded_sessions",
  "control_window_booted",
  "harness_kill",
  "keybindings_set_overrides",
  "plugin:webview|set_webview_zoom",
  "plugin:window|set_title",
  "quick_composer_set_enabled",
  "reminder_configure",
  "reminder_register_window",
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
  session_list_by_project: (store, { cwd }) =>
    store.sessions.filter((session) => session.cwd === cwd),
  monos_list: (store) => ({ revision: 1, monos: store.monos }),
  automations_list: (store) => store.automations,
  automation_runs_list: empty,
  control_load: none,
  control_owns_session: () => false,
  harness_runtime_binary_paths: () => ({}),
  harness_update_check_claim: () => false,
  list_external_editors: empty,
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
  git_branches: () => ({ current: "main", detached: false, branches: [] }),
  git_diff_stats: () => ({ files: 0, additions: 0, deletions: 0 }),
  git_github_repositories: empty,
  git_worktrees: (_, { cwd }) => ({ worktrees: [], defaultRoot: cwd }),
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

const DEFAULT_STORE: FakeStore = {
  home: "/Users/test",
  projects: [],
  sessions: [],
  monos: [],
  automations: [],
};

/**
 * Replaces the Tauri backend with an in-memory store. A command without a
 * handler is recorded in `unmocked` and rejects, so the spec fixture can fail
 * the test instead of the app silently showing an empty state.
 */
export function installBackend(seed: Partial<FakeStore> = {}): AppBackend {
  const store: FakeStore = { ...DEFAULT_STORE, ...seed };
  localStorage.setItem(
    "monocode.recentProjects",
    JSON.stringify(store.projects.map((path) => ({ path, openedAt: 1 }))),
  );
  const backend: AppBackend = { store, calls: [], unmocked: [], emit };
  mockWindows("main");
  mockIPC(
    (command, args) => {
      backend.calls.push({ command, args });
      const handler = HANDLERS[command];
      // Real IPC sends JSON, so the app never holds a reference into the store.
      if (handler)
        return structuredClone(
          handler(store, (args ?? {}) as Record<string, unknown>),
        );
      backend.unmocked.push(command);
      throw new Error(`Unmocked Tauri command: ${command}`);
    },
    { shouldMockEvents: true },
  );
  window.appBackend = backend;
  return backend;
}
