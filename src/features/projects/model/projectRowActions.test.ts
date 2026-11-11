import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  readProjectRowFacts,
  removeProjectFromActiveSpace,
  rowOffersRemove,
} from "./projectRowActions";
import { setOpenSessionsSource, SPACE_UNREAD_REFUSAL } from "./projectDeleteCheck";
import { __spacesTestReset } from "../../spaces/model/activeSpace";
import type { Session } from "../../sessions/model/session";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

const P = "/work/vibtra";
const T7 = {
  code: "monoHeld",
  project: "vibtra",
  monos: ["Vibtra Orchestrator"],
  message: "Vibtra Orchestrator uses vibtra. Take it off that Mono first.",
};
let replies: Record<string, unknown>;

function mockStorage() {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
  });
}

beforeEach(() => {
  mockStorage();
  __spacesTestReset();
  setOpenSessionsSource(() => []);
  replies = {
    spaces_list: [{ id: "space-2", name: "Space 2", color: null, createdAt: 1 }],
    spaces_settings_get: { activeSpaceId: "space-2", routeGrantTtlMs: 0 },
    spaces_logins_list: [],
    spaces_project_list: {
      spaceId: "space-2",
      projects: [],
      monoHeld: [],
      noRowIsHere: true,
      assignedElsewhere: [],
    },
    spaces_project_known: [],
    spaces_project_remove: true,
    session_list_by_project: [],
  };
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (!(command in replies)) return null;
    const reply = replies[command];
    if (reply instanceof Rejection) throw reply.value;
    return reply;
  });
});

class Rejection {
  constructor(readonly value: unknown) {}
}

const removeCalls = () =>
  vi.mocked(invoke).mock.calls.filter(([name]) => name === "spaces_project_remove");

describe("rowOffersRemove", () => {
  it.each([
    ["in another Space", [{ path: P, spaces: ["space-2", "space-1"] }], [], true],
    ["only in this Space", [{ path: P, spaces: ["space-2"] }], [], false],
    ["known to no Space", [], [], false],
    ["only here, but a Mono holds it", [{ path: P, spaces: ["space-2"] }], [P], true],
    ["compared in key form", [{ path: `${P}/`, spaces: ["space-1"] }], [], true],
  ])("%s", (_, known, monoHeld, expected) => {
    expect(rowOffersRemove(P, "space-2", known, monoHeld)).toBe(expected);
  });
});

describe("readProjectRowFacts", () => {
  it("reads whether the row is shared", async () => {
    replies.spaces_project_known = [{ path: P, spaces: ["space-1", "space-2"] }];
    await expect(readProjectRowFacts(P)).resolves.toEqual({
      offersRemove: true,
      machine: undefined,
    });
  });

  it("fails open when a read fails: the backend answers the Remove", async () => {
    replies.spaces_project_list = new Rejection("no database");
    await expect(readProjectRowFacts(P)).resolves.toMatchObject({
      offersRemove: true,
    });
  });

  it("names a remote project's machine", async () => {
    replies.remote_machines = [
      { id: "m", name: "example-machine", environmentId: "env-facts" },
    ];
    await expect(
      readProjectRowFacts("remote://env-facts/code/repo"),
    ).resolves.toMatchObject({ machine: "example-machine" });
  });
});

describe("removeProjectFromActiveSpace", () => {
  const queued = {
    id: "own",
    cwd: P,
    busy: false,
    queuedMessages: [{ id: "q" }],
  } as unknown as Session;

  it("removes through the backend", async () => {
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      removed: true,
    });
    expect(removeCalls()).toEqual([["spaces_project_remove", { projectPath: P }]]);
  });

  it("reports a project that was not in the Space", async () => {
    replies.spaces_project_remove = false;
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      removed: false,
    });
  });

  it("shows the backend's refusal as given", async () => {
    replies.spaces_project_remove = new Rejection(T7);
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      refusal: T7.message,
    });
  });

  it("refuses the window's own queued message before any write", async () => {
    setOpenSessionsSource(() => [queued]);
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      refusal: "A session is running in vibtra here. Stop it first.",
    });
    expect(removeCalls()).toEqual([]);
  });

  it("lets a Mono's refusal come before the window's running one", async () => {
    setOpenSessionsSource(() => [queued]);
    replies.spaces_project_list = {
      spaceId: "space-2",
      projects: [],
      monoHeld: [P],
      noRowIsHere: true,
      assignedElsewhere: [],
    };
    replies.spaces_project_remove = new Rejection(T7);
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      refusal: T7.message,
    });
  });

  it("still guards the window when the list cannot be read", async () => {
    setOpenSessionsSource(() => [queued]);
    replies.spaces_project_list = new Rejection("no database");
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      refusal: "A session is running in vibtra here. Stop it first.",
    });
    expect(removeCalls()).toEqual([]);
  });

  it("refuses before any write when the window's Space cannot be read", async () => {
    replies.spaces_settings_get = new Rejection("no database");
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      refusal: SPACE_UNREAD_REFUSAL,
    });
    expect(removeCalls()).toEqual([]);
  });

  it("counts only this Space's running sessions", async () => {
    setOpenSessionsSource(() => [{ ...queued, busy: true, queuedMessages: [] }]);
    replies.session_list_by_project = [{ id: "own", spaceId: "space-1" }];
    await expect(removeProjectFromActiveSpace(P)).resolves.toEqual({
      removed: true,
    });
  });
});
