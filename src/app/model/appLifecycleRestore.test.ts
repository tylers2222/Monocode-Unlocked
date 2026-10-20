import { expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { getSession } from "../../features/sessions/data/sessionStore";
import { queuePauseNotice } from "../../features/sessions/model/messageQueue";
import { newSession } from "../../features/sessions/model/session";
import { newTab } from "../../features/workspace/model/layout";
import { collectWorkspaceSnapshot } from "../../features/workspace/model/workspaceSnapshot";
import { loadResumedWorkspace } from "./appLifecycle";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn() }));
vi.mock("../../integrations/harness/core/registry", () => ({
  bindHarnessSession: vi.fn(),
  isLiveHarness: vi.fn(),
  forgetHarnessSession: vi.fn(),
}));
vi.mock("../../integrations/harness/core/child", () => ({
  killAllChildren: vi.fn(),
}));

const stored = {
  id: "chat",
  cwd: "/tmp/project",
  harness: "codex",
  model: "model",
  modelSettings: {},
  runtimeMode: "default",
  title: "Chat",
  createdAt: 1,
  updatedAt: 2,
  blocks: [{ id: "u", role: "user", text: "Fix the parser" }],
  queuedMessages: [{ id: "pending", text: "Use TypeScript", attachments: [] }],
  queueStatus: "active",
};
it("restores a queue the quit stopped with its reason, even after a background read", async () => {
  const tab = newTab(stored.id);
  const snapshot = collectWorkspaceSnapshot(
    [tab],
    [{ ...newSession("codex", stored.cwd), id: stored.id }],
    tab.id,
    stored.cwd,
    new Map(),
  );
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "session_get") return stored;
    if (command === "workspace_get_snapshot") return snapshot;
    if (command === "session_list_in_flight") return [];
    return null;
  });
  const background = (await getSession(stored.id))!;
  expect(queuePauseNotice(background)).toBeUndefined();

  const restored = (await loadResumedWorkspace())!.sessions.find(
    (session) => session.id === stored.id,
  )!;
  expect(restored.queueStatus).toBe("paused");
  expect(queuePauseNotice(restored)).toBe("Paused when MonoCode quit");
  expect(invoke).toHaveBeenCalledWith(
    "session_upsert",
    expect.objectContaining({
      session: expect.objectContaining({
        queueStatus: "paused",
        queuedMessages: [expect.objectContaining({ pausedBy: "quit" })],
      }),
    }),
  );
});
