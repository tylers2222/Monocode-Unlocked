// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  notifyArchivedProjectsChanged,
  notifyProjectPathsChanged,
} from "../../projects/model/recents";
import { subscribeAddableProjects } from "./addableProjects";
import { __knownProjectsTestReset, loadKnownProjects } from "./knownProjects";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

beforeEach(() => {
  localStorage.clear();
  __knownProjectsTestReset();
  vi.mocked(invoke).mockResolvedValue([]);
});

afterEach(() => __knownProjectsTestReset());

it.each([
  ["the known list is read again", () => loadKnownProjects()],
  ["the rail's projects change", async () => notifyProjectPathsChanged()],
  ["the archive changes", async () => notifyArchivedProjectsChanged()],
])("tells its listener when %s, until it stops", async (_change, change) => {
  const listener = vi.fn();
  const stop = subscribeAddableProjects(listener);
  await change();
  expect(listener).toHaveBeenCalledOnce();
  stop();
  await change();
  expect(listener).toHaveBeenCalledOnce();
});
