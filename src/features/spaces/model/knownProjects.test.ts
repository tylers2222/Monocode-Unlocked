import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  __knownProjectsTestReset,
  knownProjects,
  loadKnownProjects,
  subscribeKnownProjects,
  watchKnownProjects,
} from "./knownProjects";
import { SPACE_PROJECTS_CHANGED, type KnownProject } from "./spaces";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const SHARED: KnownProject = { path: "/work/shared", spaces: ["space-2"] };
const OTHER: KnownProject = { path: "/work/other", spaces: ["space-1"] };

beforeEach(() => {
  __knownProjectsTestReset();
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(invoke).mockResolvedValue([SHARED]);
  vi.mocked(listen).mockResolvedValue(() => undefined);
});

afterEach(() => __knownProjectsTestReset());

it("reads spaces_project_known and keeps its answer", async () => {
  await loadKnownProjects();
  expect(invoke).toHaveBeenCalledWith("spaces_project_known");
  expect(knownProjects()).toEqual([SHARED]);
});

it("keeps an empty list when the answer is not a list", async () => {
  vi.mocked(invoke).mockResolvedValue(null);
  await loadKnownProjects();
  expect(knownProjects()).toEqual([]);
});

it("keeps the newest read when an older one answers last", async () => {
  const answers: ((value: unknown) => void)[] = [];
  vi.mocked(invoke).mockImplementation(
    () => new Promise((resolve) => answers.push(resolve)),
  );
  const first = loadKnownProjects();
  const second = loadKnownProjects();
  answers[1]([OTHER]);
  await second;
  answers[0]([SHARED]);
  await first;
  expect(knownProjects()).toEqual([OTHER]);
});

it("tells a listener after each read, until it unsubscribes", async () => {
  const listener = vi.fn();
  const unsubscribe = subscribeKnownProjects(listener);
  await loadKnownProjects();
  expect(listener).toHaveBeenCalledOnce();
  unsubscribe();
  await loadKnownProjects();
  expect(listener).toHaveBeenCalledOnce();
});

it("watching reads now and again on each change, and subscribes once", async () => {
  watchKnownProjects();
  watchKnownProjects();
  await vi.waitFor(() => expect(knownProjects()).toEqual([SHARED]));
  expect(listen).toHaveBeenCalledOnce();
  expect(vi.mocked(listen).mock.calls[0][0]).toBe(SPACE_PROJECTS_CHANGED);
  expect(invoke).toHaveBeenCalledOnce();
  vi.mocked(invoke).mockResolvedValue([OTHER]);
  const onChange = vi.mocked(listen).mock.calls[0][1] as () => void;
  onChange();
  await vi.waitFor(() => expect(knownProjects()).toEqual([OTHER]));
});

it("forgets the list and its listeners on a test reset", async () => {
  const listener = vi.fn();
  subscribeKnownProjects(listener);
  await loadKnownProjects();
  __knownProjectsTestReset();
  expect(knownProjects()).toEqual([]);
  await loadKnownProjects();
  expect(listener).toHaveBeenCalledOnce();
});
