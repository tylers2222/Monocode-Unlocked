import { writeFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import fixture from "./fixtures/workspaceSnapshotJoin.realdb.json";

// workspaceSnapshotJoin.realdb.json is the contract with Rust. It is
// generated, never edited by hand:
//   1. WRITE_WINDOW_ROWS_FIXTURE=1 npx vitest run <this file> writes each
//      scenario's `saves`: what the real `workspaceSnapshot` saves for each
//      window, in the order the windows save.
//   2. WRITE_WINDOW_ROWS_FIXTURE=1 cargo test --lib
//      the_rows_a_relaunch_restores_are_the_ones_the_web_app_joins runs those
//      saves through the real `workspace_set_snapshot` of each window on a
//      new database, relaunches, and writes the `rows` the real
//      `workspace_get_snapshot` answers.
// Without the variable both tests compare instead: this file fails when the
// saves differ from the file, Rust when its rows do.

const ipc = vi.hoisted(() => ({ rows: [] as unknown[] }));

// Only the IPC boundary is replaced. It answers the relaunch's reads with the
// rows Rust wrote; no chat has a saved record, so each restores from its stub.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args: { path?: string } = {}) => {
    switch (command) {
      case "workspace_get_snapshot":
        return ipc.rows;
      case "session_list_in_flight":
        return [];
      case "session_get":
      case "mono_session_get":
        return null;
      case "resolve_project_location":
        return { path: args.path, identity: args.path };
      default:
        throw `not a recorded command: ${command}`;
    }
  }),
}));

import type { ProjectTerminalDock } from "../../projects/model/projectTerminal";
import { newSession, type Session } from "../../sessions/model/session";
import { pathKey } from "../../../shared/lib/paths";
import {
  leafIds,
  type EditorPane,
  type LayoutNode,
  type WorkspaceTab,
} from "./layout";
import { workspaceSnapshot, type WindowWorkspace } from "./workspaceSnapshot";

const WRITE = process.env.WRITE_WINDOW_ROWS_FIXTURE === "1";
const FIXTURE = new URL(
  "./fixtures/workspaceSnapshotJoin.realdb.json",
  import.meta.url,
);
const ACME = "/tmp/acme";
const NOTES = "/tmp/notes";
const TREE = "/tmp/acme-worktrees/fix";
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
  { ...tab("tab-shared", "a", "pane-f"), editorPanes: [filePane] },
  tab("tab-tree", "t"),
];

type Save = {
  window: "main" | "window-1";
  readAt: number;
  workspace: WindowWorkspace;
  monoCompletions: unknown[];
};

const COPIES =
  "New Window copies the tabs, each window changes its copy, main saves last";
const NO_WORKTREE = "two windows that pin no tab to a worktree";
const EMPTY_NEWEST = "main closed every tab and saves last";
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
          { ...tab("tab-shared", "a", "pane-f", "w"), editorPanes: [filePane] },
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
  [NOTHING]: [],
};

const saves = (name: string) =>
  JSON.parse(
    JSON.stringify(
      SCENARIOS[name].map(({ window, readAt, workspace, monoCompletions }) => ({
        window,
        readAt,
        snapshot: { ...workspaceSnapshot([workspace]), monoCompletions },
      })),
    ),
  ) as unknown;

const rowsOf = (name: string) =>
  fixture.scenarios.find((scenario) => scenario.name === name)?.rows ?? [];

/** A relaunch on the rows Rust answered: what the IPC read returns, and what
 * boot restores. Imported per relaunch: boot reads once per module graph. */
async function relaunch(name: string) {
  ipc.rows = rowsOf(name);
  vi.resetModules();
  const store = await import("../../sessions/data/sessionStore");
  const lifecycle = await import("../../../app/model/appLifecycle");
  return {
    joined: await store.loadWorkspaceSnapshot(),
    restored: await lifecycle.loadResumedWorkspace(),
  };
}

it.runIf(WRITE)("writes the saves", () => {
  const scenarios = Object.keys(SCENARIOS).map((name) => ({
    name,
    saves: saves(name),
    rows: rowsOf(name),
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
      panes: entry.editorPanes.map((pane) => pane.id),
    }));

    expect(tabs).toEqual([
      { id: "tab-blank", leaves: ["blank"], panes: [] },
      { id: "tab-shared", leaves: ["a", "pane-f"], panes: ["pane-f"] },
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
  });

  it("with no tab in a worktree, the join saves no tab workspace", async () => {
    const { joined } = await relaunch(NO_WORKTREE);

    expect(joined?.tabs.map((entry) => entry.id)).toEqual(["tab-j", "tab-k"]);
    expect(joined?.tabWorkspaces).toEqual([]);
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

  it("the restoring window's next save is no larger than the rows it joined", async () => {
    const bytes = (value: unknown) => Buffer.byteLength(JSON.stringify(value));
    for (const name of [COPIES, NO_WORKTREE, EMPTY_NEWEST]) {
      const { restored } = await relaunch(name);
      if (!restored) throw new Error(`${name} does not restore`);
      const next = {
        ...workspaceSnapshot([restored]),
        monoCompletions: restored.monoCompletions,
      };
      expect(bytes(next)).toBeLessThanOrEqual(
        rowsOf(name).reduce((sum: number, row) => sum + bytes(row), 0),
      );
    }
  });
});
