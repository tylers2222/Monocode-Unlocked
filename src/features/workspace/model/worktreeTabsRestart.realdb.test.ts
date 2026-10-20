import { beforeEach, expect, it, vi } from "vitest";
import realDb from "./fixtures/worktreeTabsRestart.realdb.json";

type Row = Record<string, unknown>;
const store = vi.hoisted(() => ({
  snapshot: null as unknown,
  inFlight: [] as { sessionId: string; cwd: string }[],
  records: {} as Record<string, Row>,
}));

// Only the IPC boundary is replaced, by an in-memory copy of the fixture that
// answers as session_store.rs does. Session upserts are accepted and not
// applied: they do not change which tabs are saved or where they belong.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args?: Row) => {
    switch (command) {
      case "workspace_get_snapshot":
        return store.snapshot;
      case "workspace_set_snapshot":
        store.snapshot = JSON.parse(JSON.stringify(args?.snapshot));
        return null;
      case "session_list_in_flight":
        return store.inFlight;
      case "session_set_in_flight":
        store.inFlight = args?.sessions as typeof store.inFlight;
        return null;
      case "session_get":
        return store.records[String(args?.sessionId)] ?? null;
      case "session_list_by_project":
        return [];
      case "session_upsert":
      case "control_window_booted":
      case "quit_ready":
      case "destroy_window":
        return null;
      default:
        throw `not a recorded command: ${command}`;
    }
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(async () => true) }));

import { getSession } from "../../sessions/data/sessionStore";
import { sessionWorkCwd } from "../../sessions/model/session";
import { detachSessionWorktree } from "../../source-control/model/worktrees";
import { newTab } from "./layout";
import type { WindowWorkspace } from "./workspaceSnapshot";
import { tabWorkspace } from "./workspaceTabGroups";

const PROJECT = realDb.snapshot.projectCwd;

/** A fresh module graph, as a relaunch or a reload gets. */
async function webview() {
  vi.resetModules();
  const lifecycle = await import("../../../app/model/appLifecycle");
  const model = await import("./workspaceSnapshot");
  const sessionStore = await import("../../sessions/data/sessionStore");
  return { ...lifecycle, ...model, ...sessionStore };
}

async function boot(): Promise<WindowWorkspace> {
  const window = await (await webview()).loadResumedWorkspace();
  if (!window) throw new Error("the saved workspace does not restore");
  return window;
}

/**
 * The window after the user worked in worktrees, the way the app pins tabs:
 * each saved tab whose session works in a worktree was reopened there from
 * the switcher (useWorkspaceNavigation), and the rail opened the idle
 * worktree sessions with no tab in their own worktree (App.tsx
 * onSelectHistorySession).
 */
async function inWorktrees(window: WindowWorkspace): Promise<WindowWorkspace> {
  const pins = new Map(window.pins);
  for (const tab of window.tabs) {
    const session = window.sessions.find((entry) => entry.id === tab.focusedId);
    if (session && sessionWorkCwd(session) !== session.cwd)
      pins.set(tab.id, sessionWorkCwd(session));
  }
  const tabs = [...window.tabs];
  const sessions = [...window.sessions];
  for (const id of realDb.railSessions) {
    const session = await getSession(id);
    if (!session) throw new Error(`no record for ${id}`);
    const tab = newTab(session.id);
    tabs.push(tab);
    sessions.push(session);
    pins.set(tab.id, sessionWorkCwd(session));
  }
  return { ...window, tabs, sessions, pins };
}

/** Where every tab belongs, by the rule the switcher and the strip use. */
const workspaces = (window: WindowWorkspace) =>
  window.tabs.map((tab) => [
    tab.id,
    tabWorkspace(tab, window.sessions, window.pins),
  ]);

function expectRestored(before: WindowWorkspace, after: WindowWorkspace) {
  expect(workspaces(after)).toEqual(workspaces(before));
  const active = after.tabs.find((tab) => tab.id === after.activeTabId)!;
  expect(tabWorkspace(active, after.sessions, after.pins)).toBe(PROJECT);
}

beforeEach(() => {
  store.snapshot = structuredClone(realDb.snapshot);
  store.inFlight = [];
  store.records = structuredClone(realDb.records) as Record<string, Row>;
});

it("saves every worktree tab with its worktree, and boot pins each one back", async () => {
  const window = await inWorktrees(await boot());
  const { saveWorkspaceSnapshot, workspaceSnapshot } = await webview();
  await saveWorkspaceSnapshot(workspaceSnapshot([window]));
  // 4 saved tabs reopened in their worktrees, 3 opened from the rail.
  expect(
    (store.snapshot as { tabWorkspaces?: unknown[] }).tabWorkspaces,
  ).toHaveLength(7);
  expectRestored(window, await boot());
});

it("a tab saved in a worktree that was removed comes back in the default workspace", async () => {
  const window = await inWorktrees(await boot());
  const { saveWorkspaceSnapshot, workspaceSnapshot } = await webview();
  await saveWorkspaceSnapshot(workspaceSnapshot([window]));
  const [removedTab] = window.tabs.filter(
    (tab) => tabWorkspace(tab, window.sessions, window.pins) !== PROJECT,
  );
  const id = removedTab.focusedId;
  const record = store.records[id] as { cwd: string; worktreeCwd: string };
  store.records[id] = detachSessionWorktree(record, PROJECT, record.worktreeCwd);

  const restored = await boot();
  expect(workspaces(restored)).toEqual(
    workspaces(window).map(([tabId, path]) =>
      tabId === removedTab.id ? [tabId, PROJECT] : [tabId, path],
    ),
  );
});
