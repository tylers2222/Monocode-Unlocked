// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Milestone } from "../model/milestones";
import { MilestonesView } from "./MilestonesView";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => undefined,
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: async () => false,
    onResized: async () => () => {},
  }),
}));

const APP = "/work/app";
const SHIP: Milestone = {
  id: "m-ship",
  projectPath: APP,
  title: "Ship it",
  note: "Soon",
  status: "planned",
  target: "2026-12-12",
  order: 0,
  createdBy: "user",
  createdAt: 0,
  updatedAt: 0,
};

let container: HTMLDivElement;
let root: Root;
let rows: Milestone[];

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  });
  rows = [SHIP];
  invoke.mockReset();
  invoke.mockImplementation(async (command: string) => {
    if (command === "milestones_list")
      return structuredClone({
        settings: { enabled: true, hiddenStatuses: [] },
        milestones: rows,
      });
    if (command === "milestones_update") return SHIP;
    throw new Error(`unexpected ${command}`);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function openShip() {
  await act(async () =>
    root.render(
      createElement(MilestonesView, {
        cwd: APP,
        recents: [{ path: APP, openedAt: 1 }],
        sessions: [],
        onClose: vi.fn(),
      }),
    ),
  );
  await act(async () => {
    container
      .querySelector('[data-milestone="m-ship"]')!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

const writes = () =>
  invoke.mock.calls.filter(([command]) => command !== "milestones_list");

const field = (label: string) =>
  container.querySelector<HTMLInputElement | HTMLTextAreaElement>(
    `[aria-label="${label}"]`,
  )!;

async function click(name: string) {
  const button = [...container.querySelectorAll("button")].find(
    (element) => element.textContent?.trim() === name,
  );
  if (!button) throw new Error(`no button ${name}`);
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function type(label: string, value: string) {
  const element = field(label);
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      Object.getPrototypeOf(element),
      "value",
    )!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function key(element: Element, name: string) {
  await act(async () => {
    element.dispatchEvent(
      new KeyboardEvent("keydown", { key: name, bubbles: true }),
    );
  });
}

const update = (patch: object) => [
  "milestones_update",
  { projectPath: APP, id: "m-ship", patch, by: "user" },
];

it("edit sends only the title and note the user changed, as user", async () => {
  await openShip();
  await type("Title", "  Ship the harness ");
  await type("Note", "After M-9");
  await click("Save");

  expect(writes()).toEqual([
    update({ title: "Ship the harness", note: "After M-9" }),
  ]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("a status change sends only the status", async () => {
  await openShip();
  await click("Done");
  await click("Save");

  expect(writes()).toEqual([update({ status: "done" })]);
});

it("a typed date is sent as YYYY-MM-DD", async () => {
  await openShip();
  await type("Target date", "january 15, 2027");
  await click("Save");

  expect(writes()).toEqual([update({ target: "2027-01-15" })]);
});

it("Date to Later clears the stored date", async () => {
  await openShip();
  await click("Later");
  await click("Save");

  expect(writes()).toEqual([update({ target: null })]);
});

it("Later to Date with an empty field is invalid and sends nothing", async () => {
  rows = [{ ...SHIP, target: null }];
  await openShip();
  await click("Date");
  await click("Save");

  expect(writes()).toEqual([]);
  expect(field("Target date").getAttribute("aria-invalid")).toBe("true");
});

it("an invalid date is refused in the editor and nothing is sent", async () => {
  await openShip();
  await type("Title", "Ship it now");
  await type("Target date", "Feb 30, 2026");
  await click("Save");

  expect(writes()).toEqual([]);
  expect(field("Target date").getAttribute("aria-invalid")).toBe("true");
  expect(container.textContent).toContain(
    "Use a date like Dec 12, 2026 or 2026-12-12",
  );
});

it("an empty title is never sent", async () => {
  await openShip();
  await type("Title", "   ");
  await click("Save");

  expect(writes()).toEqual([]);
  expect(container.textContent).toContain("Add a title");
});

it("Escape closes the editor without saving", async () => {
  await openShip();
  await type("Title", "Unsaved");
  await key(field("Title"), "Escape");

  expect(writes()).toEqual([]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
  expect(
    container.querySelector('[data-milestone="m-ship"]')?.textContent,
  ).toContain("Ship it");
});

it.each(["Enter", " "])(
  "%j on a focused row opens its editor",
  async (name) => {
    await act(async () =>
      root.render(
        createElement(MilestonesView, {
          cwd: APP,
          recents: [{ path: APP, openedAt: 1 }],
          sessions: [],
          onClose: vi.fn(),
        }),
      ),
    );
    await key(container.querySelector('[data-milestone="m-ship"]')!, name);

    expect(field("Title").value).toBe("Ship it");
    expect(field("Target date").value).toBe("Dec 12, 2026");
  },
);
