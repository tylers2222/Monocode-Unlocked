import { useCallback, useEffect, useRef, useState } from "react";
import {
  isRemoteProjectPath,
  sameProjectPath,
} from "../../features/projects/model/recents";
import {
  isBlankSession,
  runningTurnStart,
} from "../../features/projects/model/projectReturn";
import type { Session } from "../../features/sessions/model/session";
import {
  setWorktreeFocus,
  worktreeFocus,
  type WorktreeFocus,
} from "../../features/source-control/model/worktreeFocus";
import type { Worktree } from "../../features/source-control/model/worktrees";
import type { WorkspaceTab } from "../../features/workspace/model/layout";
import {
  filterTabsForProject,
  tabWorkspace,
  workspaceTabCwd,
} from "../../features/workspace/model/workspaceTabGroups";
import { pathKey } from "../../shared/lib/paths";

type Destination = { project: string; focus?: WorktreeFocus };
type Request = Destination & { kind: "project" | "workspace" };
type Options = {
  project: string;
  activeTabId: string;
  tabs: WorkspaceTab[];
  sessions: Session[];
  pins: Map<string, string>;
  moveSession: (
    id: string,
    tree: Worktree,
    isCurrent: () => boolean,
  ) => Promise<void>;
  activateTab: (id: string) => void;
  createTab: (project: string, focus?: WorktreeFocus) => string;
};

const workspaceKey = (project: string, path: string) =>
  `${pathKey(project)}\0${pathKey(path)}`;

/** Workspace requests own their async completion. Ordinary tab/session opens
 * cancel them and never invoke the remembered-tab selection path. */
export function useWorkspaceNavigation(options: Options) {
  const latest = useRef(options);
  latest.current = options;
  const request = useRef<Request | null>(null);
  const running = useRef(false);
  const mounted = useRef(true);
  const moving = useRef(new Set<string>());
  const memory = useRef(new Map<string, string>());
  const [revision, refreshPins] = useState(0);
  const [pending, setPending] = useState<Request | null>(null);
  const [error, setError] = useState<{
    project: string;
    message: string;
  } | null>(null);

  const cancel = useCallback(() => {
    request.current = null;
    setPending(null);
    setError(null);
  }, []);

  const select = useCallback((next: Request) => {
    if (
      !next.project ||
      next.project === "~" ||
      isRemoteProjectPath(next.project)
    )
      return;
    request.current = next;
    setError(null);
    setPending(next);
  }, []);
  const selectWorkspace = useCallback(
    (project: string, focus?: WorktreeFocus) => {
      select({ kind: "workspace", project, focus });
    },
    [select],
  );
  const selectProject = useCallback(
    (project: string) => {
      select({ kind: "project", project, focus: worktreeFocus(project) });
    },
    [select],
  );

  const drain = useCallback(async () => {
    if (running.current) return;
    running.current = true;
    try {
      while (mounted.current && request.current) {
        const next = request.current;
        const view = latest.current;
        // A project request is recorded in the same event as openProjects.
        // Wait for its landing tab to render before choosing its workspace.
        if (!sameProjectPath(view.project, next.project)) break;
        // No tab is active after a boot with every tab in a worktree.
        const tab = view.tabs.find((entry) => entry.id === view.activeTabId);
        const session =
          tab && view.sessions.find((entry) => entry.id === tab.focusedId);
        const landing = tab
          ? tabWorkspace(tab, view.sessions, view.pins)
          : null;
        const landingFocus =
          landing && !sameProjectPath(landing, next.project)
            ? { path: landing, branch: session?.branch ?? null }
            : undefined;
        // A project opened to a running session stays on it, in its own
        // workspace, rather than the remembered workspace's tab.
        const focus =
          next.kind === "project" &&
          landing &&
          session &&
          runningTurnStart(session.id, view.sessions) !== undefined
            ? landingFocus
            : next.focus;
        const path = focus?.path ?? next.project;
        const key = workspaceKey(next.project, path);
        const isCurrent = () =>
          mounted.current &&
          request.current === next &&
          latest.current.activeTabId === view.activeTabId &&
          (!tab ||
            latest.current.tabs.find((entry) => entry.id === tab.id)
              ?.focusedId === tab.focusedId) &&
          sameProjectPath(latest.current.project, next.project);
        try {
          // Mixed-project split tabs keep their existing grouping when the
          // focused pane's project changes. Consume the request without
          // moving the tab or leaving an unfinished navigation behind.
          if (
            tab &&
            !sameProjectPath(
              workspaceTabCwd(tab, view.sessions) ?? "",
              next.project,
            )
          ) {
            setWorktreeFocus(next.project, next.focus);
            continue;
          }
          const scoped = filterTabsForProject(
            view.tabs,
            view.sessions,
            next.project,
          ).filter((entry) => {
            const workspace = tabWorkspace(entry, view.sessions, view.pins);
            return !workspace || sameProjectPath(workspace, path);
          });
          const target =
            tab && (!landing || sameProjectPath(landing, path))
              ? tab
              : (scoped.find((entry) => entry.id === memory.current.get(key)) ??
                scoped[scoped.length - 1]);
          let tabId: string;
          if (target) {
            tabId = target.id;
          } else if (
            tab &&
            session &&
            isBlankSession(session) &&
            sameProjectPath(session.cwd, next.project)
          ) {
            moving.current.add(session.id);
            await view.moveSession(
              session.id,
              {
                path,
                branch: focus?.branch ?? null,
                head: "",
                isMain: !focus,
                locked: false,
                prunable: false,
                missing: false,
                dirty: null,
                unpushed: null,
                sessionIds: [],
              },
              isCurrent,
            );
            if (!isCurrent()) continue;
            tabId = tab.id;
          } else if (tab && next.kind === "project") {
            // Returning through the project rail keeps its landing
            // conversation when the remembered workspace has no open tab.
            // Its checkout stays attached to that conversation.
            tabId = tab.id;
          } else {
            tabId = view.createTab(next.project, focus);
          }
          if (!isCurrent()) continue;
          // Only a successful, current request publishes the workspace. A
          // superseded move never changes focus or pins an unrelated tab.
          if (view.pins.get(tabId) !== path) {
            view.pins.set(tabId, path);
            refreshPins((value) => value + 1);
          }
          memory.current.set(key, tabId);
          setWorktreeFocus(next.project, focus);
          view.activateTab(tabId);
        } catch (caught) {
          if (isCurrent()) {
            // A project rail landing may still be in its default workspace.
            // If restoring the remembered workspace fails, keep that landing
            // usable and label the workspace it actually belongs to.
            if (next.kind === "project") {
              setWorktreeFocus(next.project, landingFocus);
            }
            setError({
              project: next.project,
              message:
                caught instanceof Error ? caught.message : String(caught),
            });
          }
        } finally {
          if (request.current === next) {
            request.current = null;
            setPending(null);
          }
        }
      }
    } finally {
      moving.current.clear();
      running.current = false;
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      request.current = null;
    };
  }, []);

  // An idle session opened directly joins the workspace on screen without
  // changing the session's checkout. A running session opened with no tab
  // asks for its project instead (App `onSelectHistorySession`), so `drain`
  // above focuses its worktree. No navigation reason survives for a future open.
  const focusedId = options.tabs.find(
    (tab) => tab.id === options.activeTabId,
  )?.focusedId;
  useEffect(() => {
    if (request.current) return;
    const view = latest.current;
    const tab = view.tabs.find((entry) => entry.id === view.activeTabId);
    if (!tab) return;
    const project = workspaceTabCwd(tab, view.sessions);
    if (!project || project === "~" || isRemoteProjectPath(project)) return;
    const path = worktreeFocus(project)?.path ?? project;
    if (view.pins.get(tab.id) !== path) {
      view.pins.set(tab.id, path);
      refreshPins((value) => value + 1);
    }
    memory.current.set(workspaceKey(project, path), tab.id);
  }, [options.project, options.activeTabId, focusedId]);

  useEffect(() => {
    void drain();
  }, [
    pending,
    options.project,
    options.activeTabId,
    options.tabs,
    options.sessions,
    drain,
  ]);

  const isSwitching = useCallback((sessionId: string) => {
    if (moving.current.has(sessionId)) return true;
    const next = request.current;
    const view = latest.current;
    return (
      !!next &&
      view.tabs.some(
        (tab) =>
          tab.id === view.activeTabId &&
          tab.focusedId === sessionId &&
          sameProjectPath(
            workspaceTabCwd(tab, view.sessions) ?? "",
            next.project,
          ),
      )
    );
  }, []);

  return {
    selectWorkspace,
    selectProject,
    cancel,
    isSwitching,
    pending,
    error,
    revision,
  };
}
