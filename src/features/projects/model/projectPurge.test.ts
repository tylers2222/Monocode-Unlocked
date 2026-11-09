import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  leaf,
  newFileTab,
  newTab,
  type LayoutNode,
  type WorkspaceTab,
} from "../../workspace/model/layout";
import {
  deleteProjectInSpace,
  type SessionSummary,
  upsertSession,
} from "../../sessions/data/sessionStore";
import { newSession, type Session } from "../../sessions/model/session";
import {
  __spacesTestReset,
  loadSpaces,
} from "../../spaces/model/activeSpace";
import {
  loadTabGroupLabels,
  saveTabGroupLabel,
} from "../../workspace/model/tabGroups";
import { projectKey } from "../../../shared/lib/paths";
import {
  loadProjectGroupAssignments,
  saveProjectGroups,
  setProjectGroupAssignment,
} from "./projectGroups";
import {
  deleteProjectChats,
  forgetPurgedProject,
  purgeWindowPlan,
} from "./projectPurge";
import {
  archiveProject,
  loadArchivedProjects,
  loadRecents,
  rememberProject,
} from "./recents";

const P = "/code/vibtra";
const REMOTE = "remote://box/code/vibtra";

/** Stands in for monocode.db behind the Space-scoped delete; no real data. */
const db = {
  active: "space-2",
  rows: [] as Pick<SessionSummary, "id" | "cwd" | "spaceId">[],
  lastSpace: false,
  /** Deletes this many rows, then fails, as `end_session` can. */
  failAfter: undefined as number | undefined,
  /** Each delete waits for this. */
  deleteGate: Promise.resolve(),
  /** Each session save waits for this. */
  saveGate: Promise.resolve(),
  /** The next reads of a project's rows fail. */
  failList: false,
};

const own = () => db.rows.filter((row) => row.spaceId === db.active);

vi.mock("../../monos/model/mono", async (original) => ({
  ...(await original<typeof import("../../monos/model/mono")>()),
  isMonoSession: (id: string) => id.startsWith("mono-"),
}));

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
        return [];
      case "session_list_by_project":
        if (db.failList) throw "database is locked";
        return db.rows.filter((row) => row.cwd === args.cwd);
      case "session_upsert":
        await db.saveGate;
        return null;
      case "spaces_project_delete": {
        await db.deleteGate;
        const doomed = own()
          .filter((row) => !row.id.startsWith("mono-"))
          .map((row) => row.id);
        if (db.failAfter !== undefined) {
          const gone = new Set(doomed.slice(0, db.failAfter));
          db.rows = db.rows.filter((row) => !gone.has(row.id));
          throw { code: "failed", message: "disk full" };
        }
        db.rows = db.rows.filter((row) => !doomed.includes(row.id));
        return { deletedSessionIds: doomed, lastSpace: db.lastSpace };
      }
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

const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);

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
  db.lastSpace = false;
  db.failAfter = undefined;
  db.deleteGate = Promise.resolve();
  db.saveGate = Promise.resolve();
  db.failList = false;
  __spacesTestReset();
  await loadSpaces();
  vi.mocked(invoke).mockClear();
});

describe("a purge from Space 2 of a project Space 1 has (T10, R5, R8)", () => {
  it("deletes only through the scoped command, with S from the window", async () => {
    const purge = await deleteProjectChats(P);
    expect(purge.failure).toBeUndefined();
    expect([...purge.deleted]).toEqual(["two-a", "two-b"]);
    expect(purge.complete).toBe(true);
    expect(db.rows.map((row) => row.id)).toEqual(["one-a", "one-busy"]);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("spaces_project_delete", {
      projectPath: P,
    });
    // The old purge's commands are out of reach.
    expect(commands()).not.toContain("session_delete");
    expect(commands()).not.toContain("milestones_remove_project");
  });

  it("keeps a running Space 1 turn and its tab, and closes only S's", () => {
    const sessions = [
      session("one-busy", { busy: true }),
      session("one-a"),
      session("two-a"),
    ];
    const tabs = sessions.map((entry) => newTab(entry.id));
    const plan = purgeWindowPlan(tabs, sessions, P, {
      deleted: new Set(["two-a", "two-b"]),
      lastSpace: false,
      complete: true,
    });
    expect([...plan.sessionIds]).toEqual(["two-a"]);
    expect(plan.tabs).toEqual([tabs[2]]);
  });

  it("never drops a busy session, even one the backend named", () => {
    const sessions = [session("two-a", { busy: true })];
    const plan = purgeWindowPlan([newTab("two-a")], sessions, P, {
      deleted: new Set(["two-a"]),
      lastSpace: false,
      complete: true,
    });
    expect([...plan.sessionIds]).toEqual([]);
    expect(plan.tabs).toEqual([]);
  });

  it("keeps a tab that also holds another Space's session, without the deleted pane", () => {
    const sessions = [session("one-a"), session("two-a")];
    const tab = {
      ...newTab("one-a"),
      focusedId: "two-a",
      layout: {
        type: "split",
        id: "s",
        dir: "row",
        children: [leaf("one-a"), leaf("two-a")],
        sizes: [0.5, 0.5],
      } as unknown as LayoutNode,
    };
    const plan = purgeWindowPlan([tab], sessions, P, {
      deleted: new Set(["two-a"]),
      lastSpace: false,
      complete: true,
    });
    expect(plan.tabs).toEqual([]);
    expect([...plan.sessionIds]).toEqual(["two-a"]);
    expect(plan.trimmed.get(tab.id)?.layout).toEqual(leaf("one-a"));
    expect(plan.trimmed.get(tab.id)?.focusedId).toBe("one-a");
  });

  it("closes a files-only tab only with the last Space", () => {
    const file = newFileTab(`${P}/a.ts`, P);
    const pane = { id: "file-pane", files: [file], activeFileId: file.id };
    const tab = {
      ...newTab("file-pane"),
      editorPanes: [pane],
    } as unknown as WorkspaceTab;
    const purge = { deleted: new Set<string>(), complete: true };
    expect(purgeWindowPlan([tab], [], P, { ...purge, lastSpace: false }).tabs).toEqual([]);
    expect(purgeWindowPlan([tab], [], P, { ...purge, lastSpace: true }).tabs).toEqual([tab]);
    expect(purgeWindowPlan([tab], [], "/other", { ...purge, lastSpace: true }).tabs).toEqual([]);
  });

  it("does not close a blank draft with queued messages, or a remote Mono chat", () => {
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    const sessions = [
      session("draft", { blocks: [], queuedMessages }),
      session("remote-queue", { cwd: REMOTE, queuedMessages }),
      session("mono-remote", { cwd: REMOTE }),
    ];
    const purge = { deleted: new Set<string>(), lastSpace: false, complete: true };
    const tabs = sessions.map((entry) => newTab(entry.id));
    expect([...purgeWindowPlan(tabs, sessions, P, purge).sessionIds]).toEqual([]);
    expect([...purgeWindowPlan(tabs, sessions, REMOTE, purge).sessionIds]).toEqual([]);
  });

  it("closes a blank draft, which nobody else owns, but no Mono chat", () => {
    const sessions = [
      session("draft", { blocks: [] }),
      session("mono-chat", { blocks: [] }),
    ];
    const plan = purgeWindowPlan(sessions.map((s) => newTab(s.id)), sessions, P, {
      deleted: new Set(),
      lastSpace: false,
      complete: true,
    });
    expect([...plan.sessionIds]).toEqual(["draft"]);
  });

  it("leaves shared rail, archive, group, label and logo (T10)", async () => {
    rememberProject(P);
    saveProjectGroups([{ id: "g", name: "G", collapsed: false }]);
    setProjectGroupAssignment(P, "g");
    saveTabGroupLabel(projectKey(P), "Vibtra");
    const before = loadRecents();

    const after = forgetPurgedProject(P, {
      deleted: new Set(["two-a"]),
      lastSpace: false,
      complete: true,
    });

    expect(after).toEqual(before);
    expect(loadRecents()).toEqual(before);
    expect(Object.keys(loadProjectGroupAssignments())).toHaveLength(1);
    expect(loadTabGroupLabels()[projectKey(P)]).toBe("Vibtra");
    expect(commands()).not.toContain("remove_project_logo");
    expect(commands()).not.toContain("remove_project_chat_background");
  });
});

describe("a purge from the last Space (T11, R13)", () => {
  it("clears shared data, but other Spaces' hidden chats and milestones are not the window's", async () => {
    db.lastSpace = true;
    rememberProject(P);
    saveProjectGroups([{ id: "g", name: "G", collapsed: false }]);
    setProjectGroupAssignment(P, "g");

    const after = forgetPurgedProject(P, await deleteProjectChats(P));
    await vi.waitFor(() =>
      expect(loadProjectGroupAssignments()).toEqual({}),
    );

    expect(after).toEqual([]);
    expect(commands()).toContain("remove_project_logo");
    // Space 1's two hidden chats are still there, and no window path reached
    // the old purge: the backend deletes milestones in its transaction.
    expect(db.rows.map((row) => row.id)).toEqual(["one-a", "one-busy"]);
    expect(commands()).not.toContain("session_delete");
    expect(commands()).not.toContain("milestones_remove_project");
  });
});

describe("a remote project (T16, G6)", () => {
  const remote = (id: string, fields: Partial<Session> = {}) =>
    session(id, { cwd: REMOTE, ...fields });

  it("closes the window's idle tabs in P, and only those, when it completes", () => {
    const sessions = [remote("r"), session("two-a")];
    const tabs = sessions.map((entry) => newTab(entry.id));
    const purge = { deleted: new Set<string>(), lastSpace: false, complete: true };
    expect([...purgeWindowPlan(tabs, sessions, REMOTE, purge).sessionIds]).toEqual(["r"]);
    expect(purgeWindowPlan(tabs, sessions, REMOTE, purge).tabs).toEqual([tabs[0]]);
  });

  it("keeps a recent on a scoped delete, and takes the normal route", async () => {
    rememberProject(REMOTE);
    const purge = await deleteProjectChats(REMOTE);
    expect(purge).toMatchObject({ lastSpace: false, complete: true });
    expect(forgetPurgedProject(REMOTE, purge)).toHaveLength(1);
  });
});

describe("a failure halfway (G10)", () => {
  it("reports it, acts only on rows that went, and leaves P listed", async () => {
    db.failAfter = 1;
    rememberProject(P);
    const purge = await deleteProjectChats(P);
    expect(purge.failure).toMatchObject({ message: "disk full" });
    expect([...purge.deleted]).toEqual(["two-a"]);
    expect(purge.complete).toBe(false);
    expect(forgetPurgedProject(P, purge)).toHaveLength(1);
    expect(commands()).not.toContain("remove_project_logo");

    const sessions = [
      session("two-a"),
      session("two-b"),
      session("draft", { blocks: [] }),
    ];
    const plan = purgeWindowPlan(sessions.map((s) => newTab(s.id)), sessions, P, purge);
    expect([...plan.sessionIds]).toEqual(["two-a"]);
  });

  it("keeps a row that went from saving again, and lets one that stayed save", async () => {
    db.failAfter = 1;
    db.rows = [
      { id: "fresh-went", cwd: P, spaceId: "space-2" },
      { id: "fresh-stayed", cwd: P, spaceId: "space-2" },
    ];
    await deleteProjectInSpace(P, ["fresh-went", "fresh-stayed"]);
    vi.mocked(invoke).mockClear();
    await upsertSession(session("fresh-went"));
    expect(commands()).not.toContain("session_upsert");
    await upsertSession(session("fresh-stayed"));
    expect(commands()).toContain("session_upsert");
  });
});

describe("writes around the delete (G10)", () => {
  it("blocks S's rows while the delete runs, and frees them on a refusal", async () => {
    let release = () => {};
    db.deleteGate = new Promise((done) => (release = done));
    // Tombstones last for good, so each test here needs ids no other test deleted.
    db.rows.push({ id: "fresh-block", cwd: P, spaceId: "space-2" });
    const running = deleteProjectInSpace(P, ["fresh-block"]);
    await new Promise((done) => setTimeout(done));
    await upsertSession(session("fresh-block"));
    expect(commands()).not.toContain("session_upsert");
    release();
    await running;
    await upsertSession(session("fresh-block"));
    expect(commands()).not.toContain("session_upsert");

    db.failAfter = 0;
    db.rows.push({ id: "two-c", cwd: P, spaceId: "space-2" });
    const { failure } = await deleteProjectInSpace(P, ["two-c"]);
    expect(failure).toBeDefined();
    await upsertSession(session("two-c"));
    expect(commands()).toContain("session_upsert");
  });

  it("never lets a deleted chat save again, even after a minute (R18)", async () => {
    vi.useFakeTimers();
    try {
      db.rows.push({ id: "fresh-r18", cwd: P, spaceId: "space-2" });
      await deleteProjectInSpace(P, ["fresh-r18"]);
      vi.advanceTimersByTime(120_000);
    } finally {
      vi.useRealTimers();
    }
    vi.mocked(invoke).mockClear();
    await upsertSession(session("fresh-r18"));
    expect(commands()).not.toContain("session_upsert");
  });

  it("lets writes in flight finish before the delete", async () => {
    let release = () => {};
    db.saveGate = new Promise((done) => (release = done));
    const saving = upsertSession(session("one-a"));
    await new Promise((done) => setTimeout(done));
    const deleting = deleteProjectInSpace(P, ["two-a"]);
    await new Promise((done) => setTimeout(done));
    expect(commands()).not.toContain("spaces_project_delete");
    release();
    await Promise.all([saving, deleting]);
    expect(commands()).toContain("spaces_project_delete");
  });

  it("takes only this Space's rows out of saving while the delete runs", async () => {
    db.rows.push({ id: "fresh-chats", cwd: P, spaceId: "space-2" });
    let release = () => {};
    db.deleteGate = new Promise((done) => (release = done));
    const deleting = deleteProjectChats(P);
    await new Promise((done) => setTimeout(done));
    await upsertSession(session("fresh-chats"));
    expect(commands()).not.toContain("session_upsert");
    await upsertSession(session("one-a"));
    expect(commands()).toContain("session_upsert");
    release();
    await deleting;
  });

  it("holds every row for a minute when a failure leaves the rows unreadable", async () => {
    vi.useFakeTimers();
    try {
      db.failAfter = 0;
      const deleting = deleteProjectInSpace(P, ["fresh-unread"]);
      db.failList = true;
      await vi.advanceTimersByTimeAsync(0);
      await expect(deleting).resolves.toMatchObject({
        deletedSessionIds: [],
        lastSpace: false,
      });
      db.failList = false;
      vi.mocked(invoke).mockClear();
      await upsertSession(session("fresh-unread"));
      expect(commands()).not.toContain("session_upsert");
      await vi.advanceTimersByTimeAsync(60_000);
      await upsertSession(session("fresh-unread"));
      expect(commands()).toContain("session_upsert");
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not block another Space's session", async () => {
    await deleteProjectInSpace(P, ["two-a"]);
    vi.mocked(invoke).mockClear();
    await upsertSession(session("one-a"));
    expect(commands()).toContain("session_upsert");
  });
});

it("does not touch Archive's own list on a scoped purge", () => {
  rememberProject(P);
  archiveProject(P);
  const before = loadArchivedProjects();
  forgetPurgedProject(P, { deleted: new Set(), lastSpace: false, complete: true });
  expect(loadArchivedProjects()).toEqual(before);
});
