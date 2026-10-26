import { writeFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import fixture from "./fixtures/workspaceSnapshotJoin.realdb.json";

// workspaceSnapshotJoin.realdb.json is the contract with Rust. It is
// generated, never edited by hand:
//   1. WRITE_WINDOW_ROWS_FIXTURE=1 npx vitest run <this file> writes each
//      scenario's `saves`: what the real `workspaceSnapshot` saves for each
//      window, in the order the windows save, and the chats each window
//      left running.
//   2. WRITE_WINDOW_ROWS_FIXTURE=1 cargo test --lib
//      the_rows_a_relaunch_restores_are_the_ones_the_web_app_joins runs those
//      saves through the real `workspace_set_snapshot` and
//      `session_set_in_flight` of each window on a new database, relaunches,
//      and writes the `rows` the real `workspace_get_snapshot` answers and
//      the `inFlight` chats the real `session_list_in_flight` answers.
// Without the variable both tests compare instead: this file fails when the
// saves differ from the file, Rust when its rows do.

const ipc = vi.hoisted(() => ({
  rows: [] as unknown[],
  inFlight: [] as unknown[],
  records: new Map<string, unknown>(),
}));

// Only the IPC boundary is replaced. It answers the relaunch's reads with the
// rows and running chats Rust wrote. Only a running chat has a saved record,
// the one its quit wrote; every other chat restores from its stub.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(
    async (command: string, args: { path?: string; sessionId?: string } = {}) => {
      switch (command) {
        case "workspace_get_snapshot":
          return ipc.rows;
        case "session_list_in_flight":
          return ipc.inFlight;
        case "session_get":
          return ipc.records.get(args.sessionId ?? "") ?? null;
        case "mono_session_get":
          return null;
        case "resolve_project_location":
          return { path: args.path, identity: args.path };
        default:
          throw `not a recorded command: ${command}`;
      }
    },
  ),
}));

import { planProjectReturn } from "../../projects/model/projectReturn";
import type { ProjectTerminalDock } from "../../projects/model/projectTerminal";
import { newSession, type Session } from "../../sessions/model/session";
import { pathKey } from "../../../shared/lib/paths";
import {
  leafIds,
  type EditorPane,
  type LayoutNode,
  type WorkspaceTab,
} from "./layout";
import {
  parseWorkspaceSnapshot,
  workspaceSnapshot,
  type WindowWorkspace,
} from "./workspaceSnapshot";

const WRITE = process.env.WRITE_WINDOW_ROWS_FIXTURE === "1";
const FIXTURE = new URL(
  "./fixtures/workspaceSnapshotJoin.realdb.json",
  import.meta.url,
);
const ACME = "/tmp/acme";
const NOTES = "/tmp/notes";
const TREE = "/tmp/acme-worktrees/fix";
const BUG = "/tmp/acme-worktrees/bug";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const chat = (id: string, title: string, cwd = ACME, worktreeCwd?: string) => ({
  ...newSession("codex", cwd, "gpt-5-codex", "supervised"),
  id,
  title,
  ...(worktreeCwd ? { worktreeCwd } : {}),
}) satisfies Session;

function tab(id: string, ...panes: string[]): WorkspaceTab {
  const leaves = panes.map((pane): LayoutNode => ({ type: "leaf", id: pane }));
  return {
    kind: "session",
    id,
    layout:
      leaves.length === 1
        ? leaves[0]
        : {
            type: "split",
            id: `split-${id}`,
            dir: "right",
            children: leaves,
            sizes: leaves.map(() => 1 / leaves.length),
          },
    focusedId: panes[0],
    editorPanes: [],
    terminalPanes: [],
  };
}

const filePane: EditorPane = {
  id: "pane-f",
  files: [{ id: "file-readme", path: `${ACME}/README.md`, cwd: ACME }],
  activeFileId: "file-readme",
};

const terminalPane: EditorPane = {
  id: "pane-t",
  files: [{ id: "pane-t-shell", path: "zsh", cwd: ACME, terminal: true }],
  activeFileId: "pane-t-shell",
};

const dock = (projectPath: string, id: string): ProjectTerminalDock => ({
  projectPath,
  pane: {
    id,
    files: [{ id: `${id}-shell`, path: "zsh", cwd: projectPath, terminal: true }],
    activeFileId: `${id}-shell`,
  },
  side: "bottom",
  size: 240,
  open: true,
});

const completion = (requestId: string) => ({
  origin: { monoId: "mono-1", turn: 1 },
  request: { requestId, sessionId: "x", project: ACME, prompt: "Sum up the notes" },
});

const windowWorkspace = (fields: Partial<WindowWorkspace>): WindowWorkspace => ({
  tabs: [],
  sessions: [],
  activeTabId: "",
  projectCwd: ACME,
  projectReturnMemory: new Map(),
  projectTerminals: [],
  pins: new Map(),
  ...fields,
});

/** The tabs New Window copied into the second window. */
const copied = [
  tab("tab-blank", "blank"),
  {
    ...tab("tab-shared", "a", "pane-f", "pane-t"),
    editorPanes: [filePane],
    terminalPanes: [terminalPane],
  },
  tab("tab-tree", "t"),
];

type InFlight = { sessionId: string; cwd: string };

type Save = {
  window: "main" | "window-1" | "window-2";
  readAt: number;
  workspace: WindowWorkspace;
  monoCompletions: unknown[];
  /** The chats the window's quit left running. */
  inFlight?: InFlight[];
};

const COPIES =
  "New Window copies the tabs, each window changes its copy, main saves last";
const NO_WORKTREE = "two windows that pin no tab to a worktree";
const EMPTY_NEWEST = "main closed every tab and saves last";
const OWED_ONLY = "no window saved a tab, but a chat runs and a Mono completion is owed";
const HOME_ONE = "one window in ~ with chats in two projects";
const HOME_TWO = "two windows, the newest in ~";
const WORKTREE_NEWEST = "main saved only a worktree tab, window-1 a project tab";
const LOST_CHAT = "window-1's copy of a worktree tab lost its worktree chat to main";
const RENAMED_PIN = "window-1's copy of a worktree tab lost a chat and keeps its worktree";
const THREE = "three windows open chats in a copy of one tab";
const RETURN = "one window remembers the chat it last had open in a project it is not in";
const NOTHING = "nothing was saved";

const SCENARIOS: Record<string, Save[]> = {
  [COPIES]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [
          // Opened from the history into the copied blank tab
          // (App.tsx replaceBlankPaneWithSession).
          tab("tab-blank", "z"),
          {
            ...tab("tab-shared", "a", "pane-f", "pane-t", "w"),
            editorPanes: [filePane],
            terminalPanes: [terminalPane],
          },
          tab("tab-tree", "t"),
          tab("tab-u", "u"),
          tab("tab-n", "n"),
        ],
        sessions: [
          chat("z", "z"),
          chat("a", "a, as window-1 saw it"),
          chat("w", "w"),
          chat("t", "t", ACME, TREE),
          chat("u", "u", ACME, TREE),
          chat("n", "n", NOTES),
        ],
        activeTabId: "tab-n",
        projectCwd: NOTES,
        projectReturnMemory: new Map([[pathKey(NOTES), "n"]]),
        projectTerminals: [dock(ACME, "dock-second"), dock(NOTES, "dock-notes")],
        lastDockSide: "left",
        pins: new Map([
          ["tab-tree", TREE],
          ["tab-u", TREE],
        ]),
      }),
      monoCompletions: [completion("req-window-1")],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [...copied, tab("tab-x", "x")],
        sessions: [
          chat("blank", ""),
          chat("a", "a, as main saw it"),
          chat("t", "t", ACME, TREE),
          chat("x", "x"),
        ],
        activeTabId: "tab-blank",
        projectTerminals: [dock(ACME, "dock-main")],
        lastDockSide: "bottom",
        pins: new Map([["tab-tree", ACME]]),
      }),
      monoCompletions: [completion("req-main")],
    },
  ],
  [NO_WORKTREE]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-k", "k")],
        sessions: [chat("k", "k")],
        activeTabId: "tab-k",
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-j", "j")],
        sessions: [chat("j", "j")],
        activeTabId: "tab-j",
      }),
      monoCompletions: [],
    },
  ],
  [EMPTY_NEWEST]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-k", "k")],
        sessions: [chat("k", "k", NOTES)],
        activeTabId: "tab-k",
        projectCwd: NOTES,
      }),
      monoCompletions: [completion("req-window-1")],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({}),
      monoCompletions: [completion("req-main")],
    },
  ],
  [OWED_ONLY]: [
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({ sessions: [chat("s", "s")] }),
      monoCompletions: [completion("req-1")],
      inFlight: [{ sessionId: "s", cwd: ACME }],
    },
  ],
  [HOME_ONE]: [
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-1", "s1"), tab("tab-2", "s2")],
        sessions: [chat("s1", "s1", "/tmp/first"), chat("s2", "s2", "/tmp/second")],
        activeTabId: "tab-1",
        projectCwd: "~",
      }),
      monoCompletions: [],
    },
  ],
  [HOME_TWO]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-n", "n")],
        sessions: [chat("n", "n", NOTES)],
        activeTabId: "tab-n",
        projectCwd: NOTES,
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-1", "s1")],
        sessions: [chat("s1", "s1", "/tmp/first")],
        activeTabId: "tab-1",
        projectCwd: "~",
      }),
      monoCompletions: [],
    },
  ],
  [WORKTREE_NEWEST]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-k", "k")],
        sessions: [chat("k", "k")],
        activeTabId: "tab-k",
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-t", "t")],
        sessions: [chat("t", "t", ACME, TREE)],
        activeTabId: "tab-t",
        pins: new Map([["tab-t", TREE]]),
      }),
      monoCompletions: [],
    },
  ],
  [LOST_CHAT]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-s", "s1", "s2")],
        sessions: [chat("s1", "s1", ACME, TREE), chat("s2", "s2", ACME, BUG)],
        pins: new Map([["tab-s", TREE]]),
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-s", "s1")],
        sessions: [chat("s1", "s1", ACME, TREE)],
        pins: new Map([["tab-s", TREE]]),
      }),
      monoCompletions: [],
    },
  ],
  [RENAMED_PIN]: [
    {
      window: "window-1",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-c", "x", "w", "z")],
        sessions: [chat("x", "x"), chat("w", "w"), chat("z", "z", ACME, BUG)],
        pins: new Map([["tab-c", BUG]]),
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-c", "x")],
        sessions: [chat("x", "x")],
        activeTabId: "tab-c",
      }),
      monoCompletions: [],
    },
  ],
  [THREE]: [
    {
      window: "window-2",
      readAt: 1_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-c", "z", "v")],
        sessions: [chat("z", "z"), chat("v", "v")],
        activeTabId: "tab-c",
      }),
      monoCompletions: [],
    },
    {
      window: "window-1",
      readAt: 1_500,
      workspace: windowWorkspace({
        tabs: [tab("tab-c", "z", "w")],
        sessions: [chat("z", "z"), chat("w", "w")],
        activeTabId: "tab-c",
      }),
      monoCompletions: [],
    },
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-c", "x")],
        sessions: [chat("x", "x")],
        activeTabId: "tab-c",
      }),
      monoCompletions: [],
    },
  ],
  [RETURN]: [
    {
      window: "main",
      readAt: 2_000,
      workspace: windowWorkspace({
        tabs: [tab("tab-a", "a"), tab("tab-n1", "n1"), tab("tab-n2", "n2")],
        sessions: [chat("a", "a"), chat("n1", "n1", NOTES), chat("n2", "n2", NOTES)],
        activeTabId: "tab-a",
        projectReturnMemory: new Map([[pathKey(NOTES), "n2"]]),
      }),
      monoCompletions: [],
    },
  ],
  [NOTHING]: [],
};

const saves = (name: string) =>
  JSON.parse(
    JSON.stringify(
      SCENARIOS[name].map(({ window, readAt, workspace, monoCompletions, inFlight }) => ({
        window,
        readAt,
        snapshot: { ...workspaceSnapshot([workspace]), monoCompletions },
        ...(inFlight ? { inFlight } : {}),
      })),
    ),
  ) as unknown;

const scenarioOf = (name: string) =>
  fixture.scenarios.find((scenario) => scenario.name === name) as
    | { rows: unknown[]; inFlight?: InFlight[] }
    | undefined;
const rowsOf = (name: string) => scenarioOf(name)?.rows ?? [];
const inFlightOf = (name: string) => scenarioOf(name)?.inFlight ?? [];

/** A relaunch on the rows Rust answered: what boot parses from the store's
 * read, and what boot restores. A running chat's record is the one its
 * window's quit wrote. Imported per relaunch: boot reads once per module
 * graph. */
async function relaunch(name: string) {
  ipc.rows = rowsOf(name);
  ipc.inFlight = inFlightOf(name);
  const running = new Set(inFlightOf(name).map((ref) => ref.sessionId));
  ipc.records = new Map(
    SCENARIOS[name]
      .flatMap((save) => save.workspace.sessions)
      .filter((session) => running.has(session.id))
      .map((session) => [
        session.id,
        { ...session, blocks: [], createdAt: 1, updatedAt: 1 },
      ]),
  );
  vi.resetModules();
  const store = await import("../../sessions/data/sessionStore");
  const lifecycle = await import("../../../app/model/appLifecycle");
  return {
    joined: parseWorkspaceSnapshot(await store.loadWorkspaceSnapshot()),
    restored: await lifecycle.loadResumedWorkspace(),
  };
}

it.runIf(WRITE)("writes the saves", () => {
  const scenarios = Object.keys(SCENARIOS).map((name) => ({
    name,
    saves: saves(name),
    rows: rowsOf(name),
    inFlight: inFlightOf(name),
  }));
  writeFileSync(FIXTURE, `${JSON.stringify({ scenarios }, null, 2)}\n`);
});

it("the fixture holds what the app saves for each window", () => {
  expect(
    fixture.scenarios.map(({ name, saves }) => ({ name, saves })),
  ).toEqual(Object.keys(SCENARIOS).map((name) => ({ name, saves: saves(name) })));
});

describe("a relaunch restores every window's tabs from the rows Rust answers", () => {
  it("each chat and pane comes back in one tab, and the newest window's tabs, stubs and settings win", async () => {
    const { joined, restored } = await relaunch(COPIES);
    if (!restored || !joined) throw new Error("the rows do not restore");
    const tabs = restored.tabs.map((entry) => ({
      id: entry.id,
      leaves: leafIds(entry.layout),
      panes: [...entry.editorPanes, ...entry.terminalPanes].map((pane) => pane.id),
    }));

    expect(tabs).toEqual([
      { id: "tab-blank", leaves: ["blank"], panes: [] },
      { id: "tab-shared", leaves: ["a", "pane-f", "pane-t"], panes: ["pane-f", "pane-t"] },
      { id: "tab-tree", leaves: ["t"], panes: [] },
      { id: "tab-x", leaves: ["x"], panes: [] },
      { id: expect.stringMatching(UUID), leaves: ["z"], panes: [] },
      { id: expect.stringMatching(UUID), leaves: ["w"], panes: [] },
      { id: "tab-u", leaves: ["u"], panes: [] },
      { id: "tab-n", leaves: ["n"], panes: [] },
    ]);
    expect(new Set(tabs.map((entry) => entry.id)).size).toBe(tabs.length);
    expect({
      activeTabId: restored.activeTabId,
      projectCwd: restored.projectCwd,
      lastDockSide: restored.lastDockSide,
      titleOfA: restored.sessions.find((session) => session.id === "a")?.title,
      sessions: restored.sessions.map((session) => session.id).sort(),
      treeTab: restored.pins.get("tab-tree"),
      uTab: restored.pins.get("tab-u"),
      docks: restored.projectTerminals.map((entry) => entry.pane.id),
      returnTargets: [...restored.projectReturnMemory],
      monoCompletions: restored.monoCompletions,
    }).toEqual({
      activeTabId: "tab-blank",
      projectCwd: ACME,
      lastDockSide: "bottom",
      titleOfA: "a, as main saw it",
      sessions: ["a", "blank", "n", "t", "u", "w", "x", "z"],
      treeTab: ACME,
      uTab: TREE,
      docks: ["dock-main"],
      returnTargets: [[pathKey(ACME), "blank"]],
      monoCompletions: [completion("req-main")],
    });
    expect(joined.tabWorkspaces).toEqual([{ tabId: "tab-u", path: TREE }]);
    expect(joined.sessions.map((stub) => stub.id)).toEqual(
      ["blank", "a", "t", "x", "z", "w", "u", "n"],
    );
  });

  it("with no tab in a worktree, the join saves no tab workspace", async () => {
    const { joined } = await relaunch(NO_WORKTREE);

    expect(joined?.tabs.map((entry) => entry.id)).toEqual(["tab-j", "tab-k"]);
    expect(joined?.tabWorkspaces).toBeUndefined();
  });

  it("a window with no tab adds nothing, and the newest row's Mono completions are the ones delivered", async () => {
    const { joined, restored } = await relaunch(EMPTY_NEWEST);

    expect(joined?.tabs.map((entry) => entry.id)).toEqual(["tab-k"]);
    expect({
      tabs: restored?.tabs.map((entry) => entry.id),
      projectCwd: restored?.projectCwd,
      monoCompletions: restored?.monoCompletions,
    }).toEqual({
      tabs: ["tab-k"],
      projectCwd: NOTES,
      monoCompletions: [completion("req-main")],
    });
  });

  it("a database where no window saved restores nothing", async () => {
    expect(rowsOf(NOTHING)).toEqual([]);
    expect(await relaunch(NOTHING)).toEqual({ joined: null, restored: null });
  });

  it("with no tab to restore, a chat still running comes back with the Mono completions its quit owed", async () => {
    const { joined, restored } = await relaunch(OWED_ONLY);

    expect(inFlightOf(OWED_ONLY)).toEqual([{ sessionId: "s", cwd: ACME }]);
    expect(joined).toBeNull();
    expect({
      leaves: restored?.tabs.map((entry) => leafIds(entry.layout)),
      monoCompletions: restored?.monoCompletions,
    }).toEqual({ leaves: [["s"]], monoCompletions: [completion("req-1")] });
  });

  it("a restore in ~ opens the project of the newest window's first chat", async () => {
    const one = await relaunch(HOME_ONE);
    const two = await relaunch(HOME_TWO);

    expect({
      one: one.restored?.projectCwd,
      oneOrder: one.restored?.sessions.map((session) => session.id),
      two: two.restored?.projectCwd,
    }).toEqual({ one: "/tmp/first", oneOrder: ["s1", "s2"], two: "/tmp/first" });
  });

  it("when the newest window saved no project tab, an older window's project tab is active", async () => {
    const { joined, restored } = await relaunch(WORKTREE_NEWEST);

    expect({
      joined: joined?.activeTabId,
      restored: restored?.activeTabId,
      worktreeTab: restored?.pins.get("tab-t"),
    }).toEqual({ joined: "tab-k", restored: "tab-k", worktreeTab: TREE });
  });

  it("a copy that lost the chat its worktree was for saves no worktree, as one window would", async () => {
    const { joined, restored } = await relaunch(LOST_CHAT);
    const lost = restored?.tabs.find((entry) => leafIds(entry.layout).includes("s2"));

    expect(restored?.tabs.map((entry) => leafIds(entry.layout))).toEqual([["s1"], ["s2"]]);
    expect({
      kept: restored?.pins.get("tab-s"),
      lost: lost && restored?.pins.get(lost.id),
      saved: joined?.tabWorkspaces,
    }).toEqual({
      kept: TREE,
      lost: ACME,
      saved: [{ tabId: "tab-s", path: TREE }],
    });
  });

  it("a copy that lost a chat keeps its worktree under its new id while a chat left in it works there", async () => {
    const { joined, restored } = await relaunch(RENAMED_PIN);
    const renamed = restored?.tabs.find((entry) => entry.id !== "tab-c");

    expect(restored?.tabs.map((entry) => leafIds(entry.layout))).toEqual([["x"], ["w", "z"]]);
    expect({
      copied: restored?.pins.get("tab-c"),
      renamed: renamed && restored?.pins.get(renamed.id),
      saved: joined?.tabWorkspaces,
    }).toEqual({
      copied: ACME,
      renamed: BUG,
      saved: [{ tabId: expect.stringMatching(UUID), path: BUG }],
    });
  });

  it("three windows that opened chats in a copy of one tab restore each chat in one tab", async () => {
    const { restored } = await relaunch(THREE);

    expect(
      restored?.tabs.map((entry) => ({ id: entry.id, leaves: leafIds(entry.layout) })),
    ).toEqual([
      { id: "tab-c", leaves: ["x"] },
      { id: expect.stringMatching(UUID), leaves: ["z", "w"] },
      { id: expect.stringMatching(UUID), leaves: ["v"] },
    ]);
  });

  it("switching back to a project the window is not in opens the chat it last had open there", async () => {
    const { restored } = await relaunch(RETURN);
    if (!restored) throw new Error("the rows do not restore");

    expect(
      planProjectReturn({
        memory: restored.projectReturnMemory,
        tabs: restored.tabs,
        sessions: restored.sessions,
        activeTabId: restored.activeTabId,
        excludedIds: new Set(),
        projectPath: NOTES,
      }),
    ).toEqual({ action: "activate", tabId: "tab-n2", paneId: "n2" });
  });

  it("the restoring window's next save is no larger than the rows it joined", async () => {
    const bytes = (value: unknown) => Buffer.byteLength(JSON.stringify(value));
    // A lone row can grow: a window saved in ~ names its first chat's
    // project once restored, as before ADE-11.
    const joining = Object.keys(SCENARIOS).filter(
      (name) => SCENARIOS[name].length > 1,
    );
    for (const name of joining) {
      const { restored } = await relaunch(name);
      if (!restored) throw new Error(`${name} does not restore`);
      const next = {
        ...workspaceSnapshot([restored]),
        monoCompletions: restored.monoCompletions,
      };
      expect(bytes(next), name).toBeLessThanOrEqual(
        rowsOf(name).reduce((sum: number, row) => sum + bytes(row), 0),
      );
    }
  });
});
