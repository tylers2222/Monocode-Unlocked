import { describe, expect, it } from "vitest";
import { newFileTab, newTab, placePane, type WorkspaceTab } from "../../workspace/model/layout";
import { newSession, type Session } from "../../sessions/model/session";
import { planProjectReturn, reconcileProjectReturn } from "./projectReturn";
import {
  emptyTabVisitHistory,
  recordTabVisit,
  tabVisitBack,
} from "../../workspace/model/tabVisitHistory";
import { removeSessionFromWorkspace } from "../../sessions/model/sessionWorkspaceLifecycle";
import { applyPlaceSessionOnPane } from "../../workspace/model/workspaceTabGroups";

function chat(id: string, cwd: string): Session {
  return {
    ...newSession("cursor", cwd),
    id,
    blocks: [{ id: `${id}-user`, role: "user", text: id }],
  };
}

function workspace() {
  const sessions = [
    chat("a1", "/alpha"),
    chat("a2", "/alpha"),
    chat("b1", "/beta"),
    chat("b2", "/beta"),
  ];
  const tabs = sessions.map((session) => ({
    ...newTab(session.id),
    id: `tab-${session.id}`,
  }));
  return {
    sessions,
    tabs,
    activeTabId: "tab-b2",
    memory: new Map<string, string>(),
    excludedIds: new Set<string>(),
  };
}

describe("project return memory", () => {
  it("records visits independently and changes on explicit selection or Back", () => {
    const state = workspace();
    const alpha = reconcileProjectReturn({ ...state, activeTabId: "tab-a2" });
    const beta = reconcileProjectReturn({ ...state, memory: alpha });
    expect([...beta]).toEqual([
      ["/alpha", "a2"],
      ["/beta", "b2"],
    ]);
    const changed = reconcileProjectReturn({
      ...state,
      memory: beta,
      activeTabId: "tab-a1",
    });
    expect(changed.get("/alpha")).toBe("a1");
    const history = recordTabVisit(emptyTabVisitHistory("tab-a2"), "tab-b2");
    const back = tabVisitBack(history);
    expect(back).not.toBeNull();
    const returned = reconcileProjectReturn({
      ...state,
      memory: changed,
      activeTabId: back?.current ?? "",
    });
    expect(returned.get("/alpha")).toBe("a2");
    expect(returned.get("/beta")).toBe("b2");
    expect(state.memory.size).toBe(0);
    expect(alpha.size).toBe(1);
  });

  it("does not change identity for repeated observation or streaming updates", () => {
    const state = workspace();
    const memory = reconcileProjectReturn(state);
    state.sessions[0].blocks.push({
      id: "response",
      role: "assistant",
      text: "working",
    });
    state.sessions[0].busy = true;
    expect(reconcileProjectReturn({ ...state, memory })).toBe(memory);
  });

  it("prunes removed and retargeted tabs without changing the input map", () => {
    const state = workspace();
    const memory = new Map([
      ["/alpha", "a2"],
      ["/gone", "missing"],
      ["/beta", "b2"],
    ]);
    state.sessions[1].cwd = "/gamma";
    expect([...reconcileProjectReturn({ ...state, memory })]).toEqual([
      ["/beta", "b2"],
    ]);
    expect(memory.size).toBe(3);
  });

  it("records editor-only tabs and keeps the selected file pane", () => {
    const file = newFileTab("/gamma/readme.md", "/gamma");
    const tab = {
      ...newTab("editor"),
      id: "files",
      editorPanes: [{ id: "editor", files: [file], activeFileId: file.id }],
    };
    const state = workspace();
    const tabs = [...state.tabs, tab];
    const memory = reconcileProjectReturn({
      ...state,
      tabs,
      activeTabId: tab.id,
    });
    expect(
      planProjectReturn({ ...state, tabs, memory, projectPath: "/gamma" }),
    ).toEqual({ action: "activate", tabId: tab.id, paneId: "editor" });
    expect(tab.focusedId).toBe("editor");
  });

  it("records canonical Windows keys and does not combine matching display names", () => {
    const state = workspace();
    state.sessions[1].cwd = "C:/Work/Alpha/";
    const memory = reconcileProjectReturn({ ...state, activeTabId: "tab-a2" });
    expect(memory.get("c:/work/alpha")).toBe("a2");
    expect(
      planProjectReturn({ ...state, memory, projectPath: "c:\\work\\ALPHA" }),
    ).toEqual({ action: "activate", tabId: "tab-a2", paneId: "a2" });
    expect(
      planProjectReturn({ ...state, memory, projectPath: "/elsewhere/Alpha" }),
    ).toEqual({ action: "create" });
  });

  it("validates the result of actual session removal and preserves close replacement", () => {
    const state = workspace();
    const memory = new Map([
      ["/alpha", "a2"],
      ["/beta", "b2"],
    ]);
    const removed = removeSessionFromWorkspace({
      ...state,
      sessionId: "a2",
      scope: "project",
      createReplacement: (seed) => chat("replacement", seed?.cwd ?? "/alpha"),
    });
    const next = reconcileProjectReturn({ ...removed, memory });
    expect(
      planProjectReturn({
        ...removed,
        memory: next,
        excludedIds: state.excludedIds,
        projectPath: "/alpha",
      }),
    ).toEqual({ action: "activate", tabId: "tab-a1", paneId: "a1" });
    const activeRemoved = removeSessionFromWorkspace({
      ...state,
      activeTabId: "tab-a2",
      sessionId: "a2",
      scope: "project",
      createReplacement: (seed) => chat("replacement", seed?.cwd ?? "/alpha"),
    });
    expect(
      reconcileProjectReturn({ ...activeRemoved, memory }).get("/alpha"),
    ).toBe("a1");
  });

  it("returns to the focused session after actual pane placement", () => {
    const state = workspace();
    const placed = applyPlaceSessionOnPane({
      tabs: state.tabs,
      sessions: state.sessions,
      sessionId: "a2",
      targetId: "a1",
      edge: "right",
      replaceTarget: false,
      scope: "workspace",
      createReplacement: (seed) => chat("replacement", seed?.cwd ?? "/alpha"),
    });
    expect(placed).not.toBeNull();
    if (!placed) throw new Error("Expected placed workspace");
    const memory = reconcileProjectReturn({
      ...placed,
      memory: new Map([["/alpha", "a2"]]),
    });
    const beta = reconcileProjectReturn({
      ...placed,
      memory,
      activeTabId: "tab-b2",
    });
    const decision = planProjectReturn({
      ...placed,
      excludedIds: state.excludedIds,
      memory: beta,
      activeTabId: "tab-b2",
      projectPath: "/alpha",
    });
    expect(decision).toEqual({ action: "activate", tabId: "tab-a1", paneId: "a2" });
    expect(placed.tabs.find((tab) => tab.id === "tab-a1")?.focusedId).toBe(
      "a2",
    );
  });

  it("drops a removed project's choice rather than reopening it", () => {
    const state = workspace();
    const memory = new Map([["/alpha", "a2"]]);
    const remaining = {
      ...state,
      tabs: state.tabs.filter((tab) => tab.id.startsWith("tab-b")),
    };
    const next = reconcileProjectReturn({ ...remaining, memory });
    expect(next.has("/alpha")).toBe(false);
    expect(
      planProjectReturn({ ...remaining, memory: next, projectPath: "/alpha" }),
    ).toEqual({ action: "create" });
  });

  it("does not record a missing active tab or a projectless tab", () => {
    const state = workspace();
    expect(reconcileProjectReturn({ ...state, activeTabId: "missing" })).toBe(
      state.memory,
    );
    state.sessions[3].cwd = "~";
    expect(reconcileProjectReturn(state)).toBe(state.memory);
  });
});

describe("project selection", () => {
  it.each(["missing", "tab-b1"])("rejects invalid Alpha target %s", (tabId) => {
    const state = workspace();
    state.memory.set("/alpha", tabId);
    expect(planProjectReturn({ ...state, projectPath: "/alpha" })).toEqual({
      action: "activate",
      tabId: "tab-a1",
      paneId: "a1",
    });
  });

  it("keeps the focused project", () => {
    expect(
      planProjectReturn({ ...workspace(), projectPath: "/beta/" }),
    ).toEqual({ action: "keep" });
  });

  it("selects the first destination tab without a remembered choice", () => {
    expect(
      planProjectReturn({ ...workspace(), projectPath: "/alpha" }),
    ).toEqual({ action: "activate", tabId: "tab-a1", paneId: "a1" });
  });

  it("prefers an existing destination over a blank source", () => {
    const state = workspace();
    state.sessions[3].blocks = [];
    expect(planProjectReturn({ ...state, projectPath: "/alpha" })).toEqual({
      action: "activate",
      tabId: "tab-a1",
      paneId: "a1",
    });
  });

  it("returns to A2 and B2 independently of tab order", () => {
    const state = workspace();
    state.memory = new Map([
      ["/alpha", "a2"],
      ["/beta", "b2"],
    ]);
    expect(planProjectReturn({ ...state, projectPath: "/alpha" })).toEqual({
      action: "activate",
      tabId: "tab-a2",
      paneId: "a2",
    });
    expect(
      planProjectReturn({
        ...state,
        activeTabId: "tab-a2",
        projectPath: "/beta",
      }),
    ).toEqual({ action: "activate", tabId: "tab-b2", paneId: "b2" });
    expect(
      planProjectReturn({
        ...state,
        tabs: [...state.tabs].reverse(),
        projectPath: "/alpha",
      }),
    ).toEqual({ action: "activate", tabId: "tab-a2", paneId: "a2" });
  });

  it("reuses a blank only when the destination has no open tab", () => {
    const state = workspace();
    state.sessions[3].blocks = [];
    expect(planProjectReturn({ ...state, projectPath: "/gamma" })).toEqual({
      action: "reuse-blank",
      sessionId: "b2",
    });
  });

  it("never reuses a busy session even when it has no user blocks", () => {
    const state = workspace();
    state.sessions[3].blocks = [];
    state.sessions[3].busy = true;
    expect(planProjectReturn({ ...state, projectPath: "/gamma" })).toEqual({
      action: "create",
    });
  });

  it("creates a session when no destination exists and the current chat is not blank", () => {
    expect(
      planProjectReturn({ ...workspace(), projectPath: "/gamma" }),
    ).toEqual({ action: "create" });
  });

  it("keeps a focused Beta session in an Alpha-first split", () => {
    const state = workspace();
    const first = state.tabs[0];
    state.tabs[0] = {
      ...first,
      layout: placePane(first.layout, "b2", "a1", "right"),
      focusedId: "b2",
    };
    state.tabs = state.tabs.filter((tab) => tab.id !== "tab-b2");
    expect(
      planProjectReturn({
        ...state,
        activeTabId: first.id,
        projectPath: "/beta",
      }),
    ).toEqual({ action: "keep" });
  });

  it("keeps a focused file in the selected project", () => {
    const state = workspace();
    const file = newFileTab("/gamma/readme.md", "/gamma");
    const tab: WorkspaceTab = {
      ...newTab("editor"),
      id: "files",
      editorPanes: [{ id: "editor", files: [file], activeFileId: file.id }],
    };
    expect(
      planProjectReturn({
        ...state,
        tabs: [...state.tabs, tab],
        activeTabId: tab.id,
        projectPath: "/gamma",
      }),
    ).toEqual({ action: "keep" });
  });
});

describe("project selection with a running session", () => {
  function turn(
    id: string,
    startedAt?: number,
    busy = false,
    cwd = "/mono",
  ): Session {
    return {
      ...newSession("claude", cwd),
      id,
      busy,
      blocks: [{ id: `${id}-user`, role: "user", text: id, startedAt }],
    };
  }

  function plan(
    sessions: Session[],
    memory = new Map<string, string>(),
    activeTabId = "tab-other",
    tabless: readonly string[] = [],
    excluded: readonly string[] = [],
  ) {
    const all = [...sessions, turn("other", 0, false, "/other")];
    const tabs = all
      .filter(
        (session) =>
          !session.orchestrationLeadId && !tabless.includes(session.id),
      )
      .map((session) => ({ ...newTab(session.id), id: `tab-${session.id}` }));
    return planProjectReturn({
      memory,
      tabs,
      sessions: all,
      activeTabId,
      excludedIds: new Set(excluded),
      projectPath: "/mono",
    });
  }

  const activates = (id: string) => ({
    action: "activate",
    tabId: `tab-${id}`,
    paneId: id,
  });

  it("opens the running session instead of the remembered idle one", () => {
    expect(
      plan(
        [turn("idle", 1), turn("running", 1, true)],
        new Map([["/mono", "idle"]]),
      ),
    ).toEqual(activates("running"));
  });

  it("opens a running session in a later tab when nothing is remembered", () => {
    expect(plan([turn("idle", 1), turn("running", 1, true)])).toEqual(
      activates("running"),
    );
  });

  it("opens the running session whose turn started last", () => {
    expect(
      plan(
        [turn("idle", 9), turn("older", 1, true), turn("newer", 2, true)],
        new Map([["/mono", "idle"]]),
      ),
    ).toEqual(activates("newer"));
  });

  it("keeps the remembered session when it is one of the running ones", () => {
    expect(
      plan(
        [turn("newer", 2, true), turn("older", 1, true)],
        new Map([["/mono", "older"]]),
      ),
    ).toEqual(activates("older"));
  });

  it("keeps today's first-tab choice when nothing runs", () => {
    expect(plan([turn("first", 1), turn("second", 2)])).toEqual(
      activates("first"),
    );
  });

  it("leaves the project's focused idle session for one that runs", () => {
    expect(
      plan([turn("idle", 1), turn("running", 2, true)], new Map(), "tab-idle"),
    ).toEqual(activates("running"));
  });

  it("stays on the project's focused session while it runs", () => {
    expect(
      plan(
        [turn("focused", 1, true), turn("newer", 2, true)],
        new Map(),
        "tab-focused",
      ),
    ).toEqual({ action: "keep" });
  });

  it("opens a running session that has no tab", () => {
    expect(
      plan(
        [turn("idle", 1), turn("started", 2, true)],
        new Map([["/mono", "idle"]]),
        "tab-other",
        ["started"],
      ),
    ).toEqual({ action: "open-session", sessionId: "started" });
  });

  it("leaves the focused idle session for a running one with no tab", () => {
    expect(
      plan(
        [turn("idle", 1), turn("started", 2, true)],
        new Map(),
        "tab-idle",
        ["started"],
      ),
    ).toEqual({ action: "open-session", sessionId: "started" });
  });

  it("ranks running sessions with and without a tab by turn start", () => {
    const sessions = [turn("tabbed", 2, true), turn("started", 3, true)];
    expect(plan(sessions, new Map(), "tab-other", ["started"])).toEqual({
      action: "open-session",
      sessionId: "started",
    });
    sessions[0] = turn("tabbed", 4, true);
    expect(plan(sessions, new Map(), "tab-other", ["started"])).toEqual(
      activates("tabbed"),
    );
  });

  it("opens the lead with no tab of a running worker", () => {
    const worker = { ...turn("worker", 3, true), orchestrationLeadId: "lead" };
    expect(
      plan([turn("idle", 1), turn("lead", 2), worker], new Map(), "tab-other", [
        "lead",
      ]),
    ).toEqual({ action: "open-session", sessionId: "lead" });
  });

  it.each([
    ["an Inbox Ask", { inboxAsk: { kind: "pr" } }, []],
    ["a session whose worktree was removed", { worktreeRemoved: true }, []],
    ["an excluded session (Mono chat, habit run, ephemeral)", {}, ["started"]],
  ])("does not open %s that runs with no tab", (_, fields, excluded) => {
    const running = { ...turn("started", 2, true), ...fields } as Session;
    expect(
      plan([turn("first", 1), running], new Map(), "tab-other", ["started"], excluded),
    ).toEqual(activates("first"));
  });

  it.each([
    ["an Inbox Ask", { inboxAsk: { kind: "pr" } }, []],
    ["excluded", {}, ["lead"]],
  ])("does not open a running worker's lead that is %s", (_, fields, excluded) => {
    const lead = { ...turn("lead", 1), ...fields } as Session;
    const worker = { ...turn("worker", 3, true), orchestrationLeadId: "lead" };
    expect(
      plan([turn("first", 1), lead, worker], new Map(), "tab-other", ["lead"], excluded),
    ).toEqual(activates("first"));
  });

  it("does not open a running worker whose lead is not loaded", () => {
    const worker = { ...turn("worker", 3, true), orchestrationLeadId: "gone" };
    expect(plan([turn("first", 1), worker])).toEqual(activates("first"));
  });

  it("opens a running session whose turn has no start time", () => {
    expect(
      plan([turn("first", 1), turn("started", undefined, true)], new Map(), "tab-other", [
        "started",
      ]),
    ).toEqual({ action: "open-session", sessionId: "started" });
  });

  it("opens a lead whose worker is running", () => {
    const worker = { ...turn("worker", 3, true), orchestrationLeadId: "lead" };
    expect(plan([turn("idle", 1), turn("lead", 2), worker])).toEqual(
      activates("lead"),
    );
  });

  it("dates a lead's turn from its busy worker", () => {
    const worker = { ...turn("worker", 9, true), orchestrationLeadId: "lead" };
    expect(plan([turn("running", 5, true), turn("lead", 1), worker])).toEqual(
      activates("lead"),
    );
  });

  it("does not rank an idle lead by its own finished turn", () => {
    const worker = { ...turn("worker", 50, true), orchestrationLeadId: "lead" };
    expect(
      plan([turn("running", 80, true), turn("lead", 100), worker]),
    ).toEqual(activates("running"));
  });

  it("gives equal turn starts to the first tab", () => {
    expect(plan([turn("first", 4, true), turn("second", 4, true)])).toEqual(
      activates("first"),
    );
  });

  it("dates a steered turn from the block that started it", () => {
    const steered = turn("steered", 5, true);
    steered.blocks.push(
      { id: "reply", role: "assistant", text: "working" },
      { id: "steer", role: "user", text: "also this", sentAt: 6 },
    );
    expect(plan([turn("fresh", 3, true), steered])).toEqual(
      activates("steered"),
    );
  });
});
