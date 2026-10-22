import { describe, expect, it } from "vitest";
import { appendUser } from "../../../integrations/harness/core/apply";
import {
  CONTINUE_PROMPT,
  INTERRUPT_MESSAGE,
  canAutoContinue,
} from "../../sessions/model/inFlight";
import {
  leaf,
  leafIds,
  newAgentTab,
  newChangesTab,
  newCommitTab,
  newFileTab,
  newEditorWorkspaceTab,
  newReleaseNotesWorkspaceTab,
  newSessionChangesTab,
  newTab,
  newTerminalFile,
  splitPane,
  type WorkspaceTab,
} from "./layout";
import { createProjectTerminal } from "../../projects/model/projectTerminal";
import { newSession, type Session } from "../../sessions/model/session";
import {
  workspaceSnapshot,
  hydrateWorkspaceSnapshot,
  parseWorkspaceSnapshot,
  type WorkspaceSnapshot,
} from "./workspaceSnapshot";
import { remotePath } from "../../connections/model/remoteProjects";
import { insertTabBesideActive } from "./tabGroups";

function chat(id: string, cwd: string): Session {
  const session = newSession("cursor", cwd);
  session.id = id;
  session.blocks = [{ id: "u1", role: "user", text: "hello" }];
  session.providerSessionId = "p1";
  return session;
}

describe("project return snapshots", () => {
  it("migrates saved host file tabs to shared remote paths", () => {
    const project = "remote://env/repo";
    const file = {
      ...newFileTab("/repo/src/index.ts", "/repo", false, undefined, project),
      remoteFile: {
        machineId: "machine",
        projectId: "project",
        relativePath: "src/index.ts",
      },
    };
    const tab = newEditorWorkspaceTab(file);
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [],
        activeTabId: tab.id,
        projectCwd: project,
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const restored = parseWorkspaceSnapshot(snapshot);
    expect(restored?.tabs[0].editorPanes[0].files[0]).toMatchObject({
      path: "remote://env/repo/src/index.ts",
      cwd: project,
    });
    expect(restored?.tabs[0].editorPanes[0].files[0].remoteFile).toBeUndefined();
    const malformed = JSON.parse(JSON.stringify(snapshot));
    malformed.tabs[0].editorPanes[0].files[0].remoteFile = { machineId: 42 };
    expect(parseWorkspaceSnapshot(malformed)).toBeNull();
  });
  it("restores a host diff tab with its review state", () => {
    const project = "remote://env/repo";
    const file = {
      ...newFileTab("/repo/a.ts", "/repo", true, "staged", project),
      remoteFile: {
        machineId: "machine",
        projectId: "project",
        relativePath: "a.ts",
      },
    };
    const tab = newEditorWorkspaceTab(file);
    const snapshot = workspaceSnapshot([{ tabs: [tab], sessions: [], activeTabId: tab.id, projectCwd: project, projectReturnMemory: new Map(), projectTerminals: [], pins: new Map() }]);
    const restored = parseWorkspaceSnapshot(snapshot);
    expect(restored?.tabs[0].editorPanes[0].files[0]).toMatchObject({
      review: true,
      changeKind: "staged",
      path: "remote://env/repo/a.ts",
    });
  });
  function saved() {
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
      ...workspaceSnapshot([
        {
          tabs,
          sessions,
          activeTabId: "tab-b2",
          projectCwd: "/beta",
          projectReturnMemory: new Map(),
          projectTerminals: [],
          pins: new Map(),
        },
      ]),
      projectReturnTargets: [
        { projectPath: "/alpha", tabId: "a2" },
        { projectPath: "/beta", tabId: "b2" },
      ],
    };
  }

  it("collects and round-trips choices while rejecting stale references", () => {
    const sessions = [
      chat("a1", "/alpha"),
      chat("a2", "/alpha"),
      chat("b2", "/beta"),
    ];
    const tabs = sessions.map((session) => ({
      ...newTab(session.id),
      id: `tab-${session.id}`,
    }));
    const memory = new Map([
      ["/alpha", "a2"],
      ["/beta", "b2"],
      ["/gone", "missing"],
    ]);
    const snapshot = workspaceSnapshot([
      {
        tabs,
        sessions,
        activeTabId: "tab-b2",
        projectCwd: "/beta",
        projectReturnMemory: memory,
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const restored = hydrateWorkspaceSnapshot(snapshot, new Map());
    expect([...(restored?.projectReturnMemory ?? [])]).toEqual([
      ["/alpha", "a2"],
      ["/beta", "b2"],
    ]);
    expect(memory.size).toBe(3);
  });

  it("restores both project choices, not just the active tab", () => {
    const restored = hydrateWorkspaceSnapshot(saved(), new Map());
    expect(restored?.projectReturnMemory?.get("/alpha")).toBe("a2");
    expect(restored?.projectReturnMemory?.get("/beta")).toBe("b2");
  });

  it("loads old snapshots and seeds only the active project", () => {
    const { projectReturnTargets: _targets, ...old } = saved();
    const restored = hydrateWorkspaceSnapshot(old, new Map());
    expect([...(restored?.projectReturnMemory ?? [])]).toEqual([
      ["/beta", "b2"],
    ]);
  });

  it("prunes invalid entries and uses the last valid duplicate", () => {
    const raw = saved();
    const parsed = parseWorkspaceSnapshot({
      ...raw,
      projectReturnTargets: [
        null,
        42,
        {},
        { projectPath: "/alpha", tabId: 12 },
        { projectPath: "/alpha/", tabId: "a1" },
        { projectPath: "/alpha", tabId: "a2" },
        { projectPath: "/gone", tabId: "missing" },
        { projectPath: "/beta", tabId: "a1" },
      ],
    });
    expect(parsed?.projectReturnTargets).toEqual([
      { projectPath: "/alpha", tabId: "a2" },
    ]);
  });

  it("lets the restored active tab override inconsistent saved preference", () => {
    const raw = saved();
    raw.projectReturnTargets[1].tabId = "tab-b1";
    expect(
      hydrateWorkspaceSnapshot(raw, new Map())?.projectReturnMemory?.get(
        "/beta",
      ),
    ).toBe("b2");
  });

  it.each([null, "broken", {}])(
    "ignores a malformed choice list: %j",
    (projectReturnTargets) => {
      const restored = hydrateWorkspaceSnapshot(
        { ...saved(), projectReturnTargets },
        new Map(),
      );
      expect(restored?.tabs).toHaveLength(4);
      expect([...(restored?.projectReturnMemory ?? [])]).toEqual([
        ["/beta", "b2"],
      ]);
    },
  );

  it("rechecks project membership against loaded sessions", () => {
    const restored = hydrateWorkspaceSnapshot(
      saved(),
      new Map([["a2", chat("a2", "/moved")]]),
    );
    expect(restored?.projectReturnMemory?.has("/alpha")).toBe(false);
    expect(restored?.projectReturnMemory?.get("/beta")).toBe("b2");
  });
});

describe("workspaceSnapshot", () => {
  it("stores tabs, stubs, and the focused tab — not transcripts", () => {
    const session = chat("s1", "/tmp/a");
    session.worktreeCwd = "/tmp/a-worktrees/feature";
    session.worktreeRemoved = true;
    session.blocks.push({ id: "a1", role: "assistant", text: "hi" });
    const file = newFileTab("/tmp/a/README.md", "/tmp/a");
    const tab = {
      ...newTab("s1"),
      id: "t1",
      editorPanes: [{ id: "e1", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [session],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        // The active tab belongs to the workspace on screen (App pins it).
        pins: new Map([["t1", "/tmp/a"]]),
      },
    ]);
    expect(snapshot.activeTabId).toBe("t1");
    expect(snapshot.tabs[0]?.editorPanes[0]?.files[0]?.path).toBe(
      "/tmp/a/README.md",
    );
    expect(snapshot.sessions).toEqual([
      expect.objectContaining({
        id: "s1",
        cwd: "/tmp/a",
        providerSessionId: "p1",
        worktreeCwd: "/tmp/a-worktrees/feature",
        worktreeRemoved: true,
      }),
    ]);
    expect("blocks" in snapshot.sessions[0]!).toBe(false);
    expect(snapshot.projectTerminals).toEqual([]);
  });

  it("drops agent tabs, and the pane holding only them", () => {
    const file = newFileTab("/tmp/a/README.md", "/tmp/a");
    const agent = newAgentTab("Audit the UI", "/tmp/a", {
      sessionId: "worker",
      leadId: "s1",
      harness: "codex",
    });
    const mixed = newAgentTab("Audit the engine", "/tmp/a", {
      sessionId: "worker-2",
      leadId: "s1",
      harness: "codex",
    });
    const tab = {
      ...newTab("s1"),
      id: "t1",
      layout: splitPane(newTab("s1").layout, "s1", "right", "e1"),
      editorPanes: [
        { id: "e1", files: [agent], activeFileId: agent.id },
        { id: "e2", files: [file, mixed], activeFileId: mixed.id },
      ],
    };
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const panes = snapshot.tabs[0]!.editorPanes;
    // The agent-only pane is gone along with its leaf; the mixed one keeps its
    // file and falls back to it as the active tab.
    expect(panes.map((pane) => pane.id)).toEqual(["e2"]);
    expect(panes[0]!.files.map((entry) => entry.path)).toEqual([
      "/tmp/a/README.md",
    ]);
    expect(panes[0]!.activeFileId).toBe(file.id);
    expect(leafIds(snapshot.tabs[0]!.layout)).toEqual(["s1"]);
  });

  it("round-trips a unified Changes tab", () => {
    const file = newChangesTab("/tmp/a", "/tmp/a/src/lib.rs", "staged");
    const tab = {
      ...newTab("s1"),
      id: "t1",
      editorPanes: [{ id: "e1", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([{ tabs: [tab], sessions: [], activeTabId: "t1", projectCwd: "/tmp/a", projectReturnMemory: new Map(), projectTerminals: [], pins: new Map() }]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());
    const restored = workspace?.tabs[0]?.editorPanes[0]?.files[0];
    expect(restored?.changes).toBe(true);
    expect(restored?.review).toBe(true);
    expect(restored?.path).toBe("/tmp/a/src/lib.rs");
    expect(restored?.changeKind).toBe("staged");
  });

  it("round-trips a session-scoped Changes tab", () => {
    const file = newSessionChangesTab(
      "/tmp/a",
      "session-a",
      "/tmp/a/src/lib.rs",
    );
    const tab = {
      ...newTab("s1"),
      id: "t1",
      editorPanes: [{ id: "e1", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([{ tabs: [tab], sessions: [], activeTabId: "t1", projectCwd: "/tmp/a", projectReturnMemory: new Map(), projectTerminals: [], pins: new Map() }]);
    const restored = hydrateWorkspaceSnapshot(snapshot, new Map())?.tabs[0]
      ?.editorPanes[0]?.files[0];
    expect(restored?.sessionChanges).toEqual({ sessionId: "session-a" });
    expect(restored?.review).toBe(true);
    expect(restored?.path).toBe("/tmp/a/src/lib.rs");
  });

  it("preserves a worktree editor's execution directory and owning project", () => {
    const file = newFileTab(
      "/repo-worktrees/feature/readme.md",
      "/repo-worktrees/feature",
      false,
      undefined,
      "/repo",
    );
    const tab = {
      ...newTab("editor"),
      editorPanes: [{ id: "editor", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [],
        activeTabId: tab.id,
        projectCwd: "/repo",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const restored = hydrateWorkspaceSnapshot(snapshot, new Map())?.tabs[0]
      ?.editorPanes[0]?.files[0];
    expect(restored).toMatchObject({
      cwd: "/repo-worktrees/feature",
      projectCwd: "/repo",
    });
  });

  it("round-trips a commit review tab", () => {
    const file = newCommitTab("/tmp/a", {
      sha: "abc1234deadbeef",
      shortSha: "abc1234",
      subject: "Fix the graph",
    });
    const tab = {
      ...newTab("s1"),
      id: "t1",
      editorPanes: [{ id: "e1", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([{ tabs: [tab], sessions: [], activeTabId: "t1", projectCwd: "/tmp/a", projectReturnMemory: new Map(), projectTerminals: [], pins: new Map() }]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());
    const restored = workspace?.tabs[0]?.editorPanes[0]?.files[0];
    expect(restored?.commit).toEqual({
      sha: "abc1234deadbeef",
      shortSha: "abc1234",
      subject: "Fix the graph",
    });
  });

  it("round-trips a release-note descriptor", () => {
    const tab = newReleaseNotesWorkspaceTab({ version: "0.1.22" });
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [],
        activeTabId: tab.id,
        projectCwd: "~",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());

    expect(workspace?.tabs[0]?.editorPanes[0]?.files[0]?.releaseNotes).toEqual({
      version: "0.1.22",
    });
    expect(workspace?.sessions).toEqual([]);
  });

  it("stores the project terminal dock", () => {
    const term = newTerminalFile("/tmp/a", "zsh");
    const dock = createProjectTerminal("/tmp/a", term);
    const snapshot = workspaceSnapshot([
      {
        tabs: [{ ...newTab("s1"), id: "t1" }],
        sessions: [],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [dock],
        pins: new Map(),
      },
    ]);
    expect(snapshot.projectTerminals).toEqual([
      expect.objectContaining({
        projectPath: "/tmp/a",
        side: "bottom",
        open: true,
      }),
    ]);
    expect(snapshot.projectTerminals[0]?.pane.files[0]?.id).toBe(term.id);
  });

  it("stores the last dock side for new projects", () => {
    const term = newTerminalFile("/tmp/a", "zsh");
    const dock = createProjectTerminal("/tmp/a", term);
    const snapshot = workspaceSnapshot([
      {
        tabs: [{ ...newTab("s1"), id: "t1" }],
        sessions: [],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [dock],
        lastDockSide: "right",
        pins: new Map(),
      },
    ]);
    expect(snapshot.lastDockSide).toBe("right");
    expect(
      workspaceSnapshot([
        {
          tabs: [{ ...newTab("s1"), id: "t1" }],
          sessions: [],
          activeTabId: "t1",
          projectCwd: "/tmp/a",
          projectReturnMemory: new Map(),
          projectTerminals: [],
          pins: new Map(),
        },
      ]).lastDockSide,
    ).toBeUndefined();
  });
});

describe("parseWorkspaceSnapshot", () => {
  it("returns null for empty or invalid payloads", () => {
    expect(parseWorkspaceSnapshot(null)).toBeNull();
    expect(parseWorkspaceSnapshot({ tabs: [], activeTabId: "t1" })).toBeNull();
    expect(
      parseWorkspaceSnapshot({ tabs: [{}], activeTabId: "t1" }),
    ).toBeNull();
  });

  it("keeps a valid last dock side and drops an invalid one", () => {
    const term = newTerminalFile("/tmp/a", "zsh");
    const snapshot = workspaceSnapshot([
      {
        tabs: [{ ...newTab("s1"), id: "t1" }],
        sessions: [],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [createProjectTerminal("/tmp/a", term)],
        lastDockSide: "right",
        pins: new Map(),
      },
    ]);
    const raw = JSON.parse(JSON.stringify(snapshot)) as Record<string, unknown>;
    expect(parseWorkspaceSnapshot(raw)?.lastDockSide).toBe("right");
    raw.lastDockSide = "diagonal";
    expect(parseWorkspaceSnapshot(raw)?.lastDockSide).toBeUndefined();
  });

  it("drops unknown fields and repairs a missing active tab", () => {
    const tab = { ...newTab("s1"), id: "t1" };
    const parsed = parseWorkspaceSnapshot({
      tabs: [{ ...tab, extra: true }],
      sessions: [
        {
          id: "s1",
          harness: "cursor",
          runtimeMode: "supervised",
          cwd: "/tmp/a",
        },
      ],
      activeTabId: "missing",
      projectCwd: "/tmp/a",
    });
    expect(parsed?.activeTabId).toBe("t1");
    expect(parsed?.tabs[0] && "extra" in parsed.tabs[0]).toBe(false);
  });

  it.each([
    { releaseNotes: { version: "" } },
    { releaseNotes: { version: 123 } },
    {
      releaseNotes: { version: "0.1.22" },
      plan: { sessionId: "s", blockId: "b", title: "Plan" },
    },
    { releaseNotes: { version: "0.1.22" }, review: true },
    { releaseNotes: { version: "0.1.22" }, changes: true },
    { releaseNotes: { version: "0.1.22" }, terminal: true },
    {
      releaseNotes: { version: "0.1.22" },
      commit: { sha: "abc", shortSha: "abc", subject: "x" },
    },
  ])("rejects a tab whose release pane is invalid: %j", (descriptor) => {
    const valid = { ...newTab("session-a"), id: "valid-tab" };
    const invalidPaneId = "invalid-release-pane";
    const invalid = {
      kind: "session",
      id: "invalid-tab",
      layout: leaf(invalidPaneId),
      focusedId: invalidPaneId,
      editorPanes: [
        {
          id: invalidPaneId,
          activeFileId: "release-file",
          files: [
            {
              id: "release-file",
              path: "release-notes:0.1.22",
              cwd: "~",
              ...descriptor,
            },
          ],
        },
      ],
      terminalPanes: [],
    };

    const parsed = parseWorkspaceSnapshot({
      tabs: [valid, invalid],
      sessions: [],
      activeTabId: "invalid-tab",
      projectCwd: "~",
    });
    expect(parsed?.tabs.map((tab) => tab.id)).toEqual(["valid-tab"]);

    const workspace = parsed && hydrateWorkspaceSnapshot(parsed, new Map());
    expect(
      workspace?.sessions.some((session) => session.id === invalidPaneId),
    ).toBe(false);
  });
});

describe("hydrateWorkspaceSnapshot", () => {
  it("reopens splits, file panes, and stored transcripts", () => {
    const left = chat("s1", "/tmp/a");
    const right = chat("s2", "/tmp/a");
    const file = newFileTab("/tmp/a/src/lib.rs", "/tmp/a");
    const tab = {
      ...newTab("s1"),
      id: "t1",
      layout: {
        type: "split" as const,
        id: "split1",
        dir: "right" as const,
        children: [leaf("s1"), leaf("e1")],
        sizes: [0.5, 0.5],
      },
      focusedId: "e1",
      editorPanes: [{ id: "e1", files: [file], activeFileId: file.id }],
    };
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [left, right],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const loaded = new Map([
      [
        "s1",
        {
          ...left,
          blocks: [
            ...left.blocks,
            { id: "a1", role: "assistant" as const, text: "stored" },
          ],
        },
      ],
    ]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, loaded);
    expect(workspace?.tabs).toHaveLength(1);
    expect(workspace?.tabs[0]?.layout).toEqual(tab.layout);
    expect(workspace?.tabs[0]?.editorPanes[0]?.files[0]?.path).toBe(
      "/tmp/a/src/lib.rs",
    );
    expect(
      workspace?.sessions.find((session) => session.id === "s1")?.blocks,
    ).toEqual(loaded.get("s1")?.blocks);
    expect(
      workspace?.sessions.find((session) => session.id === "s2")?.blocks,
    ).toEqual([]);
  });

  it("marks in-flight chats interrupted and adds a tab if they were parked", () => {
    const open = chat("s1", "/tmp/a");
    const parked = chat("s2", "/tmp/a");
    parked.busy = true;
    const snapshot = workspaceSnapshot([
      {
        tabs: [{ ...newTab("s1"), id: "t1" }],
        sessions: [open, parked],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const workspace = hydrateWorkspaceSnapshot(
      snapshot,
      new Map([
        ["s1", open],
        ["s2", parked],
      ]),
      new Set(["s2"]),
    );
    expect(workspace?.tabs).toHaveLength(2);
    const resumed = workspace?.sessions.find((session) => session.id === "s2");
    expect(resumed?.busy).toBe(false);
    expect(
      resumed?.blocks.some((block) => block.text === INTERRUPT_MESSAGE),
    ).toBe(true);
  });

  it("continues a Codex snapshot with its saved model and settings before discovery", () => {
    const session: Session = {
      ...chat("s1", "/tmp/a"),
      harness: "codex",
      model: "codex:gpt-5.6-sol",
      modelSettings: { reasoningEffort: "high", serviceTier: "priority" },
    };
    const tab = newTab(session.id);
    const snapshot = workspaceSnapshot([
      {
        tabs: [tab],
        sessions: [session],
        activeTabId: tab.id,
        projectCwd: session.cwd,
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    const restored = hydrateWorkspaceSnapshot(
      snapshot,
      new Map(),
      new Set([session.id]),
    )?.sessions[0];
    expect(restored).toBeDefined();
    expect(restored?.model).toBe("codex:gpt-5.6-sol");
    expect(restored?.modelSettings).toEqual(session.modelSettings);
    expect(canAutoContinue(restored!)).toBe(true);
    const continued = appendUser(restored!, CONTINUE_PROMPT);
    expect(continued.blocks.at(-1)?.turnModel).toEqual({
      harness: "codex",
      id: "codex:gpt-5.6-sol",
      name: "GPT-5.6-Sol",
    });
  });

  it("keeps terminal-only tabs", () => {
    const term = newTerminalFile("/tmp/a");
    const tab = {
      kind: "session" as const,
      id: "t1",
      layout: leaf("p1"),
      focusedId: "p1",
      editorPanes: [],
      terminalPanes: [{ id: "p1", files: [term], activeFileId: term.id }],
    };
    const snapshot = workspaceSnapshot([{ tabs: [tab], sessions: [], activeTabId: "t1", projectCwd: "/tmp/a", projectReturnMemory: new Map(), projectTerminals: [], pins: new Map() }]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());
    expect(workspace?.tabs[0]?.terminalPanes[0]?.files[0]?.terminal).toBe(true);
  });

  it("restores a project terminal dock", () => {
    const term = { ...newTerminalFile("/tmp/a"), foreground: "vite" };
    const dock = {
      ...createProjectTerminal("/tmp/a", term),
      side: "left" as const,
      size: 300,
      open: false,
    };
    const snapshot = workspaceSnapshot([{ tabs: [{ ...newTab("s1"), id: "t1" }], sessions: [], activeTabId: "t1", projectCwd: "/tmp/a", projectReturnMemory: new Map(), projectTerminals: [dock], pins: new Map() }]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());
    expect(workspace?.projectTerminals).toEqual([
      expect.objectContaining({
        projectPath: "/tmp/a",
        side: "left",
        size: 300,
        open: false,
      }),
    ]);
    expect(workspace?.projectTerminals?.[0]?.pane.files[0]?.terminal).toBe(
      true,
    );
    expect(
      workspace?.projectTerminals?.[0]?.pane.files[0]?.foreground,
    ).toBeUndefined();
  });

  it("restores the last dock side", () => {
    const term = newTerminalFile("/tmp/a", "zsh");
    const snapshot = workspaceSnapshot([
      {
        tabs: [{ ...newTab("s1"), id: "t1" }],
        sessions: [],
        activeTabId: "t1",
        projectCwd: "/tmp/a",
        projectReturnMemory: new Map(),
        projectTerminals: [createProjectTerminal("/tmp/a", term)],
        lastDockSide: "left",
        pins: new Map(),
      },
    ]);
    const workspace = hydrateWorkspaceSnapshot(snapshot, new Map());
    expect(workspace?.lastDockSide).toBe("left");
  });
});

describe("worktree tabs", () => {
  const window = (
    tabs: WorkspaceTab[],
    sessions: Session[],
    activeTabId: string,
    projectCwd: string,
    pins: [string, string][],
    projectReturnMemory: ReadonlyMap<string, string> = new Map(),
  ) => ({
    tabs,
    sessions,
    activeTabId,
    projectCwd,
    projectReturnMemory,
    projectTerminals: [],
    pins: new Map(pins),
  });
  const inTree = (id: string, cwd: string, tree: string) => ({
    ...chat(id, cwd),
    worktreeCwd: tree,
  });
  const restoredPins = (snapshot: WorkspaceSnapshot, loaded: Session[] = []) =>
    Object.fromEntries(
      hydrateWorkspaceSnapshot(
        snapshot,
        new Map(loaded.map((session) => [session.id, session])),
      )!.pins,
    );

  it("saves a tab pinned to a worktree with that worktree, and restore pins it back while its session works there", () => {
    const main = chat("main", "/repo");
    const feature = inTree("feature", "/repo", "/trees/a");
    const idle = chat("idle", "/repo");
    const tabs = [
      { ...newTab("main"), id: "tab-main" },
      { ...newTab("feature"), id: "tab-feature" },
      { ...newTab("idle"), id: "tab-idle" },
    ];
    const snapshot = workspaceSnapshot([
      window(tabs, [main, feature, idle], "tab-feature", "/repo", [
        ["tab-main", "/repo"],
        ["tab-feature", "/trees/a"],
        // Opened while /trees/a was shown, but it works in the checkout, so
        // it would restore in the default workspace and is saved there.
        ["tab-idle", "/trees/a"],
      ]),
    ]);
    expect(snapshot.tabs.map((tab) => tab.id)).toEqual([
      "tab-main",
      "tab-feature",
      "tab-idle",
    ]);
    expect(snapshot.tabWorkspaces).toEqual([
      { tabId: "tab-feature", path: "/trees/a" },
    ]);
    expect(snapshot.activeTabId).toBe("tab-main");
    expect(restoredPins(snapshot)).toEqual({
      "tab-main": "/repo",
      "tab-feature": "/trees/a",
      "tab-idle": "/repo",
    });
    expect(
      restoredPins(snapshot, [{ ...feature, worktreeRemoved: true }]),
    ).toEqual({
      "tab-main": "/repo",
      "tab-feature": "/repo",
      "tab-idle": "/repo",
    });
  });

  it("saves a tab pinned to a removed worktree in the default workspace, where it restores, so it can stay active and a return target", () => {
    const tabs = [
      { ...newTab("main"), id: "tab-main" },
      { ...newTab("gone"), id: "tab-gone" },
    ];
    const gone = {
      ...inTree("gone", "/repo", "/trees/a"),
      worktreeRemoved: true,
    };
    const snapshot = workspaceSnapshot([
      window(
        tabs,
        [chat("main", "/repo"), gone],
        "tab-gone",
        "/repo",
        [
          ["tab-main", "/repo"],
          ["tab-gone", "/trees/a"],
        ],
        new Map([["/repo", "gone"]]),
      ),
    ]);
    expect({
      tabWorkspaces: snapshot.tabWorkspaces,
      active: snapshot.activeTabId,
      targets: snapshot.projectReturnTargets,
      pins: restoredPins(snapshot, [gone]),
    }).toEqual({
      tabWorkspaces: undefined,
      active: "tab-gone",
      targets: [{ projectPath: "/repo", tabId: "gone" }],
      pins: { "tab-main": "/repo", "tab-gone": "/repo" },
    });
  });

  it("with no default-workspace tab, saves no active tab, and boot keeps it that way with every tab in its worktree", () => {
    const tabs = [
      { ...newTab("w"), id: "tab-w" },
      { ...newTab("x"), id: "tab-x" },
    ];
    const snapshot = workspaceSnapshot([
      window(
        tabs,
        [inTree("w", "/repo", "/trees/a"), inTree("x", "/repo", "/trees/b")],
        "tab-w",
        "/repo",
        [
          ["tab-w", "/trees/a"],
          ["tab-x", "/trees/b"],
        ],
      ),
    ]);
    const restored = hydrateWorkspaceSnapshot(snapshot, new Map())!;
    expect({
      saved: snapshot.activeTabId,
      active: restored.activeTabId,
      pins: Object.fromEntries(restored.pins),
    }).toEqual({
      saved: "",
      active: "",
      pins: { "tab-w": "/trees/a", "tab-x": "/trees/b" },
    });
  });

  it("saves the window's project, dock and dock side, and never saves or restores a remote tab in a worktree", () => {
    const host = remotePath("env-1", "/srv/app");
    const hostTree = remotePath("env-1", "/srv/app-worktrees/x");
    const term = newTerminalFile("/repo", "zsh");
    const tabs = [
      { ...newTab("main"), id: "tab-main" },
      { ...newTab("remote"), id: "tab-remote" },
    ];
    const snapshot = workspaceSnapshot([
      {
        ...window(
          tabs,
          [chat("main", "/repo"), inTree("remote", host, hostTree)],
          "tab-main",
          "/repo",
          [["tab-remote", hostTree]],
        ),
        projectTerminals: [createProjectTerminal("/repo", term)],
        lastDockSide: "left",
      },
    ]);
    expect({
      projectCwd: snapshot.projectCwd,
      lastDockSide: snapshot.lastDockSide,
      dock: snapshot.projectTerminals.map((dock) => dock.pane.files[0]?.id),
      tabWorkspaces: snapshot.tabWorkspaces,
      pins: restoredPins(snapshot),
    }).toEqual({
      projectCwd: "/repo",
      lastDockSide: "left",
      dock: [term.id],
      tabWorkspaces: undefined,
      pins: { "tab-main": "/repo" },
    });
  });

  it("saves as active the nearest default-workspace tab before a worktree tab, else after it", () => {
    const ids = ["w1", "a", "w2", "b"];
    const tabs = ids.map((id) => ({ ...newTab(id), id }));
    const save = (activeTabId: string, inWorktree: string[]) =>
      workspaceSnapshot([
        window(
          tabs,
          ids.map((id) =>
            inWorktree.includes(id)
              ? inTree(id, "/repo", `/trees/${id}`)
              : chat(id, "/repo"),
          ),
          activeTabId,
          "/repo",
          inWorktree.map((id) => [id, `/trees/${id}`]),
        ),
      ]).activeTabId;
    expect(save("b", ["w1", "w2"])).toBe("b");
    expect(save("w2", ["w1", "w2"])).toBe("a");
    expect(save("w1", ["w1", "w2"])).toBe("a");
  });

  it("looks for the nearest default-workspace tab in the saved project only", () => {
    const sessions = [
      chat("s1", "/p"),
      chat("s2", "/q"),
      inTree("s3", "/p", "/p-trees/w1"),
      chat("s4", "/p"),
    ];
    // onSelectHistorySession: the project switches to /p, then the running
    // session's tab opens beside the still-active /q tab.
    const tabs = insertTabBesideActive(
      [
        { ...newTab("s1"), id: "a" },
        { ...newTab("s2"), id: "q1" },
        { ...newTab("s4"), id: "b" },
      ],
      { ...newTab("s3"), id: "w1" },
      "q1",
    );
    const snapshot = workspaceSnapshot([
      window(tabs, sessions, "w1", "/p", [["w1", "/p-trees/w1"]]),
    ]);
    expect(snapshot.activeTabId).toBe("a");
  });

  it("falls back to the next tab of the project, then to the first default-workspace tab", () => {
    const tab = (id: string) => ({ ...newTab(id), id });
    const save = (tabs: WorkspaceTab[]) =>
      workspaceSnapshot([
        window(
          tabs,
          [
            chat("q1", "/q"),
            inTree("w1", "/p", "/p-trees/w1"),
            chat("b", "/p"),
          ],
          "w1",
          "/p",
          [["w1", "/p-trees/w1"]],
        ),
      ]).activeTabId;
    expect(save([tab("q1"), tab("w1"), tab("b")])).toBe("b");
    expect(save([tab("q1"), tab("w1")])).toBe("q1");
  });

  it("drops a project return target whose tab is in a worktree", () => {
    const tabs = [
      { ...newTab("main"), id: "tab-main" },
      { ...newTab("feature"), id: "tab-feature" },
    ];
    const sessions = [
      chat("main", "/repo"),
      inTree("feature", "/repo", "/trees/a"),
    ];
    const memory = new Map([["/repo", "feature"]]);
    const targets = (pin: string) =>
      workspaceSnapshot([
        window(
          tabs,
          sessions,
          "tab-main",
          "/repo",
          [["tab-feature", pin]],
          memory,
        ),
      ]).projectReturnTargets;
    expect(targets("/repo")).toEqual([
      { projectPath: "/repo", tabId: "feature" },
    ]);
    expect(targets("/trees/a")).toEqual([]);
  });

  it("keeps a saved active tab that belongs to no project", () => {
    const snapshot = workspaceSnapshot([
      {
        tabs: [
          { ...newTab("s1"), id: "a" },
          { ...newTab("home"), id: "h" },
        ],
        sessions: [chat("s1", "/p"), chat("home", "~")],
        activeTabId: "h",
        projectCwd: "/p",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    expect(snapshot.activeTabId).toBe("h");
  });

  it("saves the nearest saved tab when an agent-only active tab is dropped", () => {
    const agent = newAgentTab("Audit the UI", "/repo", {
      sessionId: "worker",
      leadId: "s1",
      harness: "codex",
    });
    const agentTab = {
      ...newTab("s1"),
      id: "g",
      layout: leaf("e1"),
      focusedId: "e1",
      editorPanes: [{ id: "e1", files: [agent], activeFileId: agent.id }],
    };
    const snapshot = workspaceSnapshot([
      {
        tabs: [
          { ...newTab("s1"), id: "a" },
          agentTab,
          { ...newTab("s2"), id: "b" },
        ],
        sessions: [chat("s1", "/repo"), chat("s2", "/repo")],
        activeTabId: "g",
        projectCwd: "/repo",
        projectReturnMemory: new Map(),
        projectTerminals: [],
        pins: new Map(),
      },
    ]);
    expect(snapshot.tabs.map((tab) => tab.id)).toEqual(["a", "b"]);
    expect(snapshot.activeTabId).toBe("a");
  });
});
