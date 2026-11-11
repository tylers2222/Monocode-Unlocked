import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MonoState } from "../../monos/model/mono";
import {
  __spacesTestReset,
  inActiveSpace,
  loadSpaces,
  otherSpaceHasNews,
  refreshProjectList,
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
  /** Each read of the Space list waits for this. */
  listGate: Promise.resolve(),
  /** The next list reads fail. */
  failProjects: false,
  /** Each list read waits for the next of these, in call order. */
  listGates: [] as Promise<void>[],
};

const listReply = () => ({
  spaceId: db.active,
  projects: [...db.projects]
    .filter(([, space]) => space === db.active)
    .map(([path]) => ({ path, assigned: true, linked: false, monos: [] })),
  monoHeld: [],
  noRowIsHere: db.active === "space-1",
  assignedElsewhere: [...db.projects]
    .filter(([, space]) => space !== db.active)
    .map(([path]) => path),
});

const events = vi.hoisted(() => new Map<string, () => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: () => void) => {
    events.set(name, handler);
    return () => events.delete(name);
  }),
}));

const settings = () => ({ activeSpaceId: db.active, routeGrantTtlMs: 21_600_000 });
type Args = { patch: { activeSpaceId?: string } };
const COMMANDS: Record<string, (args: Args) => unknown> = {
  spaces_list: async () => {
    await db.listGate;
    if (db.failList) throw new Error("database is locked");
    return structuredClone(db.spaces);
  },
  spaces_settings_get: settings,
  spaces_settings_set: ({ patch }) => {
    db.active = patch.activeSpaceId ?? db.active;
    return settings();
  },
  spaces_logins_list: () => structuredClone(db.logins),
  spaces_project_list: async () => {
    // The reply is read when the backend answers, for the Space it had then.
    const answer = listReply();
    await db.listGates.shift();
    if (db.failProjects) throw new Error("database is locked");
    return answer;
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
  db.listGate = Promise.resolve();
  db.failProjects = false;
  db.listGates = [];
  events.clear();
  vi.mocked(invoke).mockClear();
});

afterEach(() => vi.restoreAllMocks());

/** Lets every pending read answer. */
async function settle() {
  for (let i = 0; i < 5; i += 1) await new Promise((done) => setTimeout(done));
}

describe("the active Space model", () => {
  it("shows every row until a read succeeds, then only the active Space's", async () => {
    db.active = "space-2";
    expect(inActiveSpace("space-1")).toBe(true);
    expect(inActiveSpace("space-2")).toBe(true);
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    await settle();

    expect(listener).toHaveBeenCalled();
    expect(spacesView()).toMatchObject({
      loaded: true,
      activeSpaceId: "space-2",
      spaces: db.spaces,
      logins: db.logins,
    });
    expect(inActiveSpace("space-2")).toBe(true);
    expect(inActiveSpace("space-1")).toBe(false);
    // A row the database has not saved yet joins the active Space.
    expect(inActiveSpace(undefined)).toBe(true);

    // Once read, another subscriber reads nothing more.
    vi.mocked(invoke).mockClear();
    const again = subscribeSpaces(() => {});
    await settle();
    expect(commands()).toEqual([]);

    // A new read sees what changed since.
    db.active = "space-1";
    await loadSpaces();
    expect(spacesView().activeSpaceId).toBe("space-1");
    stop();
    again();
    listener.mockClear();
    await switchSpace("space-2");
    expect(listener).not.toHaveBeenCalled();
  });

  it("logs a failed read, hides no row, and reads again for the next subscriber", async () => {
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
    expect(inActiveSpace("space-1")).toBe(true);
    expect(inActiveSpace("space-2")).toBe(true);

    db.failList = false;
    const retry = subscribeSpaces(() => {});
    await settle();
    expect(spacesView().loaded).toBe(true);
    stop();
    retry();
  });

  it("switching saves which Space shows and reads its projects: nothing is stopped", async () => {
    await loadSpaces();
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    vi.mocked(invoke).mockClear();

    await switchSpace("space-2");
    expect(vi.mocked(invoke).mock.calls.map(([command, args]) => [command, args])).toEqual([
      ["spaces_settings_set", { patch: { activeSpaceId: "space-2" } }],
      ["spaces_project_list", undefined],
    ]);
    expect(spacesView().activeSpaceId).toBe("space-2");
    expect(listener).toHaveBeenCalledTimes(1);

    // Picking the Space already shown saves nothing.
    vi.mocked(invoke).mockClear();
    await switchSpace("space-2");
    expect(commands()).toEqual([]);
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

  it("never lets a read undo a switch saved while it was reading", async () => {
    await loadSpaces();
    let release = () => {};
    db.listGate = new Promise((done) => (release = done));
    const reading = loadSpaces();
    await settle();
    await switchSpace("space-2");
    expect(db.active).toBe("space-2");
    release();
    await reading;
    // The read began with Space 1 saved; the switch after it stays.
    expect(spacesView().activeSpaceId).toBe("space-2");
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

describe("the Space's project list (T13, T14)", () => {
  /** A list read that answers when released. */
  const gate = () => {
    let open = () => {};
    db.listGates.push(new Promise<void>((done) => (open = done)));
    return () => open();
  };
  const listPaths = () => {
    const { projectList } = spacesView();
    return typeof projectList === "string"
      ? projectList
      : projectList.projects.map((item) => item.path);
  };

  it("is loading until the first answer, with the Spaces, in one publish", async () => {
    expect(spacesView().projectList).toBe("loading");
    db.active = "space-2";
    const listener = vi.fn();
    const stop = subscribeSpaces(listener);
    await settle();
    expect(listPaths()).toEqual(["/code/two"]);
    expect(spacesView()).toMatchObject({ loaded: true, activeSpaceId: "space-2" });
    expect(listener).toHaveBeenCalledTimes(1);
    stop();
  });

  it("T13: a Space switch refetches and shows Space and list together", async () => {
    await loadSpaces();
    const seen: [string, unknown][] = [];
    const stop = subscribeSpaces(() =>
      seen.push([spacesView().activeSpaceId, listPaths()]),
    );
    const release = gate();
    const switching = switchSpace("space-2");
    await settle();
    expect(spacesView().activeSpaceId).toBe("space-1");
    release();
    await switching;
    expect(seen).toEqual([["space-2", ["/code/two"]]]);
    stop();
  });

  it("T13: the event in any window refetches, without a reload", async () => {
    await loadSpaces();
    const stop = subscribeSpaces(() => {});
    await settle();
    db.active = "space-2";
    await switchSpace("space-2");
    expect(listPaths()).toEqual(["/code/two"]);
    db.projects.set("/code/new", "space-2");
    events.get("spaces:projects-changed")?.();
    await settle();
    expect(listPaths()).toEqual(["/code/two", "/code/new"]);
    stop();
  });

  it("T13: the event and a switch reload recents and archive", async () => {
    const seen: string[] = [];
    const target = new EventTarget();
    vi.stubGlobal("window", target);
    target.addEventListener("monocode:project-paths-changed", () => seen.push("recents"));
    target.addEventListener("monocode:archived-projects-changed", () => seen.push("archive"));
    await loadSpaces();
    const stop = subscribeSpaces(() => {});
    await settle();
    events.get("spaces:projects-changed")?.();
    expect(seen).toEqual(["recents", "archive"]);
    await settle();
    await switchSpace("space-2");
    expect(seen).toEqual(["recents", "archive", "recents", "archive"]);
    stop();
    vi.unstubAllGlobals();
  });

  it("T13: drops a reply older than the latest request", async () => {
    await loadSpaces();
    const first = gate();
    const second = gate();
    const older = refreshProjectList();
    db.projects.set("/code/one", "space-1");
    const newer = refreshProjectList();
    second();
    await newer;
    const answer = listPaths();
    first();
    await older;
    expect(listPaths()).toEqual(answer);
    expect(answer).toEqual(["/code/one"]);
  });

  it("T13: a slow first switch never shows over a later one", async () => {
    await loadSpaces();
    db.spaces.push({ id: "space-3", name: "Space 3", color: null, createdAt: 3 });
    db.projects.set("/code/three", "space-3");
    const toTwo = gate();
    const toThree = gate();
    const first = switchSpace("space-2");
    await settle();
    const second = switchSpace("space-3");
    await settle();
    toThree();
    await second;
    expect(spacesView().activeSpaceId).toBe("space-3");
    toTwo();
    await first;
    expect(spacesView().activeSpaceId).toBe("space-3");
    expect(listPaths()).toEqual(["/code/three"]);
  });

  it("T13: drops a reply for a Space the window does not want", async () => {
    await loadSpaces();
    const before = spacesView();
    // Another Space answers, as a read that crossed a switch would.
    db.active = "space-2";
    await refreshProjectList();
    expect(spacesView()).toBe(before);
  });

  it("T14, I8: a failed read shows every project, and the next subscriber retries", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    db.failProjects = true;
    await loadSpaces();
    expect(spacesView()).toMatchObject({ loaded: true, projectList: "failed" });
    expect(error).toHaveBeenCalledWith(
      "monocode: could not read the Space's projects",
      expect.any(Error),
    );
    db.failProjects = false;
    const stop = subscribeSpaces(() => {});
    await settle();
    expect(listPaths()).toEqual([]);
    stop();
  });

  it("T14: a good list falls back to failed when a later read fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await loadSpaces();
    db.failProjects = true;
    await refreshProjectList();
    expect(spacesView().projectList).toBe("failed");
  });

  it("T14: a failed Spaces read also shows every project", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    db.failList = true;
    await loadSpaces();
    expect(spacesView()).toMatchObject({ loaded: false, projectList: "failed" });
  });

  it("listens once, and warns when events cannot be heard", async () => {
    const stopA = subscribeSpaces(() => {});
    const stopB = subscribeSpaces(() => {});
    await settle();
    expect(vi.mocked(listen)).toHaveBeenCalledTimes(1);
    stopA();
    stopB();
    __spacesTestReset();
    events.clear();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const failure = new Error("no window");
    vi.mocked(listen).mockRejectedValueOnce(failure);
    const stop = subscribeSpaces(() => {});
    await settle();
    expect(warn).toHaveBeenCalledWith("monocode: no project change events", failure);
    expect(events.has("spaces:projects-changed")).toBe(false);
    // The failed listen is not kept: the next subscriber listens again.
    const again = subscribeSpaces(() => {});
    await settle();
    expect(events.has("spaces:projects-changed")).toBe(true);
    again();
    stop();
  });

  it("T13: an event before the first answer reads no list", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    db.failList = true;
    const stop = subscribeSpaces(() => {});
    await settle();
    vi.mocked(invoke).mockClear();
    events.get("spaces:projects-changed")?.();
    await settle();
    expect(commands()).not.toContain("spaces_project_list");
    stop();
  });

  it("T13: a switch saved during a read can be switched back at once", async () => {
    await loadSpaces();
    let release = () => {};
    db.listGate = new Promise((done) => (release = done));
    const reading = loadSpaces();
    await settle();
    await switchSpace("space-2");
    release();
    await reading;
    await settle();
    // The read asks for the Space the switch left, not the one it began with.
    await switchSpace("space-1");
    expect(db.active).toBe("space-1");
    expect(spacesView().activeSpaceId).toBe("space-1");
  });

  it("T13: a read that ends while a switch's list is pending keeps the switch", async () => {
    await loadSpaces();
    let release = () => {};
    db.listGate = new Promise((done) => (release = done));
    const reading = loadSpaces();
    await settle();
    const listed = gate();
    const switching = switchSpace("space-2");
    await settle();
    release();
    await reading;
    listed();
    await switching;
    await settle();
    expect(spacesView().activeSpaceId).toBe("space-2");
    expect(listPaths()).toEqual(["/code/two"]);
  });
});
