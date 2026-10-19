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
    if (command === "milestones_add") return { ...SHIP, id: "m-new" };
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

/** The page on APP with `rows`, as the app mounts it. */
async function mount() {
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

async function openRow(id: string) {
  await act(async () => {
    container
      .querySelector(`[data-milestone="${id}"]`)!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function openShip() {
  await mount();
  await openRow("m-ship");
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

async function key(
  element: Element,
  name: string,
  init: KeyboardEventInit = {},
) {
  await act(async () => {
    element.dispatchEvent(
      new KeyboardEvent("keydown", { key: name, bubbles: true, ...init }),
    );
  });
}

const update = (patch: object) => [
  "milestones_update",
  { projectPath: APP, id: "m-ship", patch, by: "user" },
];

async function openAdd(milestones: Milestone[] = [SHIP]) {
  rows = milestones;
  await mount();
  await click("Add milestone");
}

const added = (milestone: object) => [
  "milestones_add",
  { projectPath: APP, milestone: { ...milestone, createdBy: "user" } },
];

it("add sends the trimmed title, the note, the status and the date as user", async () => {
  await openAdd();
  expect(
    container
      .querySelector("[data-milestone-editor]")
      ?.getAttribute("aria-label"),
  ).toBe("New milestone");
  await type("Title", "  Milestone list in the Mono overview  ");
  await type("Note", "Shown when a Mono starts a turn");
  await click("In progress");
  await click("Date");
  await type("Target date", "December 12, 2026");
  await click("Add");

  expect(writes()).toEqual([
    added({
      title: "Milestone list in the Mono overview",
      note: "Shown when a Mono starts a turn",
      status: "in_progress",
      target: "2026-12-12",
    }),
  ]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("a new milestone opens as Planned and Later, and Later is sent as null", async () => {
  await openAdd([]);
  await type("Title", "Someday");
  await key(field("Title"), "Enter");

  expect(writes()).toEqual([
    added({ title: "Someday", note: "", status: "planned", target: null }),
  ]);
});

it("an add with an empty title or a bad date is never sent", async () => {
  await openAdd();
  await type("Title", "   ");
  await click("Add");
  expect(container.textContent).toContain("Add a title");
  await type("Title", "Dated");
  await click("Date");
  await type("Target date", "Feb 30, 2026");
  await key(field("Target date"), "Enter", { metaKey: true });

  expect(writes()).toEqual([]);
  expect(field("Target date").getAttribute("aria-invalid")).toBe("true");
});

it("Escape closes the add card without sending anything", async () => {
  await openAdd();
  await type("Title", "Never mind");
  await key(field("Title"), "Escape");

  expect(writes()).toEqual([]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

// Review n10 and #98 m1: a save that settles after the user moved on must
// not close, or show its error on, the editor open now. Each opening counts:
// another row, the same row again, or a second add card.
const REOPEN = {
  "another row": { start: "m-ship", next: "m-other", label: "Edit Other" },
  "the same row": { start: "m-ship", next: "m-ship", label: "Edit Ship it" },
  "a second add card": { start: null, next: null, label: "New milestone" },
} as const;
it.each(
  (Object.keys(REOPEN) as (keyof typeof REOPEN)[]).flatMap((reopen) =>
    (["resolve", "reject"] as const).map((settle) => [reopen, settle] as const),
  ),
)(
  "a late save leaves the editor then opened on %s alone (%s)",
  async (reopen, settle) => {
    const { start, next, label } = REOPEN[reopen];
    rows = [SHIP, { ...SHIP, id: "m-other", title: "Other", order: 1 }];
    let finish = { resolve: (_: Milestone) => {}, reject: (_: string) => {} };
    const saved = new Promise<Milestone>(
      (resolve, reject) => (finish = { resolve, reject }),
    );
    invoke.mockImplementation(async (command: string) =>
      command === "milestones_update" || command === "milestones_add"
        ? saved
        : { settings: { enabled: true, hiddenStatuses: [] }, milestones: rows },
    );
    const open = (id: string | null) =>
      id === null ? click("Add milestone") : openRow(id);
    await mount();
    await open(start);
    await type("Title", "Saved late");
    await key(field("Title"), "Enter", { metaKey: true });
    await key(field("Title"), "Escape");
    await open(next);
    await type("Note", "Typed, unsaved");

    await act(async () => {
      if (settle === "resolve") finish.resolve(SHIP);
      else finish.reject("invalid_input: title must be 1 to 200 characters");
      await saved.catch(() => undefined);
    });

    const card = container.querySelector("[data-milestone-editor]");
    expect(card?.getAttribute("aria-label")).toBe(label);
    expect(field("Note").value).toBe("Typed, unsaved");
    expect(card?.textContent).not.toContain("invalid_input");
  },
);

// #98 m2: the header button stays visible while the add card is open.
it("Add milestone while the add card is open keeps what was typed", async () => {
  await openAdd();
  await type("Title", "Half typed");
  await click("Add milestone");

  expect(field("Title").value).toBe("Half typed");
  expect(writes()).toEqual([]);
});

const removed = [
  "milestones_remove",
  { projectPath: APP, id: "m-ship", by: "user" },
];

// Frame M·7: Delete sits in the edit card and asks once, inline.
it("Delete asks once inline; Keep sends nothing and returns to the edit card", async () => {
  await openShip();
  await type("Note", "Typed before Delete");
  await click("Delete");

  expect(
    container
      .querySelector("[data-milestone-editor]")
      ?.getAttribute("aria-label"),
  ).toBe("Delete Ship it");
  expect(container.textContent).toContain(
    "Delete “Ship it”? This can’t be undone.",
  );
  expect(writes()).toEqual([]);
  await click("Keep");

  expect(writes()).toEqual([]);
  expect(field("Note").value).toBe("Typed before Delete");
});

it("the confirm's Delete removes the row as user and closes the card", async () => {
  await openShip();
  await click("Delete");
  await click("Delete");

  expect(writes()).toEqual([removed]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("Escape or ⌘↵ in the confirm deletes nothing", async () => {
  await openShip();
  await click("Delete");
  const card = container.querySelector("[data-milestone-editor]")!;
  await key(card, "Enter", { metaKey: true });
  expect(writes()).toEqual([]);
  await key(card, "Escape");

  expect(writes()).toEqual([]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("a refused delete keeps the confirm open with the reason", async () => {
  invoke.mockImplementation(async (command: string) => {
    if (command === "milestones_remove") throw "unknown_milestone";
    return {
      settings: { enabled: true, hiddenStatuses: [] },
      milestones: rows,
    };
  });
  await openShip();
  await click("Delete");
  await click("Delete");

  expect(writes()).toEqual([removed]);
  const card = container.querySelector("[data-milestone-editor]");
  expect(card?.getAttribute("aria-label")).toBe("Delete Ship it");
  expect(card?.textContent).toContain("unknown_milestone");
});

it("the add card has no Delete", async () => {
  await openAdd();
  expect(
    [...container.querySelectorAll("[data-milestone-editor] button")].map(
      (button) => button.textContent?.trim(),
    ),
  ).not.toContain("Delete");
});

it("Enter in the title saves", async () => {
  await openShip();
  await type("Title", "Ship the harness");
  await key(field("Title"), "Enter");

  expect(writes()).toEqual([update({ title: "Ship the harness" })]);
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

it("a second ⌘↵ while the save is in flight sends nothing more", async () => {
  // CI's Node has no Promise.withResolvers.
  let resolve = (_: Milestone) => {};
  const saved = new Promise<Milestone>((done) => (resolve = done));
  invoke.mockImplementation(async (command: string) =>
    command === "milestones_update"
      ? saved
      : { settings: { enabled: true, hiddenStatuses: [] }, milestones: rows },
  );
  await openShip();
  await type("Note", "Once");
  await key(field("Note"), "Enter", { metaKey: true });
  await key(field("Note"), "Enter", { metaKey: true });

  expect(writes()).toEqual([update({ note: "Once" })]);
  await act(async () => resolve(SHIP));
  expect(container.querySelector("[data-milestone-editor]")).toBeNull();
});

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
    await mount();
    await key(container.querySelector('[data-milestone="m-ship"]')!, name);

    expect(field("Title").value).toBe("Ship it");
    expect(field("Target date").value).toBe("Dec 12, 2026");
  },
);
