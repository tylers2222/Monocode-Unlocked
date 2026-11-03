import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MonoState } from "../../monos/model/mono";
import {
  __spacesTestReset,
  inActiveSpace,
  loadSpaces,
  otherSpaceHasNews,
  projectInActiveSpace,
  spaceActivity,
  spaceLoginId,
  spacesView,
  subscribeSpaces,
  switchSpace,
} from "./activeSpace";
import { createSpace, setSpaceLogin, updateSpace } from "./spaces";

/**
 * Stands in for `monocode.db` behind the Spaces commands, as
 * `spaces/mod.rs` and `spaces/account.rs` answer them.
 */
const db = {
  spaces: [] as { id: string; name: string; color: string | null; createdAt: number }[],
  active: "space-1",
  logins: [] as { spaceId: string; provider: string; accountId: string }[],
  projects: new Map<string, string>(),
  /** The next reads of the Space list fail, as a locked database would. */
  failList: false,
  /** The next Space read of a project fails. */
  failProject: false,
};

const settings = () => ({ activeSpaceId: db.active, routeGrantTtlMs: 21_600_000 });
type Args = { patch: { activeSpaceId?: string }; projectPath: string };
const COMMANDS: Record<string, (args: Args) => unknown> = {
  spaces_list: () => {
    if (db.failList) throw new Error("database is locked");
    return structuredClone(db.spaces);
  },
  spaces_settings_get: settings,
  spaces_settings_set: ({ patch }) => {
    db.active = patch.activeSpaceId ?? db.active;
    return settings();
  },
  spaces_logins_list: () => structuredClone(db.logins),
  spaces_project_get: ({ projectPath }) => {
    if (db.failProject) {
      db.failProject = false;
      throw new Error("database is locked");
    }
    return db.projects.get(projectPath) ?? "space-1";
  },
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, raw: Record<string, unknown> = {}) => {
    const args = JSON.parse(JSON.stringify(raw));
    const answer = COMMANDS[command];
    // The commands this suite only checks the arguments of echo them.
    return answer ? answer(args) : { command, args };
  }),
}));

const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);

beforeEach(() => {
  __spacesTestReset();
  db.spaces = [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
  ];
  db.active = "space-1";
  db.logins = [
    { spaceId: "space-1", provider: "codex", accountId: "dev" },
    { spaceId: "space-2", provider: "claude", accountId: "studio" },
  ];
  db.projects = new Map([["/code/two", "space-2"]]);
  db.failList = false;
  db.failProject = false;
  vi.mocked(invoke).mockClear();
});

afterEach(() => vi.restoreAllMocks());

/** Lets every pending read answer. */
async function settle() {
  for (let i = 0; i < 5; i += 1) await new Promise((done) => setTimeout(done));
}

describe("the active Space model", () => {
  it("shows every row until the first read, then only the active Space's", async () => {
    expect(inActiveSpace("space-2")).toBe(true);
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    await settle();

    expect(listener).toHaveBeenCalled();
    expect(spacesView()).toMatchObject({
      loaded: true,
      activeSpaceId: "space-1",
      spaces: db.spaces,
      logins: db.logins,
    });
    expect(inActiveSpace("space-1")).toBe(true);
    expect(inActiveSpace("space-2")).toBe(false);
    // A row the database has not saved yet joins the active Space.
    expect(inActiveSpace(undefined)).toBe(true);

    // Once read, another subscriber reads nothing more.
    vi.mocked(invoke).mockClear();
    const again = subscribeSpaces(() => {});
    await settle();
    expect(commands()).toEqual([]);

    // A new read sees what changed since.
    db.active = "space-2";
    await loadSpaces();
    expect(spacesView().activeSpaceId).toBe("space-2");
    stop();
    again();
    listener.mockClear();
    await switchSpace("space-1");
    expect(listener).not.toHaveBeenCalled();
  });

  it("logs a failed read, keeps every row shown, and reads again for the next subscriber", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    db.failList = true;
    const stop = subscribeSpaces(() => {});
    await settle();
    expect(error).toHaveBeenCalledWith(
      "monocode: could not read Spaces",
      expect.any(Error),
    );
    expect(spacesView()).toMatchObject({
      loaded: false,
      spaces: [],
      activeSpaceId: "space-1",
      logins: [],
    });
    expect(inActiveSpace("space-2")).toBe(true);

    db.failList = false;
    const retry = subscribeSpaces(() => {});
    await settle();
    expect(spacesView().loaded).toBe(true);
    stop();
    retry();
  });

  it("switching only saves which Space shows: nothing else is called or stopped", async () => {
    await loadSpaces();
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    vi.mocked(invoke).mockClear();

    await switchSpace("space-2");
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["spaces_settings_set", { patch: { activeSpaceId: "space-2" } }],
    ]);
    expect(spacesView().activeSpaceId).toBe("space-2");
    expect(listener).toHaveBeenCalledTimes(1);

    // Picking the Space already shown saves nothing.
    vi.mocked(invoke).mockClear();
    await switchSpace("space-2");
    expect(commands()).toEqual([]);
    stop();
  });

  it("asks the backend for each project's Space once, and again after a failed ask", async () => {
    await loadSpaces();
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    vi.mocked(invoke).mockClear();

    // Unknown until the answer: it shows.
    expect(projectInActiveSpace("/code/two")).toBe(true);
    expect(projectInActiveSpace("/code/two")).toBe(true);
    await settle();
    expect(commands()).toEqual(["spaces_project_get"]);
    expect(listener).toHaveBeenCalled();
    expect(spacesView().projectSpaces.get("/code/two")).toBe("space-2");
    expect(projectInActiveSpace("/code/two")).toBe(false);
    expect(projectInActiveSpace("/code/one")).toBe(true);
    await settle();
    expect(projectInActiveSpace("/code/one")).toBe(true);

    db.failProject = true;
    vi.mocked(invoke).mockClear();
    projectInActiveSpace("/code/three");
    await settle();
    projectInActiveSpace("/code/three");
    await settle();
    expect(commands()).toEqual(["spaces_project_get", "spaces_project_get"]);
    expect(spacesView().projectSpaces.get("/code/three")).toBe("space-1");
    stop();
  });

  it("gives each Space its login per CLI; Space 1 with none keeps the CLI's own", () => {
    const logins = [
      { spaceId: "space-1", provider: "codex", accountId: "dev" },
      { spaceId: "space-2", provider: "claude", accountId: "studio" },
      { spaceId: "space-3", provider: "codex", accountId: "other" },
    ];
    expect(spaceLoginId(logins, "space-1", "claude")).toBe("default");
    expect(spaceLoginId(logins, "space-1", "codex")).toBe("dev");
    expect(spaceLoginId(logins, "space-2", "claude")).toBe("studio");
    expect(spaceLoginId(logins, "space-2", "codex")).toBeUndefined();
    expect(spaceLoginId(logins, "space-3", "claude")).toBeUndefined();
  });

  it("counts each Space's working, waiting and unread Monos for its badge", () => {
    const monos = [
      { id: "a", spaceId: "space-1" },
      { id: "b", spaceId: "space-2" },
      { id: "c", spaceId: "space-2" },
      { id: "d", spaceId: "space-3" },
      { id: "unsaved" },
    ].map((mono) => ({ ...mono, mascot: "crab", color: "#0f0", projects: [] }));
    const states = new Map<string, MonoState>([
      ["a", { status: "working" }],
      ["b", { status: "working" }],
      ["c", { status: "needs-you" }],
      ["d", { status: "idle" }],
      ["unsaved", { status: "needs-you" }],
    ]);
    const unseen = new Map([
      ["a", 4],
      ["c", 2],
      ["unsaved", 9],
    ]);
    const activity = spaceActivity(monos, states, unseen);
    expect([...activity]).toEqual([
      ["space-1", { working: 1, needsYou: 0, unread: 4 }],
      ["space-2", { working: 1, needsYou: 1, unread: 2 }],
      ["space-3", { working: 0, needsYou: 0, unread: 0 }],
    ]);
    expect([...spaceActivity(monos, new Map(), undefined)]).toEqual([
      ["space-1", { working: 0, needsYou: 0, unread: 0 }],
      ["space-2", { working: 0, needsYou: 0, unread: 0 }],
      ["space-3", { working: 0, needsYou: 0, unread: 0 }],
    ]);

    // News is an unread reply or a Mono waiting, in a Space not shown.
    expect(otherSpaceHasNews(activity, "space-1")).toBe(true);
    expect(otherSpaceHasNews(activity, "space-2")).toBe(true);
    const quiet = new Map([
      ["space-1", { working: 0, needsYou: 3, unread: 3 }],
      ["space-2", { working: 2, needsYou: 0, unread: 0 }],
    ]);
    expect(otherSpaceHasNews(quiet, "space-1")).toBe(false);
    const waiting = new Map([["space-2", { working: 0, needsYou: 1, unread: 0 }]]);
    expect(otherSpaceHasNews(waiting, "space-1")).toBe(true);
    const unread = new Map([["space-2", { working: 0, needsYou: 0, unread: 1 }]]);
    expect(otherSpaceHasNews(unread, "space-1")).toBe(true);
  });

  it("starts over for the next suite after a test reset", async () => {
    await loadSpaces();
    projectInActiveSpace("/code/two");
    await settle();
    const pending = loadSpaces();
    __spacesTestReset();
    expect(spacesView().loaded).toBe(false);
    vi.mocked(invoke).mockClear();
    projectInActiveSpace("/code/two");
    expect(commands()).toEqual(["spaces_project_get"]);
    const fresh = loadSpaces();
    expect(fresh).not.toBe(pending);
    await Promise.all([pending, fresh]);
    await settle();
  });

  it("sends the other Space commands with their arguments", async () => {
    await expect(createSpace("Studio")).resolves.toEqual({
      command: "spaces_create",
      args: { name: "Studio", color: null },
    });
    await expect(createSpace("Studio", "#f00")).resolves.toMatchObject({
      args: { name: "Studio", color: "#f00" },
    });
    await expect(updateSpace("space-2", "Work", null)).resolves.toEqual({
      command: "spaces_update",
      args: { id: "space-2", name: "Work", color: null },
    });
    const login = { spaceId: "space-2", provider: "codex", accountId: "dev" };
    await expect(setSpaceLogin(login)).resolves.toEqual({
      command: "spaces_login_set",
      args: { login },
    });
  });
});
