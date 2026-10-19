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
import { sameProjectPath } from "../../projects/model/recents";
import { getSession } from "../../sessions/data/sessionStore";
import { sessionWorkCwd, type Session } from "../../sessions/model/session";
import { newTab, type WorkspaceTab } from "./layout";
import { insertTabBesideActive } from "./tabGroups";
import {
  collectWorkspaceSnapshot,
  hydrateWorkspaceSnapshot,
  parseWorkspaceSnapshot,
} from "./workspaceSnapshot";
import { workspaceTabCwd } from "./workspaceTabGroups";

it("saves an active tab it also saves after a running worktree session opens, and boot lands on it", async () => {
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
  // beside the active tab, pinned to its own worktree. Every restored tab is
  // pinned to its project folder (restoredPins), and keepWorkspaceTab saves a
  // tab only when its pin is that folder.
  const opened = newTab(running.id);
  const tabs = insertTabBesideActive(window.tabs, opened, window.activeTabId);
  const sessions = [...window.sessions, running];
  const pins = new Map(
    window.tabs.map((tab) => [tab.id, workspaceTabCwd(tab, sessions) ?? ""]),
  );
  pins.set(opened.id, sessionWorkCwd(running));
  const keepTab = (tab: WorkspaceTab) =>
    sameProjectPath(pins.get(tab.id) ?? "", workspaceTabCwd(tab, sessions) ?? "");

  const snapshot = collectWorkspaceSnapshot(
    tabs,
    sessions,
    opened.id,
    window.projectCwd,
    new Map(),
    [],
    undefined,
    keepTab,
  );
  saved.snapshot = snapshot;
  const restored = await loadResumedWorkspace();

  expect({
    savedTabs: snapshot.tabs.map((tab) => tab.id),
    savedActive: snapshot.activeTabId,
    bootActive: restored?.activeTabId,
  }).toEqual({
    savedTabs: ["tab-0001", "tab-0002", "tab-0004"],
    savedActive: "tab-0002",
    bootActive: "tab-0002",
  });
});
