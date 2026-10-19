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
    if (command === "milestones_add") return { ...SHIP, id: "m-new" };
    if (command === "milestones_update") return SHIP;
    if (command === "milestones_remove") return null;
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

async function openPage() {
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
}

const writes = () =>
  invoke.mock.calls.filter(([command]) => command !== "milestones_list");

function button(name: string) {
  const found = [...container.querySelectorAll("button")].find(
    (element) =>
      element.textContent?.trim() === name ||
      element.getAttribute("aria-label") === name,
  );
  if (!found) throw new Error(`no button ${name}`);
  return found;
}

const field = (label: string) =>
  container.querySelector<HTMLInputElement | HTMLTextAreaElement>(
    `[aria-label="${label}"]`,
  )!;

async function click(element: Element) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function type(label: string, value: string) {
  const element = field(label);
  const prototype = Object.getPrototypeOf(element);
  await act(async () => {
    Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(
      element,
      value,
    );
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

async function editShip() {
  await click(container.querySelector('[data-milestone="m-ship"]')!);
}

it("add sends the trimmed title, the note, the status and the date as user", async () => {
  await openPage();
  await click(button("Add milestone"));
  await type("Title", "  Milestone list in the Mono overview  ");
  await type("Note", "Shown when a Mono starts a turn");
  await click(button("In progress"));
  await click(button("Date"));
  await type("Target date", "December 12, 2026");
  await click(button("Add"));

  expect(writes()).toEqual([
    [
      "milestones_add",
      {
        projectPath: APP,
        milestone: {
          title: "Milestone list in the Mono overview",
          note: "Shown when a Mono starts a turn",
          status: "in_progress",
          target: "2026-12-12",
          createdBy: "user",
        },
      },
    ],
  ]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("add with Later stores no date", async () => {
  await openPage();
  await click(button("Add milestone"));
  await type("Title", "Someday");
  await click(button("Add"));

  expect(writes()).toEqual([
    [
      "milestones_add",
      {
        projectPath: APP,
        milestone: {
          title: "Someday",
          note: "",
          status: "planned",
          target: null,
          createdBy: "user",
        },
      },
    ],
  ]);
});

it("an empty title is never sent", async () => {
  await openPage();
  await click(button("Add milestone"));
  await type("Title", "   ");
  await click(button("Add"));

  expect(writes()).toEqual([]);
  expect(container.textContent).toContain("Add a title");
});

it("an invalid date is refused in the editor and nothing is sent", async () => {
  await openPage();
  await editShip();
  await type("Title", "Ship it now");
  await type("Target date", "Feb 30, 2026");
  await click(button("Save"));

  expect(writes()).toEqual([]);
  expect(field("Target date").getAttribute("aria-invalid")).toBe("true");
  expect(container.textContent).toContain(
    "Use a date like Dec 12, 2026 or 2026-12-12",
  );
});

it("edit sends only the title and note the user changed", async () => {
  await openPage();
  await editShip();
  await type("Title", "Ship the harness");
  await type("Note", "After M-9");
  await click(button("Save"));

  expect(writes()).toEqual([
    [
      "milestones_update",
      {
        projectPath: APP,
        id: "m-ship",
        patch: { title: "Ship the harness", note: "After M-9" },
        by: "user",
      },
    ],
  ]);
});

it("a status change sends only the status", async () => {
  await openPage();
  await editShip();
  await click(button("Done"));
  await click(button("Save"));

  expect(writes()).toEqual([
    [
      "milestones_update",
      { projectPath: APP, id: "m-ship", patch: { status: "done" }, by: "user" },
    ],
  ]);
});

it("Date to Later clears the stored date", async () => {
  await openPage();
  await editShip();
  await click(button("Later"));
  await click(button("Save"));

  expect(writes()).toEqual([
    [
      "milestones_update",
      { projectPath: APP, id: "m-ship", patch: { target: null }, by: "user" },
    ],
  ]);
});

it("Later to Date with an empty field is invalid and does not save", async () => {
  rows = [{ ...SHIP, target: null }];
  await openPage();
  await editShip();
  await click(button("Date"));
  await click(button("Save"));

  expect(writes()).toEqual([]);
  expect(field("Target date").getAttribute("aria-invalid")).toBe("true");
});

it("delete asks once inline; Keep sends nothing and Delete removes as user", async () => {
  await openPage();
  await editShip();
  await click(button("Delete"));
  expect(container.textContent).toContain(
    "Delete “Ship it”? This can’t be undone.",
  );
  await click(button("Keep"));
  expect(writes()).toEqual([]);
  expect(field("Title").value).toBe("Ship it");

  await click(button("Delete"));
  await click(button("Delete"));
  expect(writes()).toEqual([
    ["milestones_remove", { projectPath: APP, id: "m-ship", by: "user" }],
  ]);
});

it("Escape closes the editor without saving", async () => {
  await openPage();
  await editShip();
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
    await openPage();
    await key(container.querySelector('[data-milestone="m-ship"]')!, name);

    expect(field("Title").value).toBe("Ship it");
    expect(field("Target date").value).toBe("Dec 12, 2026");
  },
);
