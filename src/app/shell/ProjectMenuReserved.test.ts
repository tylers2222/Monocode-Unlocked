// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
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

beforeEach(async () => {
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
          { path: "/home/u/MonoCode", openedAt: 2 },
          { path: "/work/client", openedAt: 1 },
        ],
        onSelectProject: vi.fn(),
        onOpenProject: vi.fn(),
        onRemoveProject: vi.fn(),
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
