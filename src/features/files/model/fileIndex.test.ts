import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectFile } from "../../../platform/tauri/fs";
import { listProjectFiles } from "../../../platform/tauri/fs";
import {
  invalidateProjectFiles,
  loadProjectFiles,
  peekProjectFiles,
  rememberOpenedFile,
  resolveFileOpenRequest,
  resolveOpenablePath,
  subscribeProjectFiles,
} from "./fileIndex";
import { notifyDirsChanged } from "./fileTree";

const cwd = "/Users/me/project";
const files: ProjectFile[] = [
  {
    name: "App.tsx",
    path: "/Users/me/project/apps/desktop/src/App.tsx",
    relative: "apps/desktop/src/App.tsx",
  },
  {
    name: "App.tsx",
    path: "/Users/me/project/apps/web/src/App.tsx",
    relative: "apps/web/src/App.tsx",
  },
  {
    name: "main.tsx",
    path: "/Users/me/project/apps/desktop/src/main.tsx",
    relative: "apps/desktop/src/main.tsx",
  },
];

const extra: ProjectFile = {
  name: "pasted.ts",
  path: "/Users/me/project/pasted.ts",
  relative: "pasted.ts",
};

vi.mock("../../../platform/tauri/fs", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../../../platform/tauri/fs")>();
  return {
    ...actual,
    listProjectFiles: vi.fn(async () => files),
  };
});

const list = vi.mocked(listProjectFiles);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("resolveOpenablePath", () => {
  beforeEach(() => {
    invalidateProjectFiles();
    list.mockReset();
    list.mockResolvedValue(files);
  });

  it("maps a basename-only link to the shortest matching project path", async () => {
    const resolved = await resolveOpenablePath(cwd, "App.tsx");
    expect(resolved).toBe(files[1].path);
  });

  it("prefers recently opened files for ambiguous basenames", async () => {
    rememberOpenedFile(cwd, files[1].path);
    const resolved = await resolveOpenablePath(cwd, "App.tsx");
    expect(resolved).toBe(files[1].path);
  });

  it("matches a relative project path", async () => {
    const resolved = await resolveOpenablePath(
      cwd,
      "apps/desktop/src/main.tsx",
    );
    expect(resolved).toBe(files[2].path);
  });

  it("still opens a direct file when the optional project index is unavailable", async () => {
    list.mockRejectedValue(new Error("Project scan unavailable"));
    await expect(
      resolveOpenablePath(cwd, "apps/desktop/src/main.tsx"),
    ).resolves.toBe(files[2].path);
  });

  it("preserves an exact path even when it is absent from the project index", async () => {
    const ignored = `${cwd}/ignored/App.tsx`;
    await expect(
      resolveFileOpenRequest(cwd, ignored, { exact: true }),
    ).resolves.toBe(ignored);
    expect(list).not.toHaveBeenCalled();
  });
});

describe("loadProjectFiles", () => {
  beforeEach(() => {
    invalidateProjectFiles();
    list.mockReset();
    list.mockResolvedValue(files);
  });

  afterEach(() => {
    vi.useRealTimers();
    invalidateProjectFiles();
  });

  it("returns the cached listing until refresh", async () => {
    await loadProjectFiles(cwd);
    list.mockResolvedValue([...files, extra]);
    expect(await loadProjectFiles(cwd)).toEqual(files);
    expect(list).toHaveBeenCalledTimes(1);
    expect(await loadProjectFiles(cwd, true)).toEqual([...files, extra]);
    expect(peekProjectFiles(cwd)).toEqual([...files, extra]);
  });

  it("does not drop a refresh that arrives while a scan is in flight", async () => {
    const first = deferred<ProjectFile[]>();
    const second = deferred<ProjectFile[]>();
    list.mockImplementationOnce(() => first.promise);
    list.mockImplementationOnce(() => second.promise);

    const initial = loadProjectFiles(cwd);
    const refresh = loadProjectFiles(cwd, true);
    expect(list).toHaveBeenCalledTimes(2);

    first.resolve(files);
    expect(await initial).toEqual(files);
    expect(peekProjectFiles(cwd)).toBeNull();

    second.resolve([...files, extra]);
    expect(await refresh).toEqual([...files, extra]);
    expect(peekProjectFiles(cwd)).toEqual([...files, extra]);
  });

  it("reuses the in-flight scan when refresh is not requested", async () => {
    const pending = deferred<ProjectFile[]>();
    list.mockImplementationOnce(() => pending.promise);

    const first = loadProjectFiles(cwd);
    const second = loadProjectFiles(cwd);
    expect(list).toHaveBeenCalledTimes(1);

    pending.resolve(files);
    expect(await first).toEqual(files);
    expect(await second).toEqual(files);
  });

  it("keeps both worktree indexes during repeated tab switches", async () => {
    const other = "/Users/me/project-worktree";
    const otherFiles = [{ ...extra, path: `${other}/pasted.ts` }];
    list.mockImplementation(async (path) =>
      path === cwd ? files : otherFiles,
    );

    for (let index = 0; index < 10; index++) {
      expect(await loadProjectFiles(cwd)).toBe(files);
      expect(await loadProjectFiles(other)).toBe(otherFiles);
    }
    expect(list).toHaveBeenCalledTimes(2);
    expect(peekProjectFiles(cwd)).toBe(files);
    expect(peekProjectFiles(other)).toBe(otherFiles);
  });

  it("lets scans for separate worktrees finish independently", async () => {
    const other = "/Users/me/project-worktree";
    const first = deferred<ProjectFile[]>();
    const second = deferred<ProjectFile[]>();
    list.mockImplementationOnce(() => first.promise);
    list.mockImplementationOnce(() => second.promise);
    const firstScan = loadProjectFiles(cwd);
    const secondScan = loadProjectFiles(other);
    expect(loadProjectFiles(cwd)).toBe(firstScan);
    expect(loadProjectFiles(other)).toBe(secondScan);
    first.resolve(files);
    second.resolve([extra]);
    await Promise.all([firstScan, secondScan]);
    expect(peekProjectFiles(cwd)).toBe(files);
    expect(peekProjectFiles(other)).toEqual([extra]);
    expect(list).toHaveBeenCalledTimes(2);
  });

  it("invalidates one worktree without evicting or cancelling another", async () => {
    const other = "/Users/me/project-worktree";
    await loadProjectFiles(cwd);
    await loadProjectFiles(other);
    invalidateProjectFiles(cwd);
    expect(peekProjectFiles(cwd)).toBeNull();
    expect(await loadProjectFiles(other)).toBe(files);
    expect(list).toHaveBeenCalledTimes(2);
    await loadProjectFiles(cwd);
    expect(list).toHaveBeenCalledTimes(3);
  });

  it("does not let an invalidated scan restore its old listing", async () => {
    const pending = deferred<ProjectFile[]>();
    list.mockImplementationOnce(() => pending.promise);
    const scan = loadProjectFiles(cwd);
    invalidateProjectFiles(cwd);
    pending.resolve(files);
    await scan;
    expect(peekProjectFiles(cwd)).toBeNull();
  });

  it("bounds retained worktrees and keeps recently revisited ones", async () => {
    for (let index = 0; index < 8; index++) {
      await loadProjectFiles(`/repo/tree-${index}`);
    }
    await loadProjectFiles("/repo/tree-0");
    await loadProjectFiles("/repo/tree-8");
    expect(peekProjectFiles("/repo/tree-0")).toBe(files);
    expect(peekProjectFiles("/repo/tree-1")).toBeNull();
    expect(list).toHaveBeenCalledTimes(9);
  });

  it("notifies subscribers when the listing changes", async () => {
    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);
    await loadProjectFiles(cwd);
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(peekProjectFiles(cwd)).toEqual(files);
    stop();
  });

  it("keeps the published array and stays quiet when a rescan finds the same files", async () => {
    const dir = {
      name: "apps",
      path: "/Users/me/project/apps",
      relative: "apps",
      isDir: true,
    };
    list.mockResolvedValue([...files, dir]);
    const listed = await loadProjectFiles(cwd);
    // A missing isDir and isDir: false both mean a file.
    list.mockResolvedValue([
      ...files.map((file) => ({ ...file, isDir: false })),
      { ...dir },
    ]);
    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);

    expect(await loadProjectFiles(cwd, true)).toBe(listed);
    expect(peekProjectFiles(cwd)).toBe(listed);
    expect(onChange).not.toHaveBeenCalled();
    stop();
  });

  it.each([
    ["path", { path: "/Users/me/project/apps/web/src/Main.tsx" }],
    ["relative", { relative: "apps/web/src/Main.tsx" }],
    ["name", { name: "Main.tsx" }],
    ["isDir", { isDir: true }],
  ])(
    "publishes a new array and notifies once when only %s changes",
    async (_field, change) => {
      await loadProjectFiles(cwd);
      const next = files.map((file, index) =>
        index === 1 ? { ...file, ...change } : { ...file },
      );
      list.mockResolvedValue(next);
      const onChange = vi.fn();
      const stop = subscribeProjectFiles(onChange);

      expect(await loadProjectFiles(cwd, true)).toBe(next);
      expect(peekProjectFiles(cwd)).toBe(next);
      expect(onChange).toHaveBeenCalledTimes(1);
      stop();
    },
  );

  it("publishes a new array and notifies once when a file is added", async () => {
    await loadProjectFiles(cwd);
    const next = [...files, extra];
    list.mockResolvedValue(next);
    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);

    expect(await loadProjectFiles(cwd, true)).toBe(next);
    expect(onChange).toHaveBeenCalledTimes(1);
    stop();
  });

  it("keeps the array after an invalidation and notifies once on the unchanged rescan", async () => {
    await loadProjectFiles(cwd);
    list.mockResolvedValue(files.map((file) => ({ ...file })));
    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);

    invalidateProjectFiles(cwd);
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(await loadProjectFiles(cwd)).toBe(files);
    expect(peekProjectFiles(cwd)).toBe(files);
    expect(onChange).toHaveBeenCalledTimes(2);
    stop();
  });

  it("does not notify when invalidating a worktree that has no listing", async () => {
    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);
    invalidateProjectFiles("/Users/me/never-listed");
    expect(onChange).not.toHaveBeenCalled();

    await loadProjectFiles(cwd);
    onChange.mockClear();
    invalidateProjectFiles(cwd);
    invalidateProjectFiles(cwd);
    expect(onChange).toHaveBeenCalledTimes(1);
    stop();
  });

  it("reloads after a directory change", async () => {
    vi.useFakeTimers();
    await loadProjectFiles(cwd);
    list.mockResolvedValue([...files, extra]);

    const onChange = vi.fn();
    const stop = subscribeProjectFiles(onChange);
    onChange.mockClear();
    notifyDirsChanged();

    await vi.runAllTimersAsync();
    expect(peekProjectFiles(cwd)).toEqual([...files, extra]);
    expect(onChange).toHaveBeenCalled();
    stop();
  });

  it("does not drop a scan for a different project", async () => {
    const other = "/Users/me/other";
    const pending = deferred<ProjectFile[]>();
    list.mockImplementationOnce(() => pending.promise);

    const scan = loadProjectFiles(other);
    invalidateProjectFiles(cwd);
    pending.resolve(files);

    expect(await scan).toEqual(files);
    expect(peekProjectFiles(other)).toEqual(files);
  });
});
