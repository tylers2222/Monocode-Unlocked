import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  addRefusalMessage,
  removeProjectFromSpace,
  SPACE_PROJECTS_CHANGED,
  subscribeSpaceProjects,
} from "./spaces";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
});

it("asks the backend to remove that path, and returns its answer", async () => {
  vi.mocked(invoke).mockResolvedValue(true);
  await expect(removeProjectFromSpace("/work/a")).resolves.toBe(true);
  expect(invoke).toHaveBeenCalledWith("spaces_project_remove", {
    projectPath: "/work/a",
  });
});

it("passes each change on, and stops listening when disposed", async () => {
  const stop = vi.fn();
  vi.mocked(listen).mockResolvedValue(stop);
  const onChange = vi.fn();
  const dispose = subscribeSpaceProjects(onChange);
  await vi.waitFor(() => expect(listen).toHaveBeenCalledOnce());
  expect(listen).toHaveBeenCalledWith(SPACE_PROJECTS_CHANGED, onChange);
  // listen has answered, so the subscription holds its stop.
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(stop).not.toHaveBeenCalled();
  dispose();
  expect(stop).toHaveBeenCalledOnce();
});

it("stops a listener that starts after it was disposed", async () => {
  const stop = vi.fn();
  let started!: (value: () => void) => void;
  vi.mocked(listen).mockReturnValue(
    new Promise<() => void>((resolve) => (started = resolve)),
  );
  const dispose = subscribeSpaceProjects(() => undefined);
  dispose();
  started(stop);
  await vi.waitFor(() => expect(stop).toHaveBeenCalledOnce());
});

it.each([
  [
    "the workspace, by its code",
    { code: "workspace", message: "This folder is part of the MonoCode workspace, so it stays." },
    "This folder is part of the MonoCode workspace, so it can't be added to a Space.",
  ],
  [
    "a path with a dot in it",
    { code: "failed", message: "Not a project: ~/my.app" },
    "Not a project: ~/my.app, so it can't be added to Work.",
  ],
  [
    "a sentence's own full stop",
    { code: "failed", message: "This window has no Space yet." },
    "This window has no Space yet, so it can't be added to Work.",
  ],
  [
    "trailing dots and space",
    { code: "failed", message: "database is locked... \n" },
    "database is locked, so it can't be added to Work.",
  ],
  ["plain text", "no database", "no database, so it can't be added to Work."],
  ["nothing at all", null, "null, so it can't be added to Work."],
])("words an Add refusal for %s", (_case, error, text) => {
  expect(addRefusalMessage(error, "Work")).toBe(text);
});
