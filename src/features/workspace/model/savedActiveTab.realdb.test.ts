import { expect, it, vi } from "vitest";
import realDb from "./fixtures/worktreeTabClick.realdb.json";

const saved = vi.hoisted(() => ({ snapshot: null as unknown }));

// Only the IPC boundary is replaced. It answers the boot reads from the
// fixture and from what the real save wrote, accepts the interruption marker
// the boot writes, and refuses everything else.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args?: { sessionId?: string }) => {
    if (command === "workspace_get_snapshot") return saved.snapshot;
    if (command === "session_list_in_flight") return realDb.inFlight;
    if (command === "session_get" || command === "mono_session_get")
      return (
        (realDb.records as Record<string, unknown>)[args?.sessionId ?? ""] ??
        null
      );
    if (command === "session_upsert") return null;
    throw `not a recorded read: ${command}`;
  }),
}));

import { loadResumedWorkspace } from "../../../app/model/appLifecycle";
import { getSession } from "../../sessions/data/sessionStore";
import { sessionWorkCwd, type Session } from "../../sessions/model/session";
import { newTab } from "./layout";
import { insertTabBesideActive } from "./tabGroups";
import {
  hydrateWorkspaceSnapshot,
  parseWorkspaceSnapshot,
  workspaceSnapshot,
} from "./workspaceSnapshot";

it("saves a running worktree session's tab in its worktree, keeps the active tab in the default workspace, and boot restores both", async () => {
  const before = parseWorkspaceSnapshot(realDb.snapshot);
  if (!before) throw new Error("the saved snapshot does not parse");
  const loaded = new Map<string, Session>();
  for (const stub of before.sessions) {
    const session = await getSession(stub.id);
    if (session) loaded.set(session.id, session);
  }
  const window = hydrateWorkspaceSnapshot(before, loaded);
  const running = await getSession(realDb.inFlight[0].sessionId);
  if (!window || !running) throw new Error("the fixture does not load");

  // App.tsx onSelectHistorySession: a running session with no tab opens
  // beside the active tab, pinned to its own worktree.
  const opened = newTab(running.id);
  const worktree = sessionWorkCwd(running);
  const snapshot = workspaceSnapshot([
    {
      ...window,
      tabs: insertTabBesideActive(window.tabs, opened, window.activeTabId),
      sessions: [...window.sessions, running],
      activeTabId: opened.id,
      pins: new Map(window.pins).set(opened.id, worktree),
    },
  ]);
  saved.snapshot = snapshot;
  const restored = await loadResumedWorkspace();

  expect(worktree).not.toBe(window.projectCwd);
  expect({
    savedTabs: snapshot.tabs.map((tab) => tab.id),
    tabWorkspaces: snapshot.tabWorkspaces,
    savedActive: snapshot.activeTabId,
    bootTabs: restored?.tabs.map((tab) => tab.id),
    bootActive: restored?.activeTabId,
    bootPin: restored?.pins.get(opened.id),
  }).toEqual({
    savedTabs: ["tab-0001", "tab-0002", opened.id, "tab-0004"],
    tabWorkspaces: [{ tabId: opened.id, path: worktree }],
    savedActive: "tab-0002",
    bootTabs: ["tab-0001", "tab-0002", opened.id, "tab-0004"],
    bootActive: "tab-0002",
    bootPin: worktree,
  });
});
