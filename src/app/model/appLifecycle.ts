import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  bindHarnessSession,
  forgetHarnessSession,
  isLiveHarness,
} from "../../integrations/harness/core/registry";
import { killAllChildren } from "../../integrations/harness/core/child";
import {
  hasInFlightSessions,
  inFlightRefs,
  isInFlightSession,
  markTurnInterrupted,
  quitWhileBusyMessage,
  wasTurnInterrupted,
  workspaceFromResumed,
} from "../../features/sessions/model/inFlight";
import { leafIds, type WorkspaceTab } from "../../features/workspace/model/layout";
import { killPty } from "../../platform/tauri/pty";
import {
  projectTerminalFileIds,
  type ProjectTerminalDock,
} from "../../features/projects/model/projectTerminal";
import { sessionAgentCwd, type Session } from "../../features/sessions/model/session";
import { sessionChildHarnesses } from "../../features/sessions/model/handoff";
import { queuePauseNotice } from "../../features/sessions/model/messageQueue";
import {
  getSession,
  listInFlightSessions,
  listSessionsByProject,
  loadWorkspaceSnapshot,
  replaceInFlightSessions,
  saveWorkspaceSnapshot,
  shouldPersistSession,
  upsertSession,
  type SessionSummary,
} from "../../features/sessions/data/sessionStore";
import {
  hydrateWorkspaceSnapshot,
  parseWorkspaceSnapshot,
  restoreWorkspacePins,
  workspaceSnapshot,
  type WindowWorkspace,
} from "../../features/workspace/model/workspaceSnapshot";
import { lastProjectPath, normalizeProjectPath, sameProjectPath } from "../../features/projects/model/recents";
import type { SavedMonoCompletion } from "../../features/monos/model/monoSessionCompletion";
import { claimMonoCompletions } from "../../features/sessions/data/monoCompletionStore";

export { hasInFlightSessions };

export type BootWorkspace = {
  resumed: WindowWorkspace | null;
  /** The Mono completion results this window claimed (ADE-89). */
  monoCompletions: SavedMonoCompletion[];
  /** Sidebar rows listed before first paint, so the rail is not empty. */
  history: SessionSummary[];
  historyCwd: string | null;
};

let resumedPromise: Promise<WindowWorkspace | null> | null = null;
let bootPromise: Promise<BootWorkspace> | null = null;
let quitting = false;
let quitDialogOpen = false;
let bootingResumed: WindowWorkspace | null = null;
let liveWorkspace: { read: () => WindowWorkspace; flush: () => void } | null =
  null;

export function isAppQuitting(): boolean {
  return quitting;
}

export function setQuitWorkspace(
  read: () => WindowWorkspace,
  flush: () => void,
): () => void {
  liveWorkspace = { read, flush };
  bootingResumed = null;
  return () => {
    if (liveWorkspace?.read === read) liveWorkspace = null;
  };
}

/**
 * Persist this window for a quit the coordinator has already confirmed.
 * Exiting is `confirm_quit`'s job, once every window has reported ready.
 */
export async function handleQuitRequested(): Promise<boolean> {
  if (liveWorkspace) {
    liveWorkspace.flush();
    quitting = true;
    try {
      await persistQuitState(liveWorkspace.read(), "quit");
      return true;
    } catch {
      quitting = false;
      return false;
    }
  }
  const { resumed } = await loadBootWorkspace();
  const pending = resumed ?? bootingResumed;
  quitting = true;
  if (!pending) return true;
  try {
    await persistBootingResume(pending);
    return true;
  } catch {
    quitting = false;
    return false;
  }
}

/** Each window counts its own live turns; Rust sums them into one decision. */
export async function reportQuitPoll(id: number): Promise<void> {
  let inFlight = 0;
  if (liveWorkspace) {
    liveWorkspace.flush();
    // Every running turn, not just the resumable ones `inFlightRefs` keeps:
    // an Inbox Ask still counts as work nobody agreed to throw away.
    inFlight = liveWorkspace.read().sessions.filter(isInFlightSession).length;
  }
  await invoke("quit_poll_reply", { id, inFlight }).catch(() => undefined);
}

/** The one quit dialog, shown by whichever window the coordinator picked. */
export async function askQuitConfirmation(
  id: number,
  inFlight: number,
): Promise<void> {
  let confirmed = false;
  if (!quitDialogOpen) {
    quitDialogOpen = true;
    try {
      confirmed = await ask(quitWhileBusyMessage(inFlight), {
        title: "MonoCode",
        kind: "warning",
        okLabel: "Quit",
      });
    } catch {
      confirmed = false;
    } finally {
      quitDialogOpen = false;
    }
  }
  await invoke("quit_decision", { id, confirmed }).catch(() => undefined);
}

export async function commitQuit(id: number): Promise<void> {
  const persisted = await handleQuitRequested();
  await invoke("quit_ready", { id, persisted }).catch(() => undefined);
}

/**
 * A quit that stopped part-way because another window could not save. This
 * window is staying open, so it must go back to persisting on unload.
 */
export function abortQuit(): void {
  quitting = false;
}

/** Confirm and stop this window's work without terminating other windows. */
export async function closeBusyWindow(): Promise<void> {
  if (!liveWorkspace) return;
  liveWorkspace.flush();
  await confirmAndCloseWindow(liveWorkspace.read());
}

export function loadResumedWorkspace(): Promise<WindowWorkspace | null> {
  if (!resumedPromise) resumedPromise = loadResumedWorkspaceOnce();
  return resumedPromise;
}

/** Restore runs once; callers share the same promise. */
export function loadBootWorkspace(): Promise<BootWorkspace> {
  if (!bootPromise) {
    bootPromise = (async () => {
      const hintedCwd = lastProjectPath();
      const historyHint = listProjectHistory(hintedCwd);
      // Design 2: end the turns and hand-offs a reload of this window left
      // behind, before any session it loads can start a turn.
      await invoke("control_window_booted").catch(console.error);
      const [resumed, monoCompletions] = await Promise.all([
        loadResumedWorkspace(),
        claimMonoCompletions().catch((error: unknown) => {
          console.error(error);
          return [];
        }),
      ]);
      const cwd = resumed?.projectCwd ?? hintedCwd;
      const listed =
        cwd && hintedCwd && sameProjectPath(cwd, hintedCwd)
          ? await historyHint
          : await listProjectHistory(cwd);
      return {
        resumed,
        monoCompletions,
        history: listed?.rows ?? [],
        historyCwd: listed?.cwd ?? null,
      };
    })();
  }
  return bootPromise;
}

async function listProjectHistory(
  cwd: string | null | undefined,
): Promise<{ cwd: string; rows: SessionSummary[] } | null> {
  if (!cwd || cwd === "~") return null;
  try {
    const rows = await listSessionsByProject(cwd);
    return { cwd: normalizeProjectPath(cwd), rows };
  } catch {
    return null;
  }
}

async function loadResumedWorkspaceOnce(): Promise<WindowWorkspace | null> {
  const [snapshotRaw, refs] = await Promise.all([
    loadWorkspaceSnapshot().catch(() => null),
    listInFlightSessions().catch(() => []),
  ]);
  const interrupted = new Set(refs.map((ref) => ref.sessionId));
  const snapshot = parseWorkspaceSnapshot(snapshotRaw);

  const ids = new Set<string>();
  if (snapshot) {
    for (const stub of snapshot.sessions) ids.add(stub.id);
    for (const tab of snapshot.tabs) {
      for (const id of leafIds(tab.layout)) ids.add(id);
    }
  }
  for (const ref of refs) ids.add(ref.sessionId);

  const loaded = new Map<string, Session>();
  await Promise.all(
    [...ids].map(async (id) => {
      const record = await getSession(id, { restart: true }).catch(() => null);
      if (record) loaded.set(id, record);
    }),
  );

  let hydrated = snapshot
    ? hydrateWorkspaceSnapshot(snapshot, loaded, interrupted)
    : null;
  if (!hydrated && refs.length > 0) {
    const sessions: Session[] = [];
    for (const ref of refs) {
      const record = loaded.get(ref.sessionId);
      if (!record) continue;
      sessions.push(markTurnInterrupted(record));
    }
    const resumed = workspaceFromResumed(sessions);
    hydrated = resumed && {
      ...resumed,
      projectTerminals: [],
      projectReturnMemory: new Map(),
      pins: restoreWorkspacePins(resumed.tabs, resumed.sessions),
    };
  }
  const workspace = hydrated;

  bootingResumed = workspace;
  if (workspace) {
    await Promise.all(
      workspace.sessions
        // Idle transcripts already came from disk. Rewriting every open chat
        // here serialized/indexed the entire workspace before first paint.
        // A queue the quit stopped is saved paused, so a later reload keeps
        // its reason.
        .filter(
          (session) =>
            (interrupted.has(session.id) || !!queuePauseNotice(session)) &&
            shouldPersistSession(session),
        )
        .map((session) => upsertSession(session).catch(() => null)),
    );
  }
  return workspace;
}

export function bindResumedSessions(sessions: Session[]): void {
  for (const session of sessions) {
    if (
      session.worktreeRemoved ||
      !session.providerSessionId ||
      !isLiveHarness(session.harness)
    )
      continue;
    bindHarnessSession(
      session.harness,
      session.id,
      session.providerSessionId,
      sessionAgentCwd(session),
      session.providerAccountId,
      session.blocks,
    );
  }
}

export async function hideCurrentWindow(): Promise<void> {
  await invoke("hide_window");
}

export async function closeCurrentWindow(): Promise<void> {
  await invoke("destroy_window");
}

export async function confirmReload(
  hasUnsavedFiles: boolean,
): Promise<boolean> {
  if (!hasUnsavedFiles) return true;
  return ask("Reload MonoCode and discard unsaved changes?", {
    title: "MonoCode",
    kind: "warning",
    okLabel: "Reload",
  });
}

export async function persistLiveTranscripts(
  sessions: Session[],
): Promise<void> {
  await Promise.all(
    sessions
      .filter(shouldPersistSession)
      .map((session) => upsertSession(session).catch(() => null)),
  );
}

export async function persistQuitState(
  workspace: WindowWorkspace,
  mode: "quit" | "unload",
): Promise<void> {
  const readAt = Date.now();
  const refs = inFlightRefs(workspace.sessions, workspace.tabs);
  const interrupted = new Set(refs.map((ref) => ref.sessionId));
  // A quit ends the process, so a swallowed write is work that never comes
  // back: let it reject and let the caller call the quit off. An unload is a
  // reload, where best effort is enough and failing loudly helps nobody.
  const write = <T,>(pending: Promise<T>): Promise<T | null> =>
    mode === "quit" ? pending : pending.catch(() => null);

  await Promise.all(
    workspace.sessions.map(async (session) => {
      if (!shouldPersistSession(session)) return;
      const payload = interrupted.has(session.id)
        ? markTurnInterrupted(session)
        : session;
      await write(upsertSession(payload));
    }),
  );
  await write(saveWorkspaceSnapshot(workspaceSnapshot([workspace]), readAt));
  // Vite/webview reload must not wipe a restored snapshot: those chats are idle
  // in this process until Continue runs.
  if (mode === "quit" || refs.length > 0) {
    await write(replaceInFlightSessions(refs));
  }
}

async function persistBootingResume(workspace: WindowWorkspace): Promise<void> {
  const readAt = Date.now();
  await Promise.all(
    workspace.sessions
      .filter(shouldPersistSession)
      .map((session) => upsertSession(session).catch(() => null)),
  );
  await saveWorkspaceSnapshot(workspaceSnapshot([workspace]), readAt).catch(
    () => undefined,
  );
  await replaceInFlightSessions(
    workspace.sessions
      .filter(wasTurnInterrupted)
      .map((session) => ({
        sessionId: session.id,
        cwd: session.cwd,
      })),
  ).catch(() => undefined);
}

async function confirmAndCloseWindow(
  workspace: WindowWorkspace,
): Promise<void> {
  if (quitDialogOpen) return;
  quitDialogOpen = true;
  try {
    const refs = inFlightRefs(workspace.sessions, workspace.tabs);
    if (refs.length > 0) {
      const ok = await ask(
        "Close this window and stop its running chats? Other windows will stay open.",
        { title: "MonoCode", kind: "warning", okLabel: "Close window" },
      );
      if (!ok) return;
    }
    quitting = true;
    try {
      await persistQuitState(workspace, "quit");
      await reapWindowRuntime(
        workspace.sessions,
        workspace.tabs,
        workspace.projectTerminals,
        false,
      );
      await closeCurrentWindow();
    } catch {
      quitting = false;
    }
  } finally {
    quitDialogOpen = false;
  }
}

export async function reapWindowRuntime(
  sessions: Session[],
  tabs: WorkspaceTab[],
  projectTerminals: ProjectTerminalDock[] = [],
  includeAllChildren = true,
): Promise<void> {
  await Promise.all(
    sessions.map((session) =>
      Promise.all(
        sessionChildHarnesses(session).map((harness) =>
          forgetHarnessSession(harness, session.id),
        ),
      ),
    ),
  );
  await Promise.all(
    [...terminalFileIds(tabs), ...projectTerminalFileIds(projectTerminals)].map(
      (id) => killPty(id),
    ),
  );
  // Catalog probes, title generators, and usage scrapers are not session
  // children. Drop them so an unused Pi/Codex probe cannot outlive the window.
  if (includeAllChildren) await killAllChildren().catch(() => undefined);
}

function terminalFileIds(tabs: WorkspaceTab[]): string[] {
  const ids: string[] = [];
  for (const tab of tabs) {
    for (const pane of [...tab.editorPanes, ...(tab.terminalPanes ?? [])]) {
      for (const file of pane.files) {
        if (file.terminal) ids.push(file.id);
      }
    }
  }
  return ids;
}
