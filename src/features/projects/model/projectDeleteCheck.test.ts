import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import {
  __spacesTestReset,
  loadSpaces,
  spacesView,
} from "../../spaces/model/activeSpace";
import {
  checkProjectArchive,
  checkProjectDelete,
  readProjectDeletePrompt,
  setOpenSessionsSource,
  SPACE_UNREAD_REFUSAL,
} from "./projectDeleteCheck";
import {
  checkProjectArchive as checkArchive,
  notifySpaceProjectsChanged,
  previewProjectDelete,
  refusalMessage,
} from "../../spaces/model/spaces";

const P = "/code/vibtra";
const REMOTE = "remote://box/code/vibtra";

/** Stands in for monocode.db behind the Space-scoped delete; no real data. */
const db = {
  active: "space-2",
  rows: [] as Pick<SessionSummary, "id" | "cwd" | "spaceId">[],
  /** A typed refusal the next preview or delete rejects with. */
  refusal: undefined as unknown,
  archiveRefusal: undefined as unknown,
  lastSpace: false,
  /** Reads of the Spaces fail while this is set. */
  loginsFail: false,
  otherSpaces: ["space-1"],
};

const own = () => db.rows.filter((row) => row.spaceId === db.active);

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
  invoke: vi.fn(async (command: string, args: Record<string, unknown> = {}) => {
    switch (command) {
      case "spaces_list":
        return [
          { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
          { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
        ];
      case "spaces_settings_get":
        return { activeSpaceId: db.active, routeGrantTtlMs: 1 };
      case "spaces_logins_list":
        if (db.loginsFail) throw "database is locked";
        return [];
      case "session_list_by_project":
        return db.rows.filter((row) => row.cwd === args.cwd);
      case "spaces_project_delete_preview":
        if (db.refusal) throw db.refusal;
        return {
          lastSpace: db.lastSpace,
          sessionCount: own().length,
          otherSpaces: db.otherSpaces,
          hiddenSessionCount: db.rows.filter((row) => row.spaceId !== db.active)
            .length,
        };
      case "spaces_project_archive_check":
        if (db.archiveRefusal) throw db.archiveRefusal;
        return null;
      default:
        return null;
    }
  }),
}));

function session(id: string, fields: Partial<Session> = {}): Session {
  return {
    ...newSession("claude", P),
    id,
    blocks: [{ id: `${id}-u`, role: "user", text: "hi" }],
    ...fields,
  } as Session;
}

function mockBrowserStorage() {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
    clear: () => data.clear(),
    key: (index: number) => [...data.keys()][index] ?? null,
    get length() {
      return data.size;
    },
  });
  vi.stubGlobal("window", new EventTarget());
}

beforeEach(async () => {
  mockBrowserStorage();
  db.active = "space-2";
  db.rows = [
    { id: "one-a", cwd: P, spaceId: "space-1" },
    { id: "one-busy", cwd: P, spaceId: "space-1" },
    { id: "two-a", cwd: P, spaceId: "space-2" },
    { id: "two-b", cwd: P, spaceId: "space-2" },
  ];
  db.refusal = undefined;
  db.archiveRefusal = undefined;
  db.lastSpace = false;
  db.loginsFail = false;
  db.otherSpaces = ["space-1"];
  __spacesTestReset();
  await loadSpaces();
  setOpenSessionsSource(() => []);
});

describe("refusals (T7, T8, Mono chat)", () => {
  const mono = {
    code: "monoHeld",
    project: P,
    message: "Vibtra Orchestrator uses vibtra. Take it off that Mono first.",
  };

  it("shows the backend's message", async () => {
    db.refusal = mono;
    await expect(checkProjectDelete(P)).resolves.toBe(mono.message);
  });


  it("puts a Mono's refusal before a running session's", async () => {
    db.refusal = mono;
    setOpenSessionsSource(() => [session("two-a", { busy: true })]);
    await expect(checkProjectDelete(P)).resolves.toBe(mono.message);
  });

  it("refuses on S's busy session and S's queue, in T8's words", async () => {
    const text = "A session is running in vibtra here. Stop it first.";
    setOpenSessionsSource(() => [session("two-a", { busy: true })]);
    await expect(checkProjectDelete(P)).resolves.toBe(text);
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    setOpenSessionsSource(() => [session("two-a", { queuedMessages })]);
    await expect(checkProjectDelete(P)).resolves.toBe(text);
    // A queue on a session with no row may be S's: it is never lost.
    setOpenSessionsSource(() => [session("no-row", { queuedMessages })]);
    await expect(checkProjectDelete(P)).resolves.toBe(text);
  });

  it("does not refuse on another Space's busy session or queue", async () => {
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    setOpenSessionsSource(() => [
      session("one-busy", { busy: true }),
      session("one-a", { queuedMessages }),
      // A worker has no listed row; the backend's running check owns it.
      session("worker", { busy: true }),
    ]);
    await expect(checkProjectDelete(P)).resolves.toBeNull();
  });

  it("counts a session whose path is written another way", async () => {
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    setOpenSessionsSource(() => [session("two-a", { cwd: `${P}/`, queuedMessages })]);
    await expect(checkProjectDelete(P)).resolves.toBe(
      "A session is running in vibtra here. Stop it first.",
    );
    await expect(checkProjectArchive(P)).resolves.toBe(
      "A session is running in vibtra here. Stop it first.",
    );
  });

  it("T11: refuses on a queue in another Space's session on the last Space", async () => {
    db.lastSpace = true;
    db.otherSpaces = [];
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    setOpenSessionsSource(() => [session("one-a", { queuedMessages })]);
    await expect(checkProjectDelete(P)).resolves.toBe(
      "A session is running in vibtra in Space 1. Stop it first.",
    );
  });

  it("ignores a session in another project", async () => {
    setOpenSessionsSource(() => [
      session("two-a", { cwd: "/code/other", busy: true }),
    ]);
    await expect(checkProjectDelete(P)).resolves.toBeNull();
  });

  it("surfaces a plain failure instead of going ahead", async () => {
    db.refusal = "database is locked";
    await expect(checkProjectDelete(P)).resolves.toBe("database is locked");
    db.refusal = new Error("gone");
    await expect(checkProjectDelete(P)).resolves.toBe("gone");
  });
});

describe("a window whose Space read failed (B2)", () => {
  const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
  const unread = async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    __spacesTestReset();
    db.loginsFail = true;
    await loadSpaces();
    expect(spacesView()).toMatchObject({ loaded: false, activeSpaceId: "space-1" });
    setOpenSessionsSource(() => [session("two-a", { queuedMessages })]);
  };

  it("reads again and sees its own Space's queue", async () => {
    await unread();
    db.loginsFail = false;
    await expect(checkProjectDelete(P)).resolves.toBe(
      "A session is running in vibtra here. Stop it first.",
    );
    expect(spacesView().activeSpaceId).toBe("space-2");
  });

  it("reads again for Archive too", async () => {
    await unread();
    db.loginsFail = false;
    await expect(checkProjectArchive(P)).resolves.toBe(
      "A session is running in vibtra here. Stop it first.",
    );
  });

  it("refuses Delete if the read still fails, even with nothing running", async () => {
    await unread();
    setOpenSessionsSource(() => []);
    await expect(checkProjectDelete(P)).resolves.toBe(SPACE_UNREAD_REFUSAL);
  });

  it("refuses Archive if the read still fails", async () => {
    await unread();
    setOpenSessionsSource(() => []);
    await expect(checkProjectArchive(P)).resolves.toBe(SPACE_UNREAD_REFUSAL);
  });

  it("does not read again once a read has succeeded", async () => {
    vi.mocked(invoke).mockClear();
    await checkProjectDelete(P);
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).not.toContain(
      "spaces_list",
    );
  });

  it("puts the backend's refusal before a failed read", async () => {
    await unread();
    db.refusal = { code: "monoHeld", message: "Take it off." };
    await expect(checkProjectDelete(P)).resolves.toBe("Take it off.");
  });
});

describe("Archive (T9, R14)", () => {
  it("refuses on a session running in any Space, naming that Space", async () => {
    setOpenSessionsSource(() => [session("one-busy", { busy: true })]);
    await expect(checkProjectArchive(P)).resolves.toBe(
      "A session is running in vibtra in Space 1. Stop it first.",
    );
  });

  it("names a Space it has no name for by its id", async () => {
    db.rows.push({ id: "gone", cwd: P, spaceId: "space-9" });
    setOpenSessionsSource(() => [session("gone", { busy: true })]);
    await expect(checkProjectArchive(P)).resolves.toBe(
      "A session is running in vibtra in space-9. Stop it first.",
    );
  });

  it("says here when one is here too, or when the Space is unknown", async () => {
    const here = "A session is running in vibtra here. Stop it first.";
    setOpenSessionsSource(() => [
      session("one-busy", { busy: true }),
      session("two-a", { busy: true }),
    ]);
    await expect(checkProjectArchive(P)).resolves.toBe(here);
    setOpenSessionsSource(() => [session("worker", { busy: true })]);
    await expect(checkProjectArchive(P)).resolves.toBe(here);
  });

  it("puts the backend's refusal first, and passes when nothing runs", async () => {
    db.archiveRefusal = { code: "monoHeld", message: "Take it off." };
    setOpenSessionsSource(() => [session("two-a", { busy: true })]);
    await expect(checkProjectArchive(P)).resolves.toBe("Take it off.");
    db.archiveRefusal = undefined;
    setOpenSessionsSource(() => [session("one-a")]);
    await expect(checkProjectArchive(P)).resolves.toBeNull();
  });
});

describe("what the Delete dialog says", () => {
  it("counts S's chats and names the Space while another Space has P", async () => {
    await expect(readProjectDeletePrompt(P)).resolves.toEqual({
      sessions: 2,
      scoped: true,
      spaceName: "Space 2",
    });
  });

  it("is scoped when it is not the last Space, with nothing else elsewhere", async () => {
    db.lastSpace = false;
    db.otherSpaces = [];
    db.rows = db.rows.filter((row) => row.spaceId === "space-2");
    await expect(readProjectDeletePrompt(P)).resolves.toMatchObject({ scoped: true });
  });

  it("is scoped when another Space has it, whatever lastSpace says", async () => {
    db.lastSpace = true;
    db.otherSpaces = ["space-1"];
    db.rows = db.rows.filter((row) => row.spaceId === "space-2");
    await expect(readProjectDeletePrompt(P)).resolves.toMatchObject({ scoped: true });
  });

  it("is scoped when only hidden chats remain elsewhere", async () => {
    db.lastSpace = true;
    db.otherSpaces = [];
    await expect(readProjectDeletePrompt(P)).resolves.toMatchObject({ scoped: true });
  });

  it("is unscoped only when this Space is the only one with chats or membership", async () => {
    db.lastSpace = true;
    db.otherSpaces = [];
    db.rows = db.rows.filter((row) => row.spaceId === "space-2");
    await expect(readProjectDeletePrompt(P)).resolves.toMatchObject({ scoped: false });
  });

  it("stays scoped when it cannot read", async () => {
    db.refusal = "locked";
    await expect(readProjectDeletePrompt(P)).resolves.toEqual({
      sessions: 0,
      scoped: true,
      spaceName: "Space 2",
    });
  });
});

it("counts the window's own busy remote session as running (T16, G6)", async () => {
  setOpenSessionsSource(() => [session("r", { cwd: REMOTE, busy: true })]);
  await expect(checkProjectDelete(REMOTE)).resolves.toBe(
    "A session is running in vibtra here. Stop it first.",
  );
});

it("sees no running session before the window has given it its sessions", async () => {
  vi.resetModules();
  const fresh = await import("./projectDeleteCheck");
  const active = await import("../../spaces/model/activeSpace");
  await active.loadSpaces();
  await expect(fresh.checkProjectDelete(P)).resolves.toBeNull();
});

describe("the adapters", () => {
  it("sends the project to the preview and the archive check", async () => {
    await previewProjectDelete(P);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("spaces_project_delete_preview", {
      projectPath: P,
    });
    await checkArchive(P);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("spaces_project_archive_check", {
      projectPath: P,
    });
  });

  it("tells other windows, and a failure to is no one's problem", async () => {
    notifySpaceProjectsChanged();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("spaces_project_notify_changed");
    vi.mocked(invoke).mockRejectedValueOnce("unknown command");
    notifySpaceProjectsChanged();
    await new Promise((done) => setTimeout(done));
  });

  it.each([
    [{ code: "monoHeld", message: "Take it off." }, "Take it off."],
    [new Error("gone"), "gone"],
    ["database is locked", "database is locked"],
    [null, "null"],
    [{ code: "odd" }, "[object Object]"],
  ])("reads the refusal text of %j", (error, text) => {
    expect(refusalMessage(error)).toBe(text);
  });
});
