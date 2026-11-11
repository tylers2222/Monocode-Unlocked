// @vitest-environment happy-dom
import { act, createElement, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { AddRemoteProjectDialog } from "./AddRemoteProjectDialog";
import { remoteProjectFor } from "../model/remoteProjects";
import { isLocalProject, looksLikeProject } from "../../projects/model/recents";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

let root: Root;
let container: HTMLDivElement;
let machines: unknown[];
const opened: string[] = [];
let addProject: (input: unknown) => Promise<unknown>;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  opened.length = 0;
  addProject = async () => "assigned";
  machines = [
    {
      id: "machine",
      name: "Home server",
      endpoint: "ssh://home",
      environmentId: "env",
    },
  ];
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "remote_machines") return machines;
    if (command === "spaces_project_add") return addProject(input);
    const { method, params } = input as {
      method: string;
      params: { path?: string; cwd?: string };
    };
    if (method === "projects.browse" && params.path === "/home/me/code/app")
      return {
        path: "/home/me/code/app",
        parent: "/home/me/code",
        entries: [],
      };
    if (method === "projects.browse")
      return params.path === "/home/me/code"
        ? {
            path: "/home/me/code",
            parent: "/home/me",
            entries: [{ name: "app", path: "/home/me/code/app" }],
          }
        : {
            path: "/home/me",
            parent: "/home",
            entries: [{ name: "code", path: "/home/me/code" }],
          };
    if (method === "projects.open")
      return { id: "host-project", cwd: params.cwd, name: "app" };
    throw new Error(`Unexpected ${method}`);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
  localStorage.clear();
});

async function render() {
  // The app runs in StrictMode, which remounts components in development.
  await act(async () =>
    root.render(
      createElement(
        StrictMode,
        null,
        createElement(AddRemoteProjectDialog, {
          onCancel: vi.fn(),
          onOpen: (key: string) => opened.push(key),
        }),
      ),
    ),
  );
  for (let i = 0; i < 4; i++)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
}
const button = (text: string) =>
  [...document.body.querySelectorAll("button")].find((candidate) =>
    candidate.textContent?.includes(text),
  )!;

it("browses a machine's folders and adds the chosen one as a project", async () => {
  await render();
  await act(async () => button("code").click());
  await act(async () => button("app").click());
  const path = document.body.querySelector<HTMLInputElement>(
    'input[aria-label="Folder path on the machine"]',
  )!;
  expect(path.value).toBe("/home/me/code/app");
  expect(path.getAttribute("spellcheck")).toBe("false");
  await act(async () => button("Open").click());
  expect(opened).toEqual(["remote://env/home/me/code/app"]);
  expect(remoteProjectFor(opened[0])).toEqual({
    key: "remote://env/home/me/code/app",
    environmentId: "env",
    projectId: "host-project",
    cwd: "/home/me/code/app",
  });
  // It is a rail project, but never a folder on this computer.
  expect(looksLikeProject(opened[0])).toBe(true);
  expect(isLocalProject(opened[0])).toBe(false);
});

it("points to Settings when no machine is connected", async () => {
  machines = [];
  await render();
  expect(document.body.textContent).toContain("No machines are connected yet");
  expect(button("Add a machine")).toBeTruthy();
});

it("ignores a project that finishes opening after cancellation", async () => {
  const original = vi.mocked(invoke).getMockImplementation()!;
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, input) => {
    if ((input as { method?: string } | undefined)?.method === "projects.open")
      return new Promise((resolve) => { finish = resolve; });
    return original(command, input);
  });
  await render();
  await act(async () => button("Open").click());
  await act(async () => button("Cancel").click());
  await act(async () => finish({ id: "late", cwd: "/home/me", name: "me" }));
  expect(opened).toEqual([]);
});

it("refuses a folder named MonoCode on the machine and adds no project", async () => {
  await render();
  const path = document.body.querySelector<HTMLInputElement>(
    'input[aria-label="Folder path on the machine"]',
  )!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(
      path,
      "/home/me/MonoCode",
    );
    path.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => button("Open").click());
  expect(document.body.querySelector('[role="alert"]')?.textContent).toBe(
    "MonoCode is reserved for the Giga workspace. Rename the folder or choose another.",
  );
  expect(opened).toEqual([]);
  expect(localStorage.length).toBe(0);
  expect(
    vi.mocked(invoke).mock.calls.some(
      ([, input]) =>
        (input as { method?: string } | undefined)?.method === "projects.open",
    ),
  ).toBe(false);
});

it("refuses a typed path the machine resolves to a folder named MonoCode", async () => {
  const original = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, input) =>
    (input as { method?: string } | undefined)?.method === "projects.open"
      ? { id: "host-project", cwd: "/home/me/MonoCode", name: "MonoCode" }
      : original(command, input),
  );
  await render();
  await act(async () => button("Open").click());
  expect(document.body.querySelector('[role="alert"]')?.textContent).toBe(
    "MonoCode is reserved for the Giga workspace. Rename the folder or choose another.",
  );
  expect(opened).toEqual([]);
  expect(localStorage.length).toBe(0);
});

it("adds a new machine folder to the Space as new, then opens it", async () => {
  const calls: unknown[] = [];
  addProject = async (input) => {
    calls.push(input);
    expect(opened).toEqual([]);
    return "assigned";
  };
  await render();
  await act(async () => button("Open").click());
  expect(calls).toEqual([
    { projectPath: "remote://env/home/me", knownToWindow: false },
  ]);
  expect(opened).toEqual(["remote://env/home/me"]);
});

it.each([
  ["monocode.recentProjects", { path: "remote://env/home/me", openedAt: 1 }],
  ["monocode.archivedProjects", { path: "remote://env/home/me", archivedAt: 1 }],
])("adds a machine folder %s holds as known", async (key, entry) => {
  localStorage.setItem(key, JSON.stringify([entry]));
  const calls: unknown[] = [];
  addProject = async (input) => {
    calls.push(input);
    return "linked";
  };
  await render();
  await act(async () => button("Open").click());
  expect(calls).toEqual([
    { projectPath: "remote://env/home/me", knownToWindow: true },
  ]);
});

it("shows a refused add and neither opens nor remembers the project", async () => {
  addProject = () =>
    Promise.reject({
      code: "failed",
      message: "The Space could not be read.",
    });
  await render();
  await act(async () => button("Open").click());
  expect(document.body.textContent).toContain("The Space could not be read.");
  expect(opened).toEqual([]);
  expect(remoteProjectFor("remote://env/home/me")).toBeUndefined();
});

it("adds nothing when the dialog closed while the folder opened", async () => {
  const original = vi.mocked(invoke).getMockImplementation()!;
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, input) => {
    if ((input as { method?: string } | undefined)?.method === "projects.open")
      return new Promise((resolve) => { finish = resolve; });
    return original(command, input);
  });
  const add = vi.fn(async () => "linked");
  addProject = add;
  await render();
  await act(async () => button("Open").click());
  await act(async () => button("Cancel").click());
  await act(async () => finish({ id: "late", cwd: "/home/me", name: "me" }));
  expect(add).not.toHaveBeenCalled();
});

it("drops an open that a newer folder browse replaced", async () => {
  const original = vi.mocked(invoke).getMockImplementation()!;
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, input) => {
    if ((input as { method?: string } | undefined)?.method === "projects.open")
      return new Promise((resolve) => { finish = resolve; });
    return original(command, input);
  });
  const add = vi.fn(async () => "linked");
  addProject = add;
  await render();
  await act(async () => button("Open").click());
  await act(async () => button("code").click());
  await act(async () => finish({ id: "late", cwd: "/home/me", name: "me" }));
  expect(add).not.toHaveBeenCalled();
  expect(opened).toEqual([]);
});

it("drops a refusal that a newer folder browse replaced", async () => {
  let refuse!: (reason: unknown) => void;
  addProject = () => new Promise((_resolve, reject) => { refuse = reject; });
  await render();
  await act(async () => button("Open").click());
  await act(async () => button("code").click());
  await act(async () => refuse({ code: "failed", message: "Stale refusal." }));
  expect(document.body.textContent).not.toContain("Stale refusal.");
});
