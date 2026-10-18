// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  MILESTONES_CHANGED,
  subscribe,
  type Milestone,
  type ProjectMilestones,
} from "../model/milestones";
import { MilestonesView } from "./MilestonesView";

const invoke = vi.hoisted(() => vi.fn());
const listeners = vi.hoisted(
  () => new Map<string, (event: { payload: unknown }) => void>(),
);
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (
    event: string,
    handler: (event: { payload: unknown }) => void,
  ) => {
    listeners.set(event, handler);
    return () => listeners.delete(event);
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: async () => false,
    onResized: async () => () => {},
  }),
}));

const APP = "/work/app";
const SITE = "/work/site";

let container: HTMLDivElement;
let root: Root;
let store: Record<string, ProjectMilestones>;

function milestone(
  fields: Partial<Milestone> & Pick<Milestone, "id">,
): Milestone {
  return {
    projectPath: APP,
    title: fields.id,
    note: "",
    status: "planned",
    target: null,
    order: 0,
    createdBy: "user",
    createdAt: 0,
    updatedAt: 0,
    ...fields,
  };
}

function project(milestones: Milestone[], settings = {}): ProjectMilestones {
  return {
    settings: { enabled: true, hiddenStatuses: [], ...settings },
    milestones,
  };
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  });
  store = {};
  invoke.mockReset();
  invoke.mockImplementation(
    async (command: string, args: { projectPath: string }) => {
      if (command !== "milestones_list")
        throw new Error(`unexpected ${command}`);
      return structuredClone(store[args.projectPath] ?? project([]));
    },
  );
  listeners.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function open(cwd = APP) {
  await act(async () =>
    root.render(
      createElement(MilestonesView, {
        cwd,
        recents: [
          { path: SITE, openedAt: 2 },
          { path: `${APP}/`, openedAt: 1 },
        ],
        sessions: [],
        onClose: vi.fn(),
      }),
    ),
  );
}

const titles = () =>
  [...container.querySelectorAll("[data-milestone]")].map(
    (row) => row.querySelector(".truncate")?.textContent,
  );

it("shows the empty state for a project with no milestones", async () => {
  store[SITE] = project([milestone({ id: "site-goal", projectPath: SITE })]);
  await open();

  expect(container.querySelector("h2")?.textContent).toBe("app");
  expect(container.textContent).toContain("No milestones yet");
  expect(container.textContent).toContain("No milestones for app yet");
  expect(container.querySelector("[data-milestone]")).toBeNull();
});

it("opens on the cwd project and shows only its rows", async () => {
  store[APP] = project([
    milestone({ id: "Ship search", status: "in_progress" }),
    milestone({ id: "Dark mode", order: 1 }),
  ]);
  store[SITE] = project([
    milestone({ id: "Site redesign", projectPath: SITE }),
  ]);
  await open(`${APP}/`);

  expect(invoke).toHaveBeenCalledWith("milestones_list", { projectPath: APP });
  expect(container.querySelector("h2")?.textContent).toBe("app");
  expect(container.textContent).toContain("2 milestones · 1 in progress");
  expect(titles()).toEqual(["Ship search", "Dark mode"]);
});

it("reads the settings: hidden statuses leave the timeline and an off project says Off", async () => {
  store[APP] = project(
    [
      milestone({ id: "Shipped", status: "done" }),
      milestone({ id: "Next", order: 1 }),
    ],
    { hiddenStatuses: ["done"] },
  );
  store[SITE] = project([], { enabled: false });
  await open();

  expect(titles()).toEqual(["Next"]);
  expect(container.textContent).toContain("2 milestones · Done hidden");
  const site = [...container.querySelectorAll("aside button")].find((button) =>
    button.textContent?.startsWith("site"),
  );
  expect(site?.textContent).toBe("siteOff");
});

it("refreshes on a milestones-changed event and marks an agent's new row for a short time", async () => {
  store[APP] = project([milestone({ id: "Ship search" })]);
  await open();
  expect(titles()).toEqual(["Ship search"]);

  vi.useFakeTimers();
  store[APP] = project([
    milestone({ id: "Ship search" }),
    milestone({ id: "Voice notes", order: 1, createdBy: "mono:m-1" }),
  ]);
  await act(async () =>
    listeners.get(MILESTONES_CHANGED)!({ payload: { projectPath: APP } }),
  );

  expect(titles()).toEqual(["Ship search", "Voice notes"]);
  const added = container.querySelector('[data-milestone="Voice notes"]');
  expect(added?.textContent).toContain("added by a Mono");
  expect(added?.textContent).toContain("New");

  await act(async () => vi.advanceTimersByTime(4000));
  expect(
    container.querySelector('[data-milestone="Voice notes"]')?.textContent,
  ).not.toContain("New");
});

const changed = (projectPath: string) =>
  act(async () =>
    listeners.get(MILESTONES_CHANGED)!({ payload: { projectPath } }),
  );

function gate() {
  let open!: () => void;
  const opened = new Promise<void>((resolve) => (open = resolve));
  return { opened, open: () => act(async () => open()) };
}

it("shows the open project's load error even when another project loads fine, until it loads", async () => {
  invoke.mockImplementation(
    async (_command: string, { projectPath }: { projectPath: string }) => {
      if (projectPath === SITE) throw new Error("database is locked");
      return project([milestone({ id: "Ship search" })]);
    },
  );
  await open(SITE);
  expect(container.querySelector("main")?.textContent).toBe(
    "database is locked",
  );

  invoke.mockResolvedValue(
    project([milestone({ id: "Site goal", projectPath: SITE })]),
  );
  await changed(SITE);
  expect(titles()).toEqual(["Site goal"]);
  expect(container.textContent).not.toContain("database is locked");
});

it("never shows another project's load error over the open project", async () => {
  const siteFails = gate();
  invoke.mockImplementation(
    async (_command: string, { projectPath }: { projectPath: string }) => {
      if (projectPath !== SITE)
        return project([milestone({ id: "Ship search" })]);
      await siteFails.opened;
      throw new Error("site failed");
    },
  );
  await open();
  await siteFails.open();
  expect(titles()).toEqual(["Ship search"]);
  expect(container.textContent).not.toContain("site failed");
});

it("never lets a slow older list overwrite a newer one", async () => {
  const slow = gate();
  const lists = [[], ["old"], ["old", "new"]];
  invoke.mockImplementation(
    async (_command: string, { projectPath }: { projectPath: string }) => {
      if (projectPath !== APP) return project([]);
      const ids = lists.shift()!;
      if (ids.length === 1) await slow.opened;
      return project(ids.map((id, order) => milestone({ id, order })));
    },
  );
  await open();
  await changed(APP);
  await changed(APP);
  await slow.open();
  expect(titles()).toEqual(["old", "new"]);
});

it("ignores a late failure from an older request: the newer rows stay and no error shows", async () => {
  const slow = gate();
  let call = 0;
  invoke.mockImplementation(
    async (_command: string, { projectPath }: { projectPath: string }) => {
      if (projectPath !== APP) return project([]);
      call += 1;
      if (call === 2) {
        await slow.opened;
        throw new Error("stale failure");
      }
      return project(call === 1 ? [] : [milestone({ id: "newer" })]);
    },
  );
  await open();
  await changed(APP);
  await changed(APP);
  await slow.open();
  expect(titles()).toEqual(["newer"]);
  expect(container.textContent).not.toContain("stale failure");
});

it("marks only rows an agent added after the first list", async () => {
  store[APP] = project([milestone({ id: "Found", createdBy: "mono:m-1" })]);
  await open();
  store[APP] = project([
    milestone({ id: "Found", createdBy: "mono:m-1" }),
    milestone({ id: "Mine", order: 1 }),
  ]);
  await changed(APP);
  expect(titles()).toEqual(["Found", "Mine"]);
  expect(container.textContent).not.toContain("New");
});

it("unmount removes the listener and the pending highlight timer", async () => {
  await open();
  vi.useFakeTimers();
  store[APP] = project([
    milestone({ id: "Voice notes", createdBy: "mono:m-1" }),
  ]);
  await changed(APP);
  expect(vi.getTimerCount()).toBe(1);

  await act(async () => root.unmount());
  expect(listeners.has(MILESTONES_CHANGED)).toBe(false);
  expect(vi.getTimerCount()).toBe(0);
  root = createRoot(container);
});

it("subscribe disposed before listen resolves still removes the listener", async () => {
  const stop = subscribe(() => undefined);
  stop();
  await act(async () => undefined);
  expect(listeners.has(MILESTONES_CHANGED)).toBe(false);
});
