import {
  closeLeaf,
  leafIds,
  type EditorPane,
  type WorkspaceTab,
} from "./layout";
import {
  parseWorkspaceSnapshot,
  type WorkspaceSnapshot,
} from "./workspaceSnapshot";

/** A field added to the snapshot does not compile here until the join has a
 * rule for it. */
type EveryField<T> = { [K in keyof Required<T>]: T[K] };

export type JoinedWorkspaceSnapshot = WorkspaceSnapshot & {
  monoCompletions?: unknown;
};

/**
 * The one snapshot boot restores from the rows `workspace_get_snapshot`
 * answers, newest first (ADE-11): a window's own row, every row the last
 * run's windows saved, or the shared row. Every chat and pane comes back in
 * one tab: a tab keeps only what the newer rows' tabs do not show, and gets a
 * new id when New Window copied its id. Its tab workspace goes with it. A row
 * with no tab adds nothing. The newest row's window state wins: the active
 * tab, the project, terminal docks, return targets, the dock side, and the
 * Mono completions owed (New Window copies those).
 */
export function joinWorkspaceSnapshots(
  rows: readonly unknown[],
): JoinedWorkspaceSnapshot | null {
  const parsed = rows.flatMap((row) => parseWorkspaceSnapshot(row) ?? []);
  const tabs: WorkspaceTab[] = [];
  const tabWorkspaces: NonNullable<WorkspaceSnapshot["tabWorkspaces"]> = [];
  for (const row of parsed) {
    const shown = new Set(
      tabs.flatMap((kept) => [kept.id, ...leafIds(kept.layout)]),
    );
    const pinned = new Map(
      row.tabWorkspaces?.map(
        (entry): [string, string] => [entry.tabId, entry.path],
      ),
    );
    for (const tab of row.tabs) {
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
      const path = pinned.get(tab.id);
      if (path) tabWorkspaces.push({ tabId: id, path });
    }
  }
  const [newest] = parsed;
  return newest
    ? ({
        tabs,
        // Oldest first: hydrate keeps a session's last stub, so the newest
        // window's stub wins.
        sessions: parsed.flatMap((row) => row.sessions).reverse(),
        activeTabId: newest.activeTabId,
        projectCwd: newest.projectCwd,
        projectTerminals: newest.projectTerminals,
        projectReturnTargets: newest.projectReturnTargets,
        lastDockSide: newest.lastDockSide,
        tabWorkspaces,
        monoCompletions: (rows[0] as { monoCompletions?: unknown })
          .monoCompletions,
      } satisfies EveryField<JoinedWorkspaceSnapshot>)
    : null;
}
