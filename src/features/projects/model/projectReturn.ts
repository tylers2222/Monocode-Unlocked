import { leafIds, type WorkspaceTab } from "../../workspace/model/layout";
import { turnOpener, type Session } from "../../sessions/model/session";
import { pathKey } from "../../../shared/lib/paths";
import { isRemoteProjectPath, sameProjectPath } from "./recents";

export type ProjectReturnMemory = ReadonlyMap<string, string>;

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

function runningPaneForProject(
  tabs: readonly WorkspaceTab[],
  sessions: readonly Session[],
  target: string,
  paneById: PaneProject,
): { tabId: string; paneId: string } | undefined {
  let best: { tabId: string; paneId: string; start: number } | undefined;
  for (const tab of tabs) {
    for (const paneId of leafIds(tab.layout)) {
      if (!paneBelongsToProject(paneId, target, paneById)) continue;
      const start = runningTurnStart(paneId, sessions);
      if (start === undefined || (best && start <= best.start)) continue;
      best = { tabId: tab.id, paneId, start };
    }
  }
  return best && { tabId: best.tabId, paneId: best.paneId };
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

  if (active?.focusedId && paneBelongsToProject(active.focusedId, target, byPane)) {
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

  const running = runningPaneForProject(tabs, sessions, target, byPane);
  if (running) return { action: "activate", ...running };

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
