// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import { setHomeDir } from "../../shared/lib/paths";
import { loadTabGroupLabels } from "../../features/workspace/model/tabGroups";
import { ProjectRail } from "./ProjectRail";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => path,
}));
vi.mock("../../features/source-control/hooks/useProjectDiffStats", () => ({
  useProjectDiffStats: vi.fn(() => null),
}));

let container: HTMLDivElement;
let root: Root;

const onRemoveProject = vi.fn();

vi.mock("../../features/projects/model/projectDeleteCheck", async (original) => ({
  ...(await original<object>()),
  checkProjectArchive: vi.fn(async () => null),
  checkProjectDelete: vi.fn(async () => null),
  readProjectDeletePrompt: vi.fn(async () => ({
    sessions: 0,
    scoped: true,
    spaceName: "Space 1",
  })),
}));

beforeEach(async () => {
  onRemoveProject.mockReset();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  setHomeDir("/home/u");
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(
      createElement(ProjectRail, {
        visible: true,
        cwd: "/work/client",
        recents: [
          { path: "/home/u/MonoCode", openedAt: 3 },
          { path: "/home/u/MonoCode/gigas/general", openedAt: 2 },
          { path: "/work/client", openedAt: 1 },
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
  return [...document.querySelectorAll('[role="menuitem"]')].map(
    (item) => item.textContent,
  );
}

it("offers no Archive or Delete for the Giga workspace, and both for another project", async () => {
  const workspace = await openMenu("MonoCode");
  expect(workspace).toContain("Pin project");
  expect(workspace).not.toContain("Archive");
  expect(workspace).not.toContain("Delete");

  await act(async () =>
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })),
  );
  const other = await openMenu("client");
  expect(other).toContain("Archive");
  expect(other).toContain("Delete");
});

it("offers no Archive or Delete on a path under the Giga workspace (F1)", async () => {
  const under = await openMenu("general");
  expect(under).not.toContain("Archive");
  expect(under).not.toContain("Delete");
});

it("refuses to rename another project to MonoCode", async () => {
  await openMenu("client");
  const input = document.querySelector<HTMLInputElement>(
    'input[aria-label="Group name"]',
  )!;
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(
      input,
      " monocode ",
    );
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    );
  });
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(
    "MonoCode is reserved for the Giga workspace. Rename the folder or choose another.",
  );
  expect(loadTabGroupLabels()).toEqual({});
});

async function pickFromMenu(project: string, item: string) {
  await openMenu(project);
  const entry = [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')]
    .find((row) => row.textContent === item)!;
  await act(async () => {
    entry.click();
    await new Promise((done) => setTimeout(done));
  });
}

describe("a refused Delete or Archive", () => {
  const refusal = "A session is running in client here. Stop it first.";

  it("stays in the open menu with the refusal in its footer, for Archive", async () => {
    vi.mocked(checkProjectArchive).mockResolvedValue(refusal);
    await pickFromMenu("client", "Archive");
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(refusal);
    expect(document.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
    expect(onRemoveProject).not.toHaveBeenCalled();
  });

  it("closes the menu and archives when Archive is allowed", async () => {
    vi.mocked(checkProjectArchive).mockResolvedValue(null);
    await pickFromMenu("client", "Archive");
    expect(onRemoveProject).toHaveBeenCalledWith("/work/client", { purgeData: false });
    expect(document.querySelectorAll('[role="menuitem"]').length).toBe(0);
  });

  it("stays in the open menu with the refusal in its footer, for Delete", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue(refusal);
    await pickFromMenu("client", "Delete");
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(refusal);
    expect(document.querySelector('[role="dialog"][aria-label^="Delete"]')).toBeNull();
    expect(document.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
  });

  it("closes the menu and opens the dialog when Delete is allowed", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue(null);
    await pickFromMenu("client", "Delete");
    expect(document.querySelector('[role="dialog"][aria-label="Delete client"]')).not.toBeNull();
    expect(document.querySelectorAll('[role="menuitem"]').length).toBe(0);
  });

  it("reopens the menu with a refusal that comes after confirm", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue(null);
    onRemoveProject.mockResolvedValue(refusal);
    await pickFromMenu("client", "Delete");
    const confirm = [...document.querySelectorAll<HTMLButtonElement>('[aria-label="Delete client"] button')]
      .find((button) => button.textContent === "Delete")!;
    await act(async () => {
      confirm.click();
      await new Promise((done) => setTimeout(done));
    });
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(refusal);
    expect(document.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
  });
});
