import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { rememberProject } from "../../projects/model/recents";
import {
  __knownProjectsTestReset,
  loadKnownProjects,
} from "../../spaces/model/knownProjects";
import { monoProjectChoices, resolveMonoProjects } from "./gigaMono";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function mockStorage() {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
  });
}

beforeEach(async () => {
  mockStorage();
  __knownProjectsTestReset();
  vi.mocked(invoke).mockResolvedValue([
    { path: "/work/shared", spaces: ["space-2"] },
    { path: "/work/rail", spaces: ["space-1"] },
  ]);
  rememberProject("/work/rail");
  await loadKnownProjects();
});

afterEach(() => vi.unstubAllGlobals());

it("offers the rail, then every project any Space has, each once", () => {
  expect(monoProjectChoices().map((item) => item.path)).toEqual([
    "/work/rail",
    "/work/shared",
  ]);
});

it("accepts a listed project outside the rail, by path and by name", () => {
  expect(resolveMonoProjects(["/work/shared/"], undefined)).toEqual([
    "/work/shared",
  ]);
  expect(resolveMonoProjects(["shared"], undefined)).toEqual(["/work/shared"]);
});

it("still refuses a folder no Space lists", () => {
  expect(() => resolveMonoProjects(["/work/none"], undefined)).toThrow(
    /Not a project in MonoCode: \/work\/none/,
  );
});

it("falls back to the rail when the known list cannot be read", async () => {
  vi.mocked(invoke).mockRejectedValue("no database");
  await loadKnownProjects();
  expect(monoProjectChoices().map((item) => item.path)).toEqual(["/work/rail"]);
});
