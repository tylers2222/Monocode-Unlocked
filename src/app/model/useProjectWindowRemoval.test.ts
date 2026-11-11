// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setOpenSessionsSource } from "../../features/projects/model/projectDeleteCheck";
import { removeProjectFromActiveSpace } from "../../features/projects/model/projectRowActions";
import {
  purgeProjectInSpace,
  type ProjectPurge,
} from "../../features/projects/model/projectPurge";
import {
  loadArchivedProjects,
  loadRecents,
  rememberProject,
} from "../../features/projects/model/recents";
import { newSession, type Session } from "../../features/sessions/model/session";
import {
  leaf,
  newFileTab,
  newTab,
  type LayoutNode,
  type WorkspaceTab,
} from "../../features/workspace/model/layout";
import { forgetHarnessSession } from "../../integrations/harness/core/registry";
import { QUEUED_FOLLOW_UP_ARCHIVED } from "./appSessionSend";
import {
  removeProject,
  removeProjectFromWindow,
  useProjectWindowRemoval,
  type ProjectRemovalContext,
} from "./useProjectWindowRemoval";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
  invoke: vi.fn(async () => null),
}));
vi.mock("../../integrations/harness/core/registry", async (original) => ({
  ...(await original<object>()),
  forgetHarnessSession: vi.fn(async () => undefined),
}));
vi.mock("../../features/projects/model/projectPurge", async (original) => ({
  ...(await original<object>()),
  purgeProjectInSpace: vi.fn(),
}));
vi.mock("../../features/projects/model/projectRowActions", async (original) => ({
  ...(await original<object>()),
  removeProjectFromActiveSpace: vi.fn(),
}));
vi.mock("../../features/projects/model/projectDeleteCheck", async (original) => ({
  ...(await original<object>()),
  setOpenSessionsSource: vi.fn(),
}));
/** The shown Space's list: by default every recent is in it. */
const space = vi.hoisted(() => ({
  list: {
    spaceId: "space-1",
    projects: [],
    monoHeld: [],
    noRowIsHere: true,
    assignedElsewhere: [] as string[],
  },
}));
vi.mock("../../features/spaces/model/activeSpace", async (original) => ({
  ...(await original<object>()),
  spacesView: () => ({
    loaded: true,
    spaces: [],
    activeSpaceId: "space-1",
    logins: [],
    projectList: space.list,
  }),
}));

const P = "/code/vibtra";
const OTHER = "/code/other";

function session(id: string, fields: Partial<Session> = {}): Session {
  return {
    ...newSession("claude", P),
    id,
    blocks: [{ id: `${id}-u`, role: "user", text: "hi" }],
    ...fields,
  } as Session;
}

const purge = (fields: Partial<ProjectPurge> = {}): ProjectPurge => ({
  deleted: new Set(),
  lastSpace: false,
  complete: true,
  ...fields,
});

function context(
  sessions: Session[],
  tabs: WorkspaceTab[],
  fields: Partial<ProjectRemovalContext> = {},
) {
  const activeTabId = tabs[0]?.id ?? "none";
  const ctx = {
    tabsRef: { current: tabs },
    sessionsRef: { current: sessions },
    activeTabIdRef: { current: activeTabId },
    projectCwdRef: { current: OTHER },
    pendingPersist: { current: new Map(sessions.map((s) => [s.id, s])) },
    lastPersisted: { current: new Map(sessions.map((s) => [s.id, "saved"])) },
    loadedSessionCache: { current: new Map<string, Session>() },
    turnWatchers: { current: { cancel: vi.fn() } },
    invalidateLoadedSession: vi.fn(),
    persistSession: vi.fn(),
    onSelectProject: vi.fn(),
    setRecents: vi.fn(),
    setSessions: vi.fn(),
    setTabs: vi.fn(),
    setActiveTabId: vi.fn(),
    setDirtyFiles: vi.fn(),
    setProjectTerminals: vi.fn(),
    setSidebarTabSelection: vi.fn(),
    setHistory: vi.fn(),
    setProjectCwd: vi.fn(),
    setComposerFocused: vi.fn(),
    ...fields,
  };
  return ctx satisfies ProjectRemovalContext;
}

/** Runs the updater a setter was called with. */
function applied<T>(setter: unknown, value: T): T {
  const [[update]] = vi.mocked(setter as (next: unknown) => void).mock.calls;
  return (update as (current: T) => T)(value);
}

const ids = (items: { id: string }[]) => items.map((item) => item.id);

beforeEach(() => {
  localStorage.clear();
  space.list.assignedElsewhere = [];
  vi.mocked(invoke).mockClear();
  vi.mocked(forgetHarnessSession).mockClear();
});

afterEach(() => vi.restoreAllMocks());

describe("a purge from Space 2 of a project Space 1 has (T10, R8, R18)", () => {
  const deleted = new Set(["two-a"]);

  it("keeps a running Space 1 turn, its tab and its pending save", () => {
    const running = session("one-busy", { busy: true });
    const mine = session("two-a");
    const tabs = [newTab(running.id), newTab(mine.id)];
    const ctx = context([running, mine], tabs);

    removeProjectFromWindow(ctx, P, purge({ deleted }));

    expect(ids(ctx.sessionsRef.current)).toEqual(["one-busy"]);
    expect(ctx.tabsRef.current).toEqual([tabs[0]]);
    expect(ctx.setSessions).toHaveBeenCalledWith(ctx.sessionsRef.current);
    expect(ctx.setTabs).toHaveBeenCalledWith(ctx.tabsRef.current);
    expect(ctx.pendingPersist.current.has("one-busy")).toBe(true);
    expect(ctx.lastPersisted.current.has("one-busy")).toBe(true);
    expect(ctx.invalidateLoadedSession).not.toHaveBeenCalledWith("one-busy");
    expect(forgetHarnessSession).toHaveBeenCalledTimes(1);
    expect(forgetHarnessSession).toHaveBeenCalledWith("claude", "two-a");
  });

  it("drops the pending save of every deleted row, open or not", () => {
    const mine = session("two-a");
    const ctx = context([mine], [newTab(mine.id)]);
    ctx.pendingPersist.current.set("hidden-worker", mine);
    ctx.lastPersisted.current.set("hidden-worker", "saved");

    removeProjectFromWindow(
      ctx,
      P,
      purge({ deleted: new Set(["two-a", "hidden-worker"]) }),
    );

    expect(ctx.invalidateLoadedSession).toHaveBeenCalledWith("hidden-worker");
    expect(ctx.pendingPersist.current.size).toBe(0);
    expect(ctx.lastPersisted.current.size).toBe(0);
  });

  it("drops a blank draft's pending save with its tab", () => {
    const draft = session("draft", { blocks: [] });
    const keep = session("one-a");
    const ctx = context([draft, keep], [newTab(draft.id), newTab(keep.id)]);
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.pendingPersist.current.has("draft")).toBe(false);
    expect(ctx.lastPersisted.current.has("draft")).toBe(false);
    expect(ctx.pendingPersist.current.has("one-a")).toBe(true);
    expect(forgetHarnessSession).toHaveBeenCalledWith("claude", "draft");
  });

  it("takes deleted rows out of the sidebar history, and only those", () => {
    const ctx = context([], [newTab("x")]);
    removeProjectFromWindow(ctx, P, purge({ deleted }));
    const rows = [{ id: "two-a" }, { id: "one-a" }];
    expect(applied(ctx.setHistory, rows)).toEqual([{ id: "one-a" }]);
  });

  it("replaces a tab that also holds another Space's session", () => {
    const keep = session("one-a");
    const mine = session("two-a");
    const tab = {
      ...newTab(keep.id),
      layout: {
        type: "split",
        id: "s",
        dir: "row",
        children: [leaf(keep.id), leaf(mine.id)],
        sizes: [0.5, 0.5],
      } as unknown as LayoutNode,
    };
    const ctx = context([keep, mine], [tab]);
    removeProjectFromWindow(ctx, P, purge({ deleted }));
    expect(ctx.tabsRef.current).toHaveLength(1);
    expect(ctx.tabsRef.current[0].layout).toEqual(leaf(keep.id));
    expect(ids(ctx.sessionsRef.current)).toEqual(["one-a"]);
  });

  it("keeps rail, terminals and shared settings, and clears only with the last Space", () => {
    rememberProject(P);
    const ctx = context([], [newTab("x")]);
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.setRecents).toHaveBeenCalledWith(loadRecents());
    expect(loadRecents()).toHaveLength(1);
    expect(ctx.setProjectTerminals).not.toHaveBeenCalled();
    expect(vi.mocked(invoke).mock.calls.map(([c]) => c)).not.toContain(
      "spaces_project_notify_changed",
    );

    const last = context([], [newTab("x")]);
    removeProjectFromWindow(last, P, purge({ lastSpace: true }));
    expect(loadRecents()).toEqual([]);
    expect(last.setRecents).toHaveBeenCalledWith([]);
    const docks = [{ projectPath: P }, { projectPath: OTHER }];
    expect(applied(last.setProjectTerminals, docks)).toEqual([{ projectPath: OTHER }]);
  });

  it("resets the sidebar selection on this project only", () => {
    // The app never saves a tab for "~"; this shows which key the reset reads.
    localStorage.setItem("monocode.projectSidebarTabs.v1", JSON.stringify({ "~": "changes" }));
    const ctx = context([], [newTab("x")]);
    removeProjectFromWindow(ctx, P, purge());
    expect(applied(ctx.setSidebarTabSelection, { project: P, tab: "sessions" })).toEqual({
      project: "~",
      tab: "changes",
    });
    vi.mocked(ctx.setSidebarTabSelection).mockClear();
    removeProjectFromWindow(ctx, P, purge());
    const other = { project: OTHER, tab: "sessions" };
    expect(applied(ctx.setSidebarTabSelection, other)).toBe(other);
  });
});

describe("a purge that stopped halfway (G10)", () => {
  it("drops only the rows that went, and leaves the window and the project alone", () => {
    const went = session("two-a");
    const stayed = session("two-b");
    const ctx = context([went, stayed], [newTab(went.id), newTab(stayed.id)], {
      projectCwdRef: { current: P },
    });
    removeProjectFromWindow(
      ctx,
      P,
      purge({ deleted: new Set(["two-a"]), complete: false }),
    );
    expect(ids(ctx.sessionsRef.current)).toEqual(["two-b"]);
    expect(ctx.setSidebarTabSelection).not.toHaveBeenCalled();
    expect(ctx.setProjectCwd).not.toHaveBeenCalled();
    expect(ctx.onSelectProject).not.toHaveBeenCalled();
    expect(ctx.setProjectTerminals).not.toHaveBeenCalled();
  });
});

describe("Archive", () => {
  it("files the project in the archive and tells other windows", () => {
    rememberProject(P);
    const ctx = context([], [newTab("x")]);
    removeProjectFromWindow(ctx, P, null);
    expect(loadArchivedProjects().map((item) => item.path)).toEqual([P]);
    expect(ctx.setRecents).toHaveBeenCalledWith([]);
    expect(invoke).toHaveBeenCalledWith("spaces_project_notify_changed");
    expect(ctx.setSidebarTabSelection).not.toHaveBeenCalled();
    expect(ctx.setHistory).not.toHaveBeenCalled();
    const docks = [{ projectPath: P }, { projectPath: OTHER }];
    expect(applied(ctx.setProjectTerminals, docks)).toEqual([{ projectPath: OTHER }]);
  });

  it("saves an idle session, cancels its queue, and forgets its harness", () => {
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    const idle = session("idle", { queuedMessages });
    const ctx = context([idle], [newTab(idle.id)]);
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.turnWatchers.current.cancel).toHaveBeenCalledWith("q", QUEUED_FOLLOW_UP_ARCHIVED);
    expect(ctx.loadedSessionCache.current.has("idle")).toBe(true);
    expect(ctx.persistSession).toHaveBeenCalledWith(idle);
    expect(ctx.pendingPersist.current.has("idle")).toBe(false);
    expect(forgetHarnessSession).toHaveBeenCalledWith("claude", "idle");
    expect(ctx.invalidateLoadedSession).not.toHaveBeenCalled();
  });

  it("does not cache a blank session, and handles one with no queue", () => {
    const blank = session("blank", { blocks: [] });
    const ctx = context([blank], [newTab(blank.id)]);
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.loadedSessionCache.current.has("blank")).toBe(false);
    expect(ctx.persistSession).toHaveBeenCalledWith(blank);
    expect(ctx.turnWatchers.current.cancel).not.toHaveBeenCalled();
  });

  it("keeps a busy session running in memory, untouched, while its tab closes", () => {
    const queuedMessages = [{ id: "q" }] as Session["queuedMessages"];
    const busy = session("busy", { busy: true, queuedMessages });
    const elsewhere = session("else", { cwd: OTHER });
    const tabs = [newTab(busy.id), newTab(elsewhere.id)];
    const ctx = context([busy, elsewhere], tabs);
    removeProjectFromWindow(ctx, P, null);
    expect(ids(ctx.sessionsRef.current)).toEqual(["busy", "else"]);
    expect(ctx.tabsRef.current).toEqual([tabs[1]]);
    expect(ctx.turnWatchers.current.cancel).not.toHaveBeenCalled();
    expect(ctx.persistSession).not.toHaveBeenCalled();
    expect(ctx.pendingPersist.current.has("busy")).toBe(true);
    expect(forgetHarnessSession).not.toHaveBeenCalled();
  });
});

describe("what the window shows after", () => {
  it("opens a blank tab when no tab is left, with the runtime mode of what remains", () => {
    const stays = session("busy", { busy: true, runtimeMode: "auto-accept" as Session["runtimeMode"] });
    const ctx = context([stays], [newTab(stays.id)]);
    const before = ctx.activeTabIdRef.current;
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.tabsRef.current).toHaveLength(1);
    expect(ctx.tabsRef.current[0].id).not.toBe(before);
    expect(ctx.sessionsRef.current).toHaveLength(2);
    const blank = ctx.sessionsRef.current[1];
    expect(blank.cwd).toBe("~");
    expect(blank.runtimeMode).toBe("auto-accept");
    expect(ctx.tabsRef.current[0].focusedId).toBe(blank.id);
    expect(ctx.activeTabIdRef.current).toBe(ctx.tabsRef.current[0].id);
    expect(ctx.setActiveTabId).toHaveBeenCalledWith(ctx.tabsRef.current[0].id);
  });

  it("opens a blank tab with nothing left at all", () => {
    const only = session("only");
    const ctx = context([only], [newTab(only.id)]);
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.sessionsRef.current[0].cwd).toBe("~");
  });

  it("moves off a closed active tab to the first tab left", () => {
    const gone = session("gone");
    const keep = session("keep", { cwd: OTHER });
    const tabs = [newTab(gone.id), newTab(keep.id)];
    const ctx = context([gone, keep], tabs);
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.activeTabIdRef.current).toBe(tabs[1].id);
    expect(ctx.setActiveTabId).toHaveBeenCalledWith(tabs[1].id);
  });

  it("leaves an active tab that stays", () => {
    const first = session("first", { cwd: OTHER });
    const keep = session("keep", { cwd: OTHER });
    const gone = session("gone");
    const tabs = [newTab(first.id), newTab(keep.id), newTab(gone.id)];
    const ctx = context([first, keep, gone], tabs, {
      activeTabIdRef: { current: tabs[1].id },
    });
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.activeTabIdRef.current).toBe(tabs[1].id);
    expect(ctx.setActiveTabId).not.toHaveBeenCalled();
  });

  it("clears dirty marks of files in closed tabs only", () => {
    const file = newFileTab(`${P}/a.ts`, P);
    const term = newFileTab(`${P}/t`, P);
    const gone = session("gone");
    const tab = {
      ...newTab(gone.id),
      editorPanes: [{ id: "e", files: [file], activeFileId: file.id }],
      terminalPanes: [{ id: "t", files: [term], activeFileId: term.id }],
    } as unknown as WorkspaceTab;
    const bare = session("bare");
    const ctx = context([gone, bare], [tab, newTab(bare.id)]);
    removeProjectFromWindow(ctx, P, null);
    const dirty = new Set([file.id, term.id, "kept"]);
    const next = applied(ctx.setDirtyFiles, dirty);
    expect([...next]).toEqual(["kept"]);
    expect(dirty.size).toBe(3);
  });

  it("handles a closed tab with no terminal panes", () => {
    const gone = session("gone");
    const tab = { ...newTab(gone.id), terminalPanes: undefined } as WorkspaceTab;
    const ctx = context([gone], [tab]);
    removeProjectFromWindow(ctx, P, null);
    expect([...applied(ctx.setDirtyFiles, new Set(["kept"]))]).toEqual(["kept"]);
  });
});

describe("the move off the open project", () => {
  it("goes to the next project, never the one it left", () => {
    rememberProject(OTHER);
    rememberProject(P);
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.onSelectProject).toHaveBeenCalledWith(OTHER);
    expect(ctx.setProjectCwd).toHaveBeenCalledWith(OTHER);
    expect(ctx.setComposerFocused).not.toHaveBeenCalled();
  });

  it("skips what is no project, and with nothing left goes to no project", () => {
    rememberProject(P);
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.onSelectProject).not.toHaveBeenCalled();
    expect(ctx.setProjectCwd).toHaveBeenCalledWith("~");
    expect(ctx.setComposerFocused).toHaveBeenCalledWith(true);
  });

  it("never goes to a project the shown Space does not have", () => {
    rememberProject(OTHER);
    rememberProject(P);
    space.list.assignedElsewhere = [OTHER];
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.onSelectProject).not.toHaveBeenCalled();
    expect(ctx.setProjectCwd).toHaveBeenCalledWith("~");
  });

  it("stays put when another project is open", () => {
    rememberProject(P);
    const ctx = context([], [newTab("x")]);
    removeProjectFromWindow(ctx, P, purge());
    expect(ctx.setProjectCwd).not.toHaveBeenCalled();
  });

  it("moves after an archive too", () => {
    rememberProject(OTHER);
    rememberProject(P);
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    removeProjectFromWindow(ctx, P, null);
    expect(ctx.setProjectCwd).toHaveBeenCalledWith(OTHER);
  });
});

describe("the route", () => {
  it("archives without the delete, and answers nothing", () => {
    const ctx = context([], [newTab("x")]);
    expect(removeProject(ctx, `${P}/`, { purgeData: false })).toBeUndefined();
    expect(purgeProjectInSpace).not.toHaveBeenCalled();
    expect(loadArchivedProjects().map((item) => item.path)).toEqual([P]);
  });

  it("changes nothing on a refusal, and answers with it", async () => {
    vi.mocked(purgeProjectInSpace).mockResolvedValue({ refusal: "No.", purge: null });
    const mine = session("two-a");
    const ctx = context([mine], [newTab(mine.id)]);
    await expect(removeProject(ctx, P, { purgeData: true })).resolves.toBe("No.");
    expect(ids(ctx.sessionsRef.current)).toEqual(["two-a"]);
    expect(ctx.setRecents).not.toHaveBeenCalled();
    expect(purgeProjectInSpace).toHaveBeenCalledWith(P);
  });

  it("acts on the delete, and answers null", async () => {
    vi.mocked(purgeProjectInSpace).mockResolvedValue({
      refusal: null,
      purge: purge({ deleted: new Set(["two-a"]) }),
    });
    const mine = session("two-a");
    const ctx = context([mine], [newTab(mine.id)]);
    await expect(removeProject(ctx, `${P}/`, { purgeData: true })).resolves.toBeNull();
    expect(ids(ctx.sessionsRef.current)).not.toContain("two-a");
    expect(purgeProjectInSpace).toHaveBeenCalledWith(P);
  });

  it("acts on a halfway failure and answers with its message", async () => {
    vi.mocked(purgeProjectInSpace).mockResolvedValue({
      refusal: "disk full",
      purge: purge({ deleted: new Set(["two-a"]), complete: false }),
    });
    const mine = session("two-a");
    const ctx = context([mine], [newTab(mine.id)]);
    await expect(removeProject(ctx, P, { purgeData: true })).resolves.toBe("disk full");
    expect(ids(ctx.sessionsRef.current)).not.toContain("two-a");
  });
});

describe("the hook", () => {
  it("gives the guards the open sessions, and acts on this render's state", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    let handler!: ReturnType<typeof useProjectWindowRemoval>;
    const Harness = ({ ctx }: { ctx: ProjectRemovalContext }) => {
      handler = useProjectWindowRemoval(ctx);
      return null;
    };
    const root = createRoot(document.createElement("div"));
    const first = context([session("a")], [newTab("a")]);
    await act(async () => root.render(createElement(Harness, { ctx: first })));
    const source = () => vi.mocked(setOpenSessionsSource).mock.lastCall![0]();
    expect(ids([...source()])).toEqual(["a"]);

    const second = context([session("b", { cwd: OTHER })], [newTab("b")]);
    await act(async () => root.render(createElement(Harness, { ctx: second })));
    expect(ids([...source()])).toEqual(["b"]);

    handler(P, { purgeData: false });
    expect(second.setRecents).toHaveBeenCalled();
    expect(first.setRecents).not.toHaveBeenCalled();
    act(() => root.unmount());
  });
});

describe("Remove from this Space (T5, T6)", () => {
  const leave = (ctx: ProjectRemovalContext, path = P) =>
    removeProject(ctx, path, { purgeData: false, leaveSpace: true });

  it("answers the refusal and changes nothing", async () => {
    vi.mocked(removeProjectFromActiveSpace).mockResolvedValue({ refusal: "No." });
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    await expect(leave(ctx, `${P}/`)).resolves.toBe("No.");
    expect(removeProjectFromActiveSpace).toHaveBeenCalledWith(P);
    expect(ctx.setProjectCwd).not.toHaveBeenCalled();
    expect(loadArchivedProjects()).toEqual([]);
    expect(purgeProjectInSpace).not.toHaveBeenCalled();
  });

  it("keeps chats and tabs, and moves the window to the next project", async () => {
    vi.mocked(removeProjectFromActiveSpace).mockResolvedValue({ removed: true });
    rememberProject(OTHER);
    rememberProject(P);
    const mine = session("mine");
    const tab = newTab(mine.id);
    const ctx = context([mine], [tab], { projectCwdRef: { current: P } });
    await expect(leave(ctx)).resolves.toBeNull();
    expect(ctx.onSelectProject).toHaveBeenCalledWith(OTHER);
    expect(ctx.setProjectCwd).toHaveBeenCalledWith(OTHER);
    expect(ids(ctx.sessionsRef.current)).toEqual(["mine"]);
    expect(ctx.tabsRef.current).toEqual([tab]);
    expect(ctx.setTabs).not.toHaveBeenCalled();
    expect(loadRecents().map((item) => item.path)).toEqual([P, OTHER]);
  });

  it("opens a blank tab on no project when nothing is left, keeping the hidden tabs", async () => {
    vi.mocked(removeProjectFromActiveSpace).mockResolvedValue({ removed: true });
    rememberProject(P);
    const mine = session("mine", { runtimeMode: "full-access" as Session["runtimeMode"] });
    const tab = newTab(mine.id);
    const ctx = context([mine], [tab], { projectCwdRef: { current: P } });
    await leave(ctx);
    expect(ctx.setProjectCwd).toHaveBeenCalledWith("~");
    expect(ctx.setComposerFocused).toHaveBeenCalledWith(true);
    const [kept, blank] = ctx.sessionsRef.current;
    expect(kept).toBe(mine);
    expect(blank.cwd).toBe("~");
    expect(blank.runtimeMode).toBe("full-access");
    const [oldTab, newOne] = ctx.tabsRef.current;
    expect(oldTab).toBe(tab);
    expect(newOne.id).not.toBe(tab.id);
    expect(ctx.activeTabIdRef.current).toBe(newOne.id);
    expect(ctx.setSessions).toHaveBeenCalledWith([mine, blank]);
    expect(ctx.setTabs).toHaveBeenCalledWith([tab, newOne]);
    expect(ctx.setActiveTabId).toHaveBeenCalledWith(newOne.id);
  });

  it("stays put when another project is open", async () => {
    vi.mocked(removeProjectFromActiveSpace).mockResolvedValue({ removed: true });
    rememberProject(P);
    const ctx = context([], [newTab("x")]);
    await expect(leave(ctx)).resolves.toBeNull();
    expect(ctx.setProjectCwd).not.toHaveBeenCalled();
    expect(ctx.setTabs).not.toHaveBeenCalled();
  });

  it("stays put when the project was not in this Space", async () => {
    vi.mocked(removeProjectFromActiveSpace).mockResolvedValue({ removed: false });
    rememberProject(P);
    const ctx = context([], [newTab("x")], { projectCwdRef: { current: P } });
    await expect(leave(ctx)).resolves.toBeNull();
    expect(ctx.setProjectCwd).not.toHaveBeenCalled();
    expect(ctx.setTabs).not.toHaveBeenCalled();
  });
});
