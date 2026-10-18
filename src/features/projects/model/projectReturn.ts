import { leafIds, type WorkspaceTab } from "../../workspace/model/layout";
import { turnOpener, type Session } from "../../sessions/model/session";
import { pathKey } from "../../../shared/lib/paths";
import { isMonoSession } from "../../monos/model/mono";
import { isHabitRun } from "../../monos/model/monoHabits";
import { isRemoteProjectPath, sameProjectPath } from "./recents";

export type ProjectReturnMemory = ReadonlyMap<string, string>;

/**
 * Opening the project you are on leaves an idle pane for a session of that
 * project that is running. Product default from General 2 (m_fvchmlt6jhh3),
 * pending Tyler's answer; `false` always stays on the focused pane.
 */
const RUNNING_SESSION_BEATS_IDLE_FOCUS = true;

type ProjectReturnContext = {
  memory: ProjectReturnMemory;
  tabs: WorkspaceTab[];
  sessions: Session[];
  activeTabId: string;
};

type PaneProject = Map<string, string>;

export type ProjectReturnDecision =
  | { action: "keep" }
  | { action: "activate"; tabId: string; paneId?: string }
  | { action: "open-session"; sessionId: string }
  | { action: "reuse-blank"; sessionId: string }
  | { action: "create" };

export function isBlankSession(session: Session | undefined): boolean {
  // Remote transcripts may not have loaded yet, and a shell can hold an
  // unfinished create or composer text. Local blocks cannot prove it is blank.
  if (!session || session.busy || isRemoteProjectPath(session.cwd)) return false;
  return !session.blocks.some((block) => block.role === "user");
}

function paneProjects(
  tabs: readonly WorkspaceTab[],
  sessions: readonly Pick<Session, "id" | "cwd">[],
): PaneProject {
  const cwdBySession = new Map(
    sessions.map((session) => [session.id, session.cwd]),
  );
  const result = new Map<string, string>();

  for (const tab of tabs) {
    for (const paneId of leafIds(tab.layout)) {
      const cwd = cwdBySession.get(paneId);
      if (cwd && cwd !== "~") {
        result.set(paneId, pathKey(cwd));
      }
    }

    for (const pane of tab.editorPanes) {
      const active = pane.files.find((file) => file.id === pane.activeFileId);
      if (active && active.cwd !== "~") {
        result.set(pane.id, pathKey(active.projectCwd ?? active.cwd));
      }
    }

    for (const pane of tab.terminalPanes ?? []) {
      const active = pane.files.find((file) => file.id === pane.activeFileId);
      if (active && active.cwd !== "~") {
        result.set(pane.id, pathKey(active.projectCwd ?? active.cwd));
      }
    }
  }

  return result;
}

function tabFocusedPaneById(tabs: readonly WorkspaceTab[]): Map<string, string> {
  return new Map(tabs.map((tab) => [tab.id, tab.focusedId]));
}

function paneBelongsToProject(
  paneId: string,
  target: string,
  paneById: PaneProject,
): boolean {
  const project = paneById.get(paneId);
  return !!project && sameProjectPath(project, target);
}

function paneForProjectInTab(
  tab: WorkspaceTab,
  target: string,
  paneById: PaneProject,
): string | undefined {
  for (const paneId of leafIds(tab.layout)) {
    if (paneById.get(paneId) === target) return paneId;
  }
  for (const pane of tab.editorPanes) {
    if (paneById.get(pane.id) === target) return pane.id;
  }
  for (const pane of tab.terminalPanes ?? []) {
    if (paneById.get(pane.id) === target) return pane.id;
  }
  return undefined;
}

function tabContainsPane(tab: WorkspaceTab, paneId: string): boolean {
  return (
    leafIds(tab.layout).includes(paneId) ||
    tab.editorPanes.some((pane) => pane.id === paneId) ||
    (tab.terminalPanes ?? []).some((pane) => pane.id === paneId)
  );
}

/** When the session's running turn started, or undefined when it is idle. A
 * busy worker has no tab, so its lead counts as running in its place. Only
 * busy sessions count: a lead's finished turn is not a running turn. */
export function runningTurnStart(
  id: string,
  sessions: readonly Session[],
): number | undefined {
  let start: number | undefined;
  for (const session of sessions) {
    if (!session.busy) continue;
    if (session.id !== id && session.orchestrationLeadId !== id) continue;
    start = Math.max(
      start ?? -Infinity,
      turnOpener(session.blocks)?.startedAt ?? -Infinity,
    );
  }
  return start;
}

/**
 * The project's newest running turn, wherever it shows. A session pane in a
 * tab activates. A running session with no tab opens: a Mono starts its
 * sessions without one, and closing a tab leaves its turn running. Workers
 * count through their lead, and only sessions the live-agents rail lists
 * count. Equal or missing start times keep tab order, then session order, so
 * a session that has a tab is activated rather than opened again.
 */
function runningSessionForProject(
  tabs: readonly WorkspaceTab[],
  sessions: readonly Session[],
  target: string,
  paneById: PaneProject,
): ProjectReturnDecision | undefined {
  const candidates: { decision: ProjectReturnDecision; start?: number }[] = [];
  for (const tab of tabs) {
    for (const paneId of leafIds(tab.layout)) {
      if (!paneBelongsToProject(paneId, target, paneById)) continue;
      candidates.push({
        decision: { action: "activate", tabId: tab.id, paneId },
        start: runningTurnStart(paneId, sessions),
      });
    }
  }
  for (const session of sessions) {
    const id = session.orchestrationLeadId ?? session.id;
    if (
      !session.busy ||
      session.inboxAsk ||
      session.ephemeral ||
      isHabitRun(session.id) ||
      isMonoSession(id) ||
      !sameProjectPath(session.cwd, target)
    )
      continue;
    candidates.push({
      decision: { action: "open-session", sessionId: id },
      start: runningTurnStart(id, sessions),
    });
  }

  let best: { decision: ProjectReturnDecision; start: number } | undefined;
  for (const { decision, start } of candidates) {
    if (start === undefined || (best && start <= best.start)) continue;
    best = { decision, start };
  }
  return best?.decision;
}

export function reconcileProjectReturn({
  memory,
  tabs,
  sessions,
  activeTabId,
}: Omit<ProjectReturnContext, "sessions"> & {
  sessions: readonly Pick<Session, "id" | "cwd">[];
}): ProjectReturnMemory {
  const activeTab = tabs.find((tab) => tab.id === activeTabId);
  const byPane = paneProjects(tabs, sessions);
  const byTab = tabFocusedPaneById(tabs);

  const next = new Map<string, string>();
  for (const [project, saved] of memory) {
    const asPane = byPane.get(saved);
    if (asPane === project) {
      next.set(project, saved);
      continue;
    }

    const focused = byTab.get(saved);
    if (!focused) continue;
    if (paneBelongsToProject(focused, project, byPane)) {
      next.set(project, focused);
    }
  }

  const activePaneId = activeTab?.focusedId;
  const activeProject = activePaneId ? byPane.get(activePaneId) : undefined;
  if (activeProject && activePaneId) next.set(activeProject, activePaneId);

  if (
    next.size === memory.size &&
    [...next].every(([project, id]) => memory.get(project) === id)
  )
    return memory;
  return next;
}

export function planProjectReturn({
  memory,
  tabs,
  sessions,
  activeTabId,
  projectPath,
}: ProjectReturnContext & { projectPath: string }): ProjectReturnDecision {
  const target = pathKey(projectPath);
  const byPane = paneProjects(tabs, sessions);
  const active = tabs.find((tab) => tab.id === activeTabId);
  const running = runningSessionForProject(tabs, sessions, target, byPane);

  if (
    active?.focusedId &&
    paneBelongsToProject(active.focusedId, target, byPane) &&
    (!running ||
      !RUNNING_SESSION_BEATS_IDLE_FOCUS ||
      runningTurnStart(active.focusedId, sessions) !== undefined)
  ) {
    return { action: "keep" };
  }

  const remembered = memory.get(target);
  const rememberedTab =
    remembered && paneBelongsToProject(remembered, target, byPane)
      ? tabs.find((tab) => tabContainsPane(tab, remembered))
      : undefined;
  if (
    remembered &&
    rememberedTab &&
    runningTurnStart(remembered, sessions) !== undefined
  ) {
    return { action: "activate", tabId: rememberedTab.id, paneId: remembered };
  }

  if (running) return running;

  if (remembered && rememberedTab) {
    return { action: "activate", tabId: rememberedTab.id, paneId: remembered };
  }

  for (const tab of tabs) {
    const paneId = paneForProjectInTab(tab, target, byPane);
    if (!paneId) continue;
    return { action: "activate", tabId: tab.id, paneId };
  }

  const current =
    active?.focusedId &&
    sessions.find((session) => session.id === active.focusedId);
  return current && isBlankSession(current)
    ? { action: "reuse-blank", sessionId: current.id }
    : { action: "create" };
}
