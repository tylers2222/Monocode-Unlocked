// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { setHomeDir } from "../../../shared/lib/paths";
import { __spacesTestReset, loadSpaces } from "../../spaces/model/activeSpace";
import {
  __knownProjectsTestReset,
  loadKnownProjects,
} from "../../spaces/model/knownProjects";
import { ADD_FROM_SPACE_FLYOUT, AddFromSpaceEntry } from "./AddFromSpaceEntry";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

let root: Root;
let container: HTMLDivElement;
let known: { path: string; spaces: string[] }[];
let add: (input: unknown) => Promise<unknown>;
const onAdded = vi.fn();

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  setHomeDir("/home/me");
  __spacesTestReset();
  __knownProjectsTestReset();
  onAdded.mockReset();
  known = [{ path: "/code/example-app", spaces: ["space-1"] }];
  add = async () => "linked";
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "spaces_list")
      return [
        { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
        { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
      ];
    if (command === "spaces_settings_get")
      return { activeSpaceId: "space-2", routeGrantTtlMs: 0 };
    if (command === "spaces_logins_list") return [];
    if (command === "spaces_project_known") return known;
    if (command === "spaces_project_add") return add(input);
    return null;
  });
  await loadSpaces();
  await loadKnownProjects();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  setHomeDir(undefined);
  vi.unstubAllGlobals();
});

async function render() {
  await act(async () =>
    root.render(
      createElement(AddFromSpaceEntry, {
        className: "",
        iconClassName: "",
        onAdded,
      }),
    ),
  );
}

function button(text: string) {
  return [...document.querySelectorAll("button")].find((item) =>
    item.textContent?.includes(text),
  );
}

async function openAndPick() {
  await act(async () => button("Add from another Space…")!.click());
  await act(async () => button("example-app")!.click());
}

it("hides the entry when no other Space has anything to add", async () => {
  known = [{ path: "/code/here", spaces: ["space-2"] }];
  await loadKnownProjects();
  await render();
  expect(button("Add from another Space…")).toBeUndefined();
});

it("hides the entry when every other project is archived", async () => {
  localStorage.setItem(
    "monocode.archivedProjects",
    JSON.stringify([{ path: "/code/example-app", archivedAt: 1 }]),
  );
  await render();
  expect(button("Add from another Space…")).toBeUndefined();
});

it("hides the entry when the read fails", async () => {
  const reading = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "spaces_project_known") throw "no database";
    return reading(command, input);
  });
  await loadKnownProjects();
  await render();
  expect(button("Add from another Space…")).toBeUndefined();
});

it("lists the project with its Space, and says nothing is copied", async () => {
  await render();
  await act(async () => button("Add from another Space…")!.click());
  const row = button("example-app")!;
  expect(row.textContent).toContain("Space 1");
  expect(document.body.textContent).toContain("Adds it to Space 2’s list.");
  expect(document.body.textContent).toContain("Nothing is copied.");
});

it("adds the project as a known one, and finishes", async () => {
  const calls: unknown[] = [];
  add = async (input) => {
    calls.push(input);
    return "linked";
  };
  await render();
  await openAndPick();
  expect(calls).toEqual([
    { projectPath: "/code/example-app", knownToWindow: true },
  ]);
  expect(onAdded).toHaveBeenCalledOnce();
});

it("keeps the picker's row ignored by the menu it hangs off", async () => {
  await render();
  await act(async () => button("Add from another Space…")!.click());
  expect(document.querySelector(ADD_FROM_SPACE_FLYOUT)).not.toBeNull();
});

// One case per reason spaces_project_add can refuse with: key_of (workspace,
// not a project), shown_space (no Space), and a failed read or write.
it.each([
  [
    "the workspace",
    {
      code: "workspace",
      project: "example-app",
      message: "This folder is part of the MonoCode workspace, so it stays.",
    },
    "This folder is part of the MonoCode workspace, so it can't be added to a Space.",
  ],
  [
    "not a project",
    { code: "failed", message: "Not a project: ~" },
    "Not a project: ~, so it can't be added to Space 2.",
  ],
  [
    "no Space",
    { code: "failed", message: "This window has no Space yet." },
    "This window has no Space yet, so it can't be added to Space 2.",
  ],
  [
    "a failed write",
    { code: "failed", message: "database is locked" },
    "database is locked, so it can't be added to Space 2.",
  ],
])("words a refusal for %s as an Add, and stays open", async (_reason, refusal, text) => {
  add = () => Promise.reject(refusal);
  await render();
  await openAndPick();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(text);
  expect(document.querySelector(ADD_FROM_SPACE_FLYOUT)).not.toBeNull();
  expect(onAdded).not.toHaveBeenCalled();
});

it("drops an earlier refusal when the list opens again", async () => {
  add = () => Promise.reject({ code: "failed", message: "database is locked" });
  await render();
  await openAndPick();
  expect(document.querySelector('[role="alert"]')).not.toBeNull();
  await act(async () => button("Add from another Space…")!.click());
  expect(document.querySelector(ADD_FROM_SPACE_FLYOUT)).toBeNull();
  await act(async () => button("Add from another Space…")!.click());
  expect(document.querySelector(ADD_FROM_SPACE_FLYOUT)).not.toBeNull();
  expect(document.querySelector('[role="alert"]')).toBeNull();
});

it("shows nothing until the Spaces are read", async () => {
  __spacesTestReset();
  const reading = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation((command, input) =>
    command === "spaces_list" ? new Promise(() => undefined) : reading(command, input),
  );
  // Before the read, the window's Space is Space 1: this would be addable.
  known = [{ path: "/code/elsewhere", spaces: ["space-2"] }];
  await loadKnownProjects();
  await render();
  expect(button("Add from another Space…")).toBeUndefined();
});

it("appears when another Space gets a project later", async () => {
  known = [];
  await loadKnownProjects();
  await render();
  expect(button("Add from another Space…")).toBeUndefined();
  known = [{ path: "/code/example-app", spaces: ["space-1"] }];
  await act(async () => loadKnownProjects());
  expect(button("Add from another Space…")).toBeDefined();
});

it("tags each project with its own Space, or the id of a Space it cannot name", async () => {
  known = [
    { path: "/code/example-app", spaces: ["space-1"] },
    { path: "/code/other-app", spaces: ["space-9"] },
  ];
  await loadKnownProjects();
  await render();
  await act(async () => button("Add from another Space…")!.click());
  expect(button("example-app")!.textContent).toContain("Space 1");
  expect(button("other-app")!.textContent).toContain("space-9");
  expect(button("other-app")!.textContent).not.toContain("Space 1");
});

it("is a plain button in a footer, and a menu item in a menu", async () => {
  await render();
  expect(button("Add from another Space…")!.getAttribute("role")).toBeNull();
  await act(async () =>
    root.render(
      createElement(AddFromSpaceEntry, {
        className: "",
        iconClassName: "",
        menuItem: true,
        onAdded,
      }),
    ),
  );
  expect(button("Add from another Space…")!.getAttribute("role")).toBe(
    "menuitem",
  );
});
