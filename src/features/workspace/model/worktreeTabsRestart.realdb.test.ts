import { beforeEach, describe, expect, it, vi } from "vitest";
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

// The App entry points that save through these (normal use, close-requested,
// reload) are proven in tests/browser/ade57-worktree-tabs-restart.spec.ts.
describe("worktree tabs come back after each save path (real saved data)", () => {
  it("workspaceSnapshot written by saveWorkspaceSnapshot, then a crash", async () => {
    const window = await inWorktrees(await boot());
    const { saveWorkspaceSnapshot, workspaceSnapshot } = await webview();
    await saveWorkspaceSnapshot(workspaceSnapshot([window]));
    // 4 saved tabs reopened in their worktrees, 3 opened from the rail.
    expect(
      (store.snapshot as { tabWorkspaces?: unknown[] }).tabWorkspaces,
    ).toHaveLength(7);
    expectRestored(window, await boot());
  });

  it("quit, with a worktree chat still running", async () => {
    const booted = await inWorktrees(await boot());
    const runningTab = booted.tabs[1];
    const window = {
      ...booted,
      sessions: booted.sessions.map((session) =>
        session.id === runningTab.focusedId
          ? { ...session, busy: true }
          : session,
      ),
    };
    const { commitQuit, setQuitWorkspace } = await webview();
    setQuitWorkspace(
      () => window,
      () => undefined,
    );
    await commitQuit(1);
    expect(store.inFlight.map((ref) => ref.sessionId)).toEqual([
      runningTab.focusedId,
    ]);

    const restored = await boot();
    expectRestored(window, restored);
    // F3 (a): it comes back in its own saved tab, in its worktree, not as a
    // new tab at the end of the default strip.
    expect(restored.tabs[1].id).toBe(runningTab.id);
    expect(
      restored.sessions
        .find((session) => session.id === runningTab.focusedId)
        ?.blocks.at(-1)?.notice,
    ).toBe("interrupt");
  });

  it("persistQuitState unload, as close-requested and reload call it", async () => {
    const window = await inWorktrees(await boot());
    const { persistQuitState } = await webview();
    await persistQuitState(window, "unload");
    expectRestored(window, await boot());
  });

  it("close of a busy window", async () => {
    const booted = await inWorktrees(await boot());
    const window = {
      ...booted,
      sessions: booted.sessions.map((session, index) =>
        index === 0 ? { ...session, busy: true } : session,
      ),
    };
    const { closeBusyWindow, setQuitWorkspace } = await webview();
    setQuitWorkspace(
      () => window,
      () => undefined,
    );
    await closeBusyWindow();
    expectRestored(window, await boot());
  });

  it("quit while the app is still booting", async () => {
    const window = await inWorktrees(await boot());
    const first = await webview();
    await first.saveWorkspaceSnapshot(first.workspaceSnapshot([window]));

    const booting = await webview();
    await booting.loadBootWorkspace();
    expect(await booting.handleQuitRequested()).toBe(true);
    expectRestored(window, await boot());
  });
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
  store.records[id] = detachSessionWorktree(
    record,
    PROJECT,
    record.worktreeCwd,
  );

  const restored = await boot();
  expect(workspaces(restored)).toEqual(
    workspaces(window).map(([tabId, path]) =>
      tabId === removedTab.id ? [tabId, PROJECT] : [tabId, path],
    ),
  );
});

it("with every tab in a worktree, saves no active tab, and boot keeps each tab in its worktree with none active", async () => {
  const booted = await inWorktrees(await boot());
  const window = { ...booted, tabs: booted.tabs.slice(0, 2) };
  const { saveWorkspaceSnapshot, workspaceSnapshot } = await webview();
  const saved = workspaceSnapshot([window]);
  await saveWorkspaceSnapshot(saved);
  const restored = await boot();
  expect({
    savedActive: saved.activeTabId,
    bootActive: restored.activeTabId,
    workspaces: workspaces(restored),
  }).toEqual({
    savedActive: "",
    bootActive: "",
    workspaces: workspaces(window),
  });
  expect(workspaces(window).every(([, path]) => path !== PROJECT)).toBe(true);
});
