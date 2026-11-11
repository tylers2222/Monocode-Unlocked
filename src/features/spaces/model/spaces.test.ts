import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
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
