// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { rememberProject } from "../../projects/model/recents";
import {
  __knownProjectsTestReset,
  loadKnownProjects,
} from "../../spaces/model/knownProjects";
import { MonoProjects } from "./MonoProjects";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

let root: Root;
let container: HTMLDivElement;

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  __knownProjectsTestReset();
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "spaces_project_known"
      ? [
          { path: "/work/rail", spaces: ["space-1"] },
          { path: "/work/shared", spaces: ["space-2"] },
        ]
      : null,
  );
  rememberProject("/work/rail");
  rememberProject("/work/spare");
  await loadKnownProjects();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

it("offers every project any Space has, less the ones the Mono has", async () => {
  await act(async () =>
    root.render(
      createElement(MonoProjects, {
        monoId: "m1",
        projects: [{ path: "/work/rail", name: "rail" }],
      }),
    ),
  );
  await act(async () =>
    [...container.querySelectorAll("button")]
      .find((button) => button.textContent?.includes("Add project"))!
      .click(),
  );
  const picker = document.querySelector('[aria-label="Add a project"]')!;
  const rows = [...picker.querySelectorAll("button")].map(
    (button) => button.textContent ?? "",
  );
  expect(rows.filter((row) => row.includes("shared"))).toHaveLength(1);
  expect(rows.filter((row) => row.includes("spare"))).toHaveLength(1);
  expect(rows.filter((row) => row.includes("rail"))).toHaveLength(0);
});
