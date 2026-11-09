// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { setHomeDir } from "../../shared/lib/paths";
import { __spacesTestReset, loadSpaces } from "../../features/spaces/model/activeSpace";
import { ProjectRail } from "./ProjectRail";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("../../features/source-control/hooks/useProjectDiffStats", () => ({
  useProjectDiffStats: vi.fn(() => null),
}));

const SHARED = "/work/vibtra";
const ONLY_HERE = "/work/site";
const GIGA = "/home/u/MonoCode";

let container: HTMLDivElement;
let root: Root;
let onRemoveProject: ReturnType<typeof vi.fn>;
let replies: Record<string, unknown>;

class Rejection {
  constructor(readonly value: unknown) {}
}

const refusal = (code: string, message: string) =>
  new Rejection({ code, project: "vibtra", message });

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  setHomeDir("/home/u");
  __spacesTestReset();
  replies = {
    spaces_list: [
      { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
      { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
    ],
    spaces_settings_get: { activeSpaceId: "space-2", routeGrantTtlMs: 0 },
    spaces_logins_list: [],
    spaces_project_list: {
      spaceId: "space-2",
      projects: [],
      monoHeld: [],
      noRowIsHere: false,
      assignedElsewhere: [],
    },
    spaces_project_known: [
      { path: SHARED, spaces: ["space-1", "space-2"] },
      { path: ONLY_HERE, spaces: ["space-2"] },
    ],
    spaces_project_get: "space-2",
    spaces_project_archive_check: null,
    spaces_project_delete_preview: {
      lastSpace: false,
      sessionCount: 0,
      otherSpaces: ["space-1"],
      hiddenSessionCount: 0,
    },
    session_list_by_project: [],
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    const reply = replies[command] ?? null;
    if (reply instanceof Rejection) throw reply.value;
    return reply;
  });
  await loadSpaces();
  onRemoveProject = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(
      createElement(ProjectRail, {
        visible: true,
        cwd: SHARED,
        recents: [
          { path: GIGA, openedAt: 3 },
          { path: SHARED, openedAt: 2 },
          { path: ONLY_HERE, openedAt: 1 },
        ],
        onSelectProject: vi.fn(),
        onOpenProject: vi.fn(),
        onRemoveProject,
      }),
    ),
  );
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  document.body.innerHTML = "";
  setHomeDir(undefined);
  vi.unstubAllGlobals();
});

async function openMenu(label: string) {
  const project = [...document.querySelectorAll("button")].find(
    (item) => item.getAttribute("aria-label") === label,
  )!;
  await act(async () => {
    project.dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
    );
  });
}

const menuItem = (label: string) =>
  [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
    (item) => (item.getAttribute("aria-label") ?? item.textContent) === label,
  );

async function pick(label: string) {
  await act(async () => menuItem(label)!.click());
}

const footer = () => document.querySelector('[role="alert"]')?.textContent;

it("offers Remove with its lines on a shared row", async () => {
  await openMenu("vibtra");
  expect(menuItem("Remove from Space 2")?.textContent).toContain(
    "Its conversations stay. They come back if you add it again.",
  );
  expect(menuItem("Archive")?.textContent).toContain("Hides it in every Space.");
  expect(menuItem("Delete")?.textContent).toContain(
    "Space 2's conversations only. You confirm next.",
  );
});

it("has today's menu on a row only in this Space", async () => {
  await openMenu("site");
  expect(menuItem("Remove from Space 2")).toBeUndefined();
  expect(menuItem("Archive")?.textContent).toBe("Archive");
  expect(menuItem("Delete")?.textContent).toBe("Delete");
});

it("offers none of the three on the Giga workspace", async () => {
  await openMenu("MonoCode");
  expect(menuItem("Remove from Space 2")).toBeUndefined();
  expect(menuItem("Archive")).toBeUndefined();
  expect(menuItem("Delete")).toBeUndefined();
});

it("reaches the window's Remove, and closes when it answers", async () => {
  onRemoveProject.mockResolvedValue(null);
  await openMenu("vibtra");
  await pick("Remove from Space 2");
  expect(onRemoveProject).toHaveBeenCalledWith(SHARED, {
    purgeData: false,
    leaveSpace: true,
  });
  expect(menuItem("Archive")).toBeUndefined();
});

it("keeps the menu open with a refused Remove's message", async () => {
  const text = "Vibtra Orchestrator uses vibtra. Take it off that Mono first.";
  onRemoveProject.mockResolvedValue(text);
  await openMenu("vibtra");
  await pick("Remove from Space 2");
  expect(footer()).toBe(text);
  expect(menuItem("Archive")).toBeDefined();
});

it.each([
  [
    "a Mono in another Space",
    "monoHeld",
    "Site Builder in Space 1 uses vibtra, and Archive hides it in every Space. Take it off that Mono first.",
  ],
  [
    "a running session",
    "sessionRunning",
    "A session is running in vibtra in Space 1. Stop it first.",
  ],
  ["a Mono's chat", "monoChat", "General's chat is in this project."],
])("guards Archive before archiving: %s", async (_, code, text) => {
  replies.spaces_project_archive_check = refusal(code, text);
  await openMenu("vibtra");
  await pick("Archive");
  expect(footer()).toBe(text);
  expect(onRemoveProject).not.toHaveBeenCalled();
});

it("archives once the guard passes", async () => {
  await openMenu("vibtra");
  await pick("Archive");
  expect(onRemoveProject).toHaveBeenCalledWith(SHARED, { purgeData: false });
});

it.each([
  [
    "monoHeld",
    "Vibtra Orchestrator and Mono Reviewer use vibtra. Take it off those Monos first.",
  ],
  ["sessionRunning", "A session is running in vibtra here. Stop it first."],
  ["monoChat", "General's chat is in this project."],
  ["automation", "The automation Nightly report runs in vibtra. Move or delete it first."],
  ["failed", "database is locked"],
])("refuses Delete in the footer, before its dialog opens: %s", async (code, text) => {
  replies.spaces_project_delete_preview = refusal(code, text);
  await openMenu("vibtra");
  await pick("Delete");
  expect(footer()).toBe(text);
  expect(document.querySelector('[aria-label="Delete vibtra"]')).toBeNull();
});

it("opens frame 22's dialog when Delete passes", async () => {
  await openMenu("vibtra");
  await pick("Delete");
  const dialog = document.querySelector('[aria-label="Delete vibtra"]')!;
  expect(dialog.textContent).toContain("Delete “vibtra” from Space 2?");
  expect(dialog.textContent).toContain("Delete from Space 2");
});
