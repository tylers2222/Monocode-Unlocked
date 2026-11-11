import {
  useEffect,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { setOpenSessionsSource } from "../../features/projects/model/projectDeleteCheck";
import {
  forgetPurgedProject,
  purgeProjectInSpace,
  purgeWindowPlan,
  type ProjectPurge,
} from "../../features/projects/model/projectPurge";
import { removeProjectFromActiveSpace } from "../../features/projects/model/projectRowActions";
import type { ProjectTerminalDock } from "../../features/projects/model/projectTerminal";
import {
  loadRecents,
  normalizeProjectPath,
  sameProjectPath,
  type RecentProject,
} from "../../features/projects/model/recents";
import { rememberLoadedSession } from "../../features/sessions/data/sessionCache";
import {
  shouldPersistSession,
  type SessionSummary,
} from "../../features/sessions/data/sessionStore";
import { sessionChildHarnesses } from "../../features/sessions/model/handoff";
import {
  newDefaultSession,
  type Session,
} from "../../features/sessions/model/session";
import { loadProjectSidebarTab } from "../../features/settings/model/projectSidebarTab";
import type { SidebarTabId } from "../../features/settings/model/appearance";
import { spacesView } from "../../features/spaces/model/activeSpace";
import {
  archiveProjectInSpace,
  projectAfterLeaving,
} from "../../features/spaces/model/spaceProjects";
import { newTab, type WorkspaceTab } from "../../features/workspace/model/layout";
import { filterTabsForProject } from "../../features/workspace/model/workspaceTabGroups";
import { forgetHarnessSession } from "../../integrations/harness/core/registry";
import { QUEUED_FOLLOW_UP_ARCHIVED } from "./appSessionSend";

type Ref<T> = MutableRefObject<T>;
type SetState<T> = Dispatch<SetStateAction<T>>;

/** The App state a project's removal changes. */
export type ProjectRemovalContext = {
  tabsRef: Ref<WorkspaceTab[]>;
  sessionsRef: Ref<Session[]>;
  activeTabIdRef: Ref<string>;
  projectCwdRef: Ref<string>;
  pendingPersist: Ref<Map<string, Session>>;
  lastPersisted: Ref<Map<string, string>>;
  loadedSessionCache: Ref<Map<string, Session>>;
  turnWatchers: Ref<{ cancel: (id: string, reason?: string) => void }>;
  invalidateLoadedSession: (sessionId: string) => void;
  persistSession: (session: Session | undefined) => void;
  onSelectProject: (path: string) => void;
  setRecents: (recents: RecentProject[]) => void;
  setSessions: (sessions: Session[]) => void;
  setTabs: (tabs: WorkspaceTab[]) => void;
  setActiveTabId: (id: string) => void;
  setDirtyFiles: SetState<Set<string>>;
  setProjectTerminals: SetState<ProjectTerminalDock[]>;
  setSidebarTabSelection: SetState<{ project: string; tab: SidebarTabId }>;
  setHistory: SetState<SessionSummary[]>;
  setProjectCwd: (path: string) => void;
  setComposerFocused: (focused: boolean) => void;
};

/** What memory drops for each session the purge touches. A running turn is not among them. */
function forgetPurgedSessions(
  ctx: ProjectRemovalContext,
  sessionIds: ReadonlySet<string>,
  purge: ProjectPurge,
): void {
  for (const sessionId of purge.deleted) {
    ctx.invalidateLoadedSession(sessionId);
    ctx.pendingPersist.current.delete(sessionId);
    ctx.lastPersisted.current.delete(sessionId);
  }
  for (const session of ctx.sessionsRef.current) {
    if (!sessionIds.has(session.id)) continue;
    ctx.pendingPersist.current.delete(session.id);
    ctx.lastPersisted.current.delete(session.id);
    for (const id of sessionChildHarnesses(session)) {
      void forgetHarnessSession(id, session.id);
    }
  }
  ctx.setHistory((current) =>
    current.filter((session) => !purge.deleted.has(session.id)),
  );
}

/** What Archive does to the project's sessions: each is saved, and a busy one keeps running. */
function saveArchivedSessions(
  ctx: ProjectRemovalContext,
  projectSessions: readonly Session[],
): void {
  for (const session of projectSessions) {
    if (session.busy) continue;
    // The session leaves memory, so its queue no longer drains here.
    for (const queued of session.queuedMessages ?? [])
      ctx.turnWatchers.current.cancel(queued.id, QUEUED_FOLLOW_UP_ARCHIVED);
    if (shouldPersistSession(session)) {
      rememberLoadedSession(ctx.loadedSessionCache.current, session);
    }
    ctx.persistSession(session);
    ctx.pendingPersist.current.delete(session.id);
    for (const id of sessionChildHarnesses(session)) {
      void forgetHarnessSession(id, session.id);
    }
  }
}

/** Tabs, sessions and the active tab after the removal; a window always keeps one tab. */
function showRemainingWork(
  ctx: ProjectRemovalContext,
  remainingTabs: WorkspaceTab[],
  remainingSessions: Session[],
  closedTabIds: ReadonlySet<string>,
): void {
  let nextTabs = remainingTabs;
  let nextSessions = remainingSessions;
  const shownTabId = ctx.activeTabIdRef.current;
  let nextActiveTabId = shownTabId;
  if (nextTabs.length === 0) {
    const session = newDefaultSession("~", nextSessions[0]?.runtimeMode);
    const tab = newTab(session.id);
    nextSessions = [...nextSessions, session];
    nextTabs = [tab];
    nextActiveTabId = tab.id;
  } else if (closedTabIds.has(nextActiveTabId)) {
    nextActiveTabId = nextTabs[0].id;
  }
  ctx.sessionsRef.current = nextSessions;
  ctx.tabsRef.current = nextTabs;
  ctx.activeTabIdRef.current = nextActiveTabId;
  ctx.setSessions(nextSessions);
  ctx.setTabs(nextTabs);
  if (nextActiveTabId !== shownTabId) ctx.setActiveTabId(nextActiveTabId);
}

function clearDirtyFiles(
  ctx: ProjectRemovalContext,
  closedTabs: readonly WorkspaceTab[],
): void {
  ctx.setDirtyFiles((prev) => {
    const updated = new Set(prev);
    for (const tab of closedTabs) {
      for (const file of [
        ...tab.editorPanes.flatMap((pane) => pane.files),
        ...(tab.terminalPanes ?? []).flatMap((pane) => pane.files),
      ]) {
        updated.delete(file.id);
      }
    }
    return updated;
  });
}

/**
 * The window moves to the next project it shows, or to no project. False when
 * there is none.
 */
function moveOffProject(
  ctx: ProjectRemovalContext,
  remaining: readonly RecentProject[],
  normalized: string,
): boolean {
  const next = projectAfterLeaving(
    remaining,
    normalized,
    spacesView().projectList,
  );
  if (next) {
    ctx.onSelectProject(next.path);
    ctx.setProjectCwd(next.path);
    return true;
  }
  ctx.setProjectCwd("~");
  ctx.setComposerFocused(true);
  return false;
}

/**
 * T5 or T6. Chats and tabs stay, hidden, and come back if it is added again.
 * On the open project the window moves off it; with no project left it shows a
 * new blank tab, since the project's tabs stay hidden.
 */
export async function leaveProjectSpace(
  ctx: ProjectRemovalContext,
  normalized: string,
): Promise<string | null> {
  const result = await removeProjectFromActiveSpace(normalized);
  if ("refusal" in result) return result.refusal;
  if (
    result.removed &&
    sameProjectPath(ctx.projectCwdRef.current, normalized) &&
    !moveOffProject(ctx, loadRecents(), normalized)
  ) {
    const sessions = ctx.sessionsRef.current;
    const session = newDefaultSession("~", sessions[0]?.runtimeMode);
    const tab = newTab(session.id);
    const nextSessions = [...sessions, session];
    const nextTabs = [...ctx.tabsRef.current, tab];
    ctx.sessionsRef.current = nextSessions;
    ctx.tabsRef.current = nextTabs;
    ctx.activeTabIdRef.current = tab.id;
    ctx.setSessions(nextSessions);
    ctx.setTabs(nextTabs);
    ctx.setActiveTabId(tab.id);
  }
  return null;
}

/**
 * `purge` null is Archive. A purge acts only on what the backend deleted; a
 * project other Spaces still have keeps its rail entry, location, settings and
 * terminals, which are shared. A purge that stopped halfway leaves the project
 * listed and the window on it.
 */
export function removeProjectFromWindow(
  ctx: ProjectRemovalContext,
  normalized: string,
  purge: ProjectPurge | null,
): void {
  const leaving = !purge || purge.complete;
  const wasCurrent = sameProjectPath(ctx.projectCwdRef.current, normalized);
  const remaining = purge
    ? forgetPurgedProject(normalized, purge)
    : archiveProjectInSpace(normalized);
  if (purge && leaving) {
    ctx.setSidebarTabSelection((current) =>
      sameProjectPath(current.project, normalized)
        ? { project: "~", tab: loadProjectSidebarTab("~") }
        : current,
    );
  }
  ctx.setRecents(remaining);

  const tabs = ctx.tabsRef.current;
  const sessions = ctx.sessionsRef.current;
  const projectSessions = sessions.filter((session) =>
    sameProjectPath(session.cwd, normalized),
  );
  const plan = purge
    ? purgeWindowPlan(tabs, sessions, normalized, purge)
    : {
        tabs: filterTabsForProject(tabs, sessions, normalized),
        trimmed: new Map<string, WorkspaceTab>(),
        sessionIds: new Set(projectSessions.map((session) => session.id)),
      };
  const closedTabIds = new Set(plan.tabs.map((tab) => tab.id));
  if (purge) forgetPurgedSessions(ctx, plan.sessionIds, purge);
  else saveArchivedSessions(ctx, projectSessions);

  showRemainingWork(
    ctx,
    tabs
      .filter((tab) => !closedTabIds.has(tab.id))
      .map((tab) => plan.trimmed.get(tab.id) ?? tab),
    sessions.filter(
      (session) =>
        !plan.sessionIds.has(session.id) || (!purge && session.busy),
    ),
    closedTabIds,
  );
  clearDirtyFiles(ctx, plan.tabs);
  if (!purge || purge.lastSpace) {
    ctx.setProjectTerminals((prev) =>
      prev.filter((dock) => !sameProjectPath(dock.projectPath, normalized)),
    );
  }
  if (wasCurrent && leaving) moveOffProject(ctx, remaining, normalized);
}

/**
 * Remove from this Space, Archive, or the guarded delete. Remove and a delete
 * answer with their refusal, or null.
 */
export function removeProject(
  ctx: ProjectRemovalContext,
  path: string,
  options: { purgeData: boolean; leaveSpace?: boolean },
): void | Promise<string | null> {
  const normalized = normalizeProjectPath(path);
  if (options.leaveSpace) return leaveProjectSpace(ctx, normalized);
  if (!options.purgeData) {
    removeProjectFromWindow(ctx, normalized, null);
    return;
  }
  return purgeProjectInSpace(normalized).then((result) => {
    if (result.purge) removeProjectFromWindow(ctx, normalized, result.purge);
    return result.refusal;
  });
}

/**
 * The window's Remove handler. The guard checks read the open sessions from
 * the same ref the handler acts on. Each render gives a handler on that
 * render's context; after a purge's wait, tabs, sessions and the active tab
 * are read through their refs.
 */
export function useProjectWindowRemoval(ctx: ProjectRemovalContext) {
  const { sessionsRef } = ctx;
  useEffect(() => {
    setOpenSessionsSource(() => sessionsRef.current);
  });
  return (path: string, options: { purgeData: boolean; leaveSpace?: boolean }) =>
    removeProject(ctx, path, options);
}
