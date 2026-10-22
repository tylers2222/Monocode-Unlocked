import { beforeEach, expect, it, vi } from "vitest";
import { newSession } from "../../features/sessions/model/session";
import { newTab } from "../../features/workspace/model/layout";
import { workspaceSnapshot } from "../../features/workspace/model/workspaceSnapshot";

const store = vi.hoisted(() => ({
  loadWorkspaceSnapshot: vi.fn(),
  listInFlightSessions: vi.fn(),
  getSession: vi.fn(),
  upsertSession: vi.fn(),
}));
vi.mock("../../features/sessions/data/sessionStore", async (original) => ({
  ...(await original<
    typeof import("../../features/sessions/data/sessionStore")
  >()),
  ...store,
}));
vi.mock("../../integrations/harness/core/registry", () => ({}));
vi.mock("../../integrations/harness/core/child", () => ({}));
const core = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => core);

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  core.invoke.mockResolvedValue(null);
  store.listInFlightSessions.mockResolvedValue([]);
  store.upsertSession.mockResolvedValue(null);
});

function savedWorkspace() {
  const sessions = Array.from({ length: 10 }, (_, index) => ({
    ...newSession("codex", "/repo"),
    id: `session-${index}`,
    blocks: [{ id: `user-${index}`, role: "user" as const, text: "Hello" }],
  }));
  const tabs = sessions.map((session) => newTab(session.id));
  store.loadWorkspaceSnapshot.mockResolvedValue(
    workspaceSnapshot([
      {
        tabs,
        sessions,
        activeTabId: tabs[0].id,
        projectCwd: "/repo",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]),
  );
  store.getSession.mockImplementation(
    async (id: string) => sessions.find((session) => session.id === id) ?? null,
  );
  return sessions;
}

it("restores ten idle tabs without rewriting their transcripts before first paint", async () => {
  const sessions = savedWorkspace();
  const { loadResumedWorkspace } = await import("./appLifecycle");
  const restored = await loadResumedWorkspace();
  expect(restored?.sessions.map((session) => session.id)).toEqual(
    sessions.map((session) => session.id),
  );
  expect(restored?.tabs).toHaveLength(10);
  expect(store.upsertSession).not.toHaveBeenCalled();
});

it("still persists the interruption marker for a recovered running turn", async () => {
  const sessions = savedWorkspace();
  store.listInFlightSessions.mockResolvedValue([
    { sessionId: sessions[0].id, cwd: "/repo" },
  ]);
  const { loadResumedWorkspace } = await import("./appLifecycle");
  const restored = await loadResumedWorkspace();
  expect(store.upsertSession).toHaveBeenCalledTimes(1);
  expect(store.upsertSession).toHaveBeenCalledWith(restored?.sessions[0]);
  expect(restored?.sessions[0].blocks.at(-1)?.notice).toBe("interrupt");
});

// Design 2 (review r4). Defect: the window loads a session before Rust ended
// the turns and hand-offs that its reload left, so a restored session can
// start a turn first, and that scan then kills it.
it("ends what a reload of the window left before it loads any session", async () => {
  savedWorkspace();
  const order: string[] = [];
  let open!: () => void;
  const ended = new Promise<void>((resolve) => (open = resolve));
  core.invoke.mockImplementation(async (command: string) => {
    if (command === "control_window_booted") await ended;
    order.push(command);
    return null;
  });
  store.getSession.mockImplementation(async () => {
    order.push("session_get");
    return null;
  });
  const { loadBootWorkspace } = await import("./appLifecycle");
  const booting = loadBootWorkspace();
  open();
  await booting;
  expect(order[0]).toBe("control_window_booted");
  expect(order).toContain("session_get");
});

// With no saved workspace, the chats that were running come back one tab each.
// Boot opens the default workspace, so each tab is pinned to its project
// folder, including one whose chat works in a worktree.
it("pins each tab of an in-flight-only restore to its project folder", async () => {
  const sessions = [
    { ...newSession("codex", "/repo"), id: "main" },
    {
      ...newSession("codex", "/repo"),
      id: "tree",
      worktreeCwd: "/repo-trees/a",
    },
  ];
  store.loadWorkspaceSnapshot.mockResolvedValue(null);
  store.listInFlightSessions.mockResolvedValue(
    sessions.map((session) => ({ sessionId: session.id, cwd: "/repo" })),
  );
  store.getSession.mockImplementation(
    async (id: string) => sessions.find((session) => session.id === id) ?? null,
  );
  const { loadResumedWorkspace } = await import("./appLifecycle");
  const restored = await loadResumedWorkspace();
  expect(
    restored?.tabs.map((tab) => [tab.focusedId, restored.pins.get(tab.id)]),
  ).toEqual([
    ["main", "/repo"],
    ["tree", "/repo"],
  ]);
});
