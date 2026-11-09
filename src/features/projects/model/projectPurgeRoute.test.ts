import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import { __spacesTestReset, loadSpaces } from "../../spaces/model/activeSpace";
import { setOpenSessionsSource } from "./projectDeleteCheck";
import { purgeProjectInSpace } from "./projectPurge";

const P = "/code/vibtra";

/** Stands in for monocode.db behind the commands the route calls; no real data. */
const db = {
  rows: [] as { id: string; cwd: string; spaceId: string }[],
  previewRefusal: undefined as unknown,
  deleteFailure: undefined as unknown,
  listFails: false,
};

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
  invoke: vi.fn(async (command: string) => {
    switch (command) {
      case "spaces_list":
        return [{ id: "space-2", name: "Space 2", color: null, createdAt: 2 }];
      case "spaces_settings_get":
        return { activeSpaceId: "space-2", routeGrantTtlMs: 1 };
      case "spaces_logins_list":
        return [];
      case "session_list_by_project":
        if (db.listFails) throw "database is locked";
        return db.rows;
      case "spaces_project_delete_preview":
        if (db.previewRefusal) throw db.previewRefusal;
        return { lastSpace: false, sessionCount: 1, otherSpaces: [], hiddenSessionCount: 0 };
      case "spaces_project_delete":
        if (db.deleteFailure) throw db.deleteFailure;
        return { deletedSessionIds: ["two-a"], lastSpace: false };
      default:
        return null;
    }
  }),
}));

const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);

beforeEach(async () => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => undefined });
  db.rows = [{ id: "two-a", cwd: P, spaceId: "space-2" }];
  db.previewRefusal = undefined;
  db.deleteFailure = undefined;
  db.listFails = false;
  __spacesTestReset();
  await loadSpaces();
  setOpenSessionsSource(() => []);
  vi.mocked(invoke).mockClear();
});

describe("the window's route to the delete (T10, T11, I8)", () => {
  it("deletes once the guards pass", async () => {
    const { refusal, purge } = await purgeProjectInSpace(P);
    expect(refusal).toBeNull();
    expect(purge).toEqual({
      deleted: new Set(["two-a"]),
      lastSpace: false,
      complete: true,
    });
  });

  it("does not call the delete when the backend refuses", async () => {
    const message = "Vibtra Orchestrator uses vibtra. Take it off that Mono first.";
    db.previewRefusal = { code: "monoHeld", message };
    await expect(purgeProjectInSpace(P)).resolves.toEqual({ refusal: message, purge: null });
    expect(commands()).not.toContain("spaces_project_delete");
  });

  it("does not call the delete when the window refuses", async () => {
    const busy = {
      ...newSession("claude", P),
      id: "two-a",
      busy: true,
    } as Session;
    setOpenSessionsSource(() => [busy]);
    await expect(purgeProjectInSpace(P)).resolves.toEqual({
      refusal: "A session is running in vibtra here. Stop it first.",
      purge: null,
    });
    expect(commands()).not.toContain("spaces_project_delete");
  });

  it("returns the message and the rows that went after a failure", async () => {
    db.deleteFailure = { code: "failed", message: "disk full" };
    const { refusal, purge } = await purgeProjectInSpace(P);
    expect(refusal).toBe("disk full");
    expect(purge?.complete).toBe(false);
    expect(purge?.lastSpace).toBe(false);
  });

  it("acts on nothing when the rows cannot be read", async () => {
    db.listFails = true;
    await expect(purgeProjectInSpace(P)).resolves.toEqual({
      refusal: "database is locked",
      purge: null,
    });
    expect(commands()).not.toContain("spaces_project_delete");
  });
});
