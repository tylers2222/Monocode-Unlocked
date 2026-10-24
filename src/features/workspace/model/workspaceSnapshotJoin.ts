import type { Session } from "../../sessions/model/session";
import {
  closeLeaf,
  leafIds,
  type EditorPane,
  type WorkspaceTab,
} from "./layout";
import {
  hydrateWorkspaceSnapshot,
  workspaceSnapshot,
  type WindowWorkspace,
  type WorkspaceSnapshot,
} from "./workspaceSnapshot";

/** A field added here does not compile until the join has a rule for it. */
type EveryField<T> = { [K in keyof Required<T>]: T[K] };

/** What a save path writes beside the window's `workspaceSnapshot`. */
export type SaveSiteFields = { monoCompletions?: unknown };

/** What every save path writes, and what a relaunch restores. */
export type SavedWorkspaceSnapshot = WorkspaceSnapshot & SaveSiteFields;

/**
 * The one snapshot boot restores from the rows `workspace_get_snapshot`
 * answers, newest first (ADE-11): a window's own row, every row the last
 * run's windows saved, or the shared row. Each row reopens as its window
 * would, and the join is what one window holding all of them saves, so every
 * save rule holds. Every chat and pane comes back in one tab: a tab keeps only
 * what the newer rows' tabs do not show, and gets a new id when New Window
 * copied its id. A tab keeps its workspace while a chat in it works there. A
 * session's newest stub wins. The newest row's window state wins: the active
 * tab, the project, terminal docks, return targets, the dock side, and the
 * Mono completions owed. With no tab to restore, the newest row is answered
 * whole, so its completions still come back.
 */
export function joinWorkspaceSnapshots(
  rows: readonly SaveSiteFields[],
): SavedWorkspaceSnapshot | SaveSiteFields | null {
  const windows = rows.flatMap(
    (row) => hydrateWorkspaceSnapshot(row, new Map()) ?? [],
  );
  const tabs: WorkspaceTab[] = [];
  const sessions = new Map<string, Session>();
  const pins = new Map<string, string>();
  for (const window of windows) {
    for (const session of window.sessions)
      sessions.set(session.id, sessions.get(session.id) ?? session);
    const shown = new Set(
      tabs.flatMap((kept) => [kept.id, ...leafIds(kept.layout)]),
    );
    for (const tab of window.tabs) {
      const left = leafIds(tab.layout)
        .filter((id) => shown.has(id))
        .reduce<WorkspaceTab | null>(
          (rest, id) => rest && closeLeaf(rest, id),
          tab,
        );
      if (!left) continue;
      const id = shown.has(tab.id) ? crypto.randomUUID() : tab.id;
      const unshown = (panes: EditorPane[]) =>
        panes.filter((pane) => !shown.has(pane.id));
      tabs.push({
        ...left,
        id,
        editorPanes: unshown(left.editorPanes),
        terminalPanes: unshown(left.terminalPanes),
      });
      for (const [pinned, path] of window.pins)
        if (pinned === tab.id) pins.set(id, path);
    }
  }
  const [newest] = windows;
  return newest
    ? ({
        ...workspaceSnapshot([
          {
            tabs,
            sessions: Array.from(sessions.values()),
            activeTabId: newest.activeTabId,
            projectCwd: newest.projectCwd,
            projectReturnMemory: newest.projectReturnMemory,
            projectTerminals: newest.projectTerminals,
            lastDockSide: newest.lastDockSide,
            pins,
          } satisfies EveryField<WindowWorkspace>,
        ]),
        monoCompletions: rows[0].monoCompletions,
      } satisfies EveryField<SaveSiteFields>)
    : (rows[0] ?? null);
}
