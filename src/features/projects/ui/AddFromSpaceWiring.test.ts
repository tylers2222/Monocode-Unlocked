// @vitest-environment happy-dom
// The add list is a flyout portalled beside the menu it hangs off. A pick
// starts with a pointerdown outside that menu, so each host menu must ignore
// the flyout, or it closes and takes the list with it before the click lands.
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
import { AddProjectButton } from "../../../app/shell/ProjectRail";
import { addFromSpaceAction } from "./AddFromSpaceEntry";
import { SearchableProjectPicker } from "./SearchableProjectPicker";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("../../source-control/hooks/useProjectDiffStats", () => ({
  useProjectDiffStats: vi.fn(() => null),
}));

let root: Root;
let container: HTMLDivElement;
let adds: unknown[];

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  setHomeDir("/home/me");
  __spacesTestReset();
  __knownProjectsTestReset();
  adds = [];
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "spaces_list")
      return [
        { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
        { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
      ];
    if (command === "spaces_settings_get")
      return { activeSpaceId: "space-2", routeGrantTtlMs: 0 };
    if (command === "spaces_logins_list") return [];
    if (command === "spaces_project_known")
      return [{ path: "/code/example-app", spaces: ["space-1"] }];
    if (command === "spaces_project_add") {
      adds.push(input);
      return "linked";
    }
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

function button(text: string) {
  return [...document.querySelectorAll("button")].find((item) =>
    item.textContent?.includes(text),
  );
}

/** Opens the add list, then picks its row as a pointer does. */
async function pickFromFlyout() {
  await act(async () => button("Add from another Space…")!.click());
  const row = button("example-app")!;
  await act(async () => {
    row.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
  });
  await act(async () => row.click());
}

it("adds from the rail's + menu: a click in the list leaves the menu open", async () => {
  const onOpenFolder = vi.fn();
  await act(async () =>
    root.render(createElement(AddProjectButton, { onOpenFolder })),
  );
  await act(async () =>
    container.querySelector<HTMLButtonElement>('[aria-label="Open project"]')!.click(),
  );
  await pickFromFlyout();
  expect(adds).toEqual([
    { projectPath: "/code/example-app", knownToWindow: true },
  ]);
  expect(document.querySelector('[role="menu"]')).toBeNull();
  expect(onOpenFolder).not.toHaveBeenCalled();
});

it("adds from the sidebar picker's footer: a click in the list leaves the picker open", async () => {
  await act(async () =>
    root.render(
      createElement(SearchableProjectPicker, {
        cwd: "/code/app",
        recents: [{ path: "/code/app", openedAt: 1 }],
        compact: true,
        onSelectProject: vi.fn(),
        onOpenProject: vi.fn(),
        footerAction: addFromSpaceAction,
      }),
    ),
  );
  await act(async () => container.querySelector("button")!.click());
  expect(document.querySelector('[aria-label="Project picker"]')).not.toBeNull();
  await pickFromFlyout();
  expect(adds).toEqual([
    { projectPath: "/code/example-app", knownToWindow: true },
  ]);
  expect(document.querySelector('[aria-label="Project picker"]')).toBeNull();
});
