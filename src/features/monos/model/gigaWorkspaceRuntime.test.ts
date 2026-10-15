// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) => {
    if (command === "home_dir") return Promise.resolve("/home/u");
    if (command === "giga_workspace_ensure")
      return Promise.resolve({
        root: "/home/u/MonoCode",
        state: "ready",
        created: [],
      });
    return Promise.reject(new Error(`unexpected ${command}`));
  },
}));

import {
  archiveProject,
  loadArchivedProjects,
  loadRecents,
  rememberProject,
} from "../../projects/model/recents";
import { ensureGigaWorkspace, turnOnGiga } from "./gigaWorkspaceRuntime";
import { createMono, saveMonoName } from "./mono";

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

const railPaths = () => loadRecents().map((project) => project.path);

async function turnOnOps(): Promise<void> {
  const mono = createMono();
  saveMonoName(mono.id, "Ops");
  await turnOnGiga(mono.id);
}

// Defect: turning Giga on leaves ~/MonoCode off the Projects rail, or a later
// ensure adds it again or moves it.
it("turning Giga on puts ~/MonoCode on the Projects rail once", async () => {
  rememberProject("/code/app");
  await turnOnOps();
  expect(railPaths()).toEqual(["/home/u/MonoCode", "/code/app"]);

  rememberProject("/code/later");
  const before = localStorage.getItem("monocode.recentProjects");
  await ensureGigaWorkspace("start");
  expect(localStorage.getItem("monocode.recentProjects")).toBe(before);
});

// Defect: the ensure brings back a workspace the user archived.
it("an archived ~/MonoCode stays archived", async () => {
  await turnOnOps();
  archiveProject("/home/u/MonoCode");
  await ensureGigaWorkspace("start");
  expect(railPaths()).toEqual([]);
  expect(loadArchivedProjects().map((project) => project.path)).toEqual([
    "/home/u/MonoCode",
  ]);
});
