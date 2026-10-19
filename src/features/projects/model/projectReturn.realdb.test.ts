import { describe, expect, it, vi } from "vitest";
import realDb from "./fixtures/untabbedRunning.realdb.json";

// The fixture is what session_store.rs get_workspace_snapshot, list_in_flight
// and get_session returned for a read-only copy of a real monocode.db, with
// made-up ids, text and paths. Only the IPC boundary is replaced: it answers
// those reads and refuses everything else.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args?: { sessionId?: string }) => {
    if (command === "workspace_get_snapshot") return realDb.snapshot;
    if (command === "session_list_in_flight") return realDb.inFlight;
    if (command === "session_get" || command === "mono_session_get")
      return (
        (realDb.records as Record<string, unknown>)[args?.sessionId ?? ""] ??
        null
      );
    throw `not a recorded read: ${command}`;
  }),
}));

import { loadResumedWorkspace } from "../../../app/model/appLifecycle";
import { getSession } from "../../sessions/data/sessionStore";
import { liveAgentExclusions } from "../../sessions/model/liveAgents";
import type { Session } from "../../sessions/model/session";
import { leafIds } from "../../workspace/model/layout";
import {
  hydrateWorkspaceSnapshot,
  parseWorkspaceSnapshot,
} from "../../workspace/model/workspaceSnapshot";
import { planProjectOpenRun } from "./projectOpenRun";

const ACME = "/Users/test/acme";
const NOTES = "/Users/test/notes";
const NOTES_TAB = "tab-0004";
const IDLE_ACME_TAB = "tab-0003";
const NEWEST_ACME_RUN = "app-mono-chat-0001-request-0003";
const NOTES_RUN = "app-giga-chat-0001-request-0007";
const running = new Set(realDb.inFlight.map((ref) => ref.sessionId));
/** The two Mono chats that started the running sessions, as App reads them from the roster. */
const MONO_CHATS = new Set(["mono-chat-0001", "giga-chat-0001"]);

/**
 * The window as it ran: every session it held, loaded by the app's own
 * loader, busy exactly where it wrote in_flight_sessions from memory
 * (App.tsx `inFlightRefs(sessions, tabs)` -> `replaceInFlightSessions`).
 */
async function liveWindow() {
  const snapshot = parseWorkspaceSnapshot(realDb.snapshot);
  if (!snapshot) throw new Error("the saved snapshot does not parse");
  const loaded = new Map<string, Session>();
  for (const stub of snapshot.sessions) {
    const session = await getSession(stub.id);
    if (session) loaded.set(stub.id, session);
  }
  const workspace = hydrateWorkspaceSnapshot(snapshot, loaded);
  if (!workspace) throw new Error("the saved snapshot does not hydrate");
  const sessions = workspace.sessions.map((session) =>
    running.has(session.id) ? { ...session, busy: true } : session,
  );
  return {
    tabs: workspace.tabs,
    memory: workspace.projectReturnMemory ?? new Map<string, string>(),
    sessions,
    excludedIds: liveAgentExclusions(sessions, MONO_CHATS, () => false),
  };
}

describe("opening a project on a real saved workspace", () => {
  it("has the shipped pattern: every running session is in no tab", async () => {
    const { tabs, sessions } = await liveWindow();
    const tabbed = new Set(tabs.flatMap((tab) => leafIds(tab.layout)));
    const busy = sessions.filter((session) => session.busy);
    expect(busy.map((session) => session.id).sort()).toEqual([...running].sort());
    expect(busy.some((session) => tabbed.has(session.id))).toBe(false);
  });

  it("opens the newest running session from another project", async () => {
    const window = await liveWindow();
    expect(
      planProjectOpenRun({ ...window, activeTabId: NOTES_TAB, paths: [ACME] }),
    ).toEqual([
      { action: "open-session", path: ACME, sessionId: NEWEST_ACME_RUN },
    ]);
  });

  it("leaves the project's remembered idle tab for its running session", async () => {
    const window = await liveWindow();
    expect(window.memory.get(ACME)).toBe(
      window.tabs.find((tab) => tab.id === IDLE_ACME_TAB)?.focusedId,
    );
    expect(
      planProjectOpenRun({
        ...window,
        activeTabId: IDLE_ACME_TAB,
        paths: [ACME],
      }),
    ).toEqual([
      { action: "open-session", path: ACME, sessionId: NEWEST_ACME_RUN },
    ]);
  });

  it("opens the running session a Giga started in the other project", async () => {
    const window = await liveWindow();
    expect(
      planProjectOpenRun({
        ...window,
        activeTabId: IDLE_ACME_TAB,
        paths: [NOTES],
      }),
    ).toEqual([{ action: "open-session", path: NOTES, sessionId: NOTES_RUN }]);
  });

  it("after a restart, activates the resumed sessions' tabs instead of opening them again", async () => {
    const restored = await loadResumedWorkspace();
    if (!restored) throw new Error("the saved workspace does not restore");
    // Auto-continue resubmits each interrupted turn, so it runs again.
    const sessions = restored.sessions.map((session) =>
      running.has(session.id) ? { ...session, busy: true } : session,
    );
    const steps = planProjectOpenRun({
      memory: restored.projectReturnMemory ?? new Map(),
      tabs: restored.tabs,
      sessions,
      activeTabId: NOTES_TAB,
      excludedIds: liveAgentExclusions(sessions, MONO_CHATS, () => false),
      paths: [ACME],
    });
    const tab = restored.tabs.find((entry) =>
      leafIds(entry.layout).includes(NEWEST_ACME_RUN),
    );
    expect(steps).toEqual([
      {
        action: "activate",
        path: ACME,
        tabId: tab?.id,
        paneId: NEWEST_ACME_RUN,
      },
    ]);
  });
});
