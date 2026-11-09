import { isMonoSession } from "../../monos/model/mono";
import {
  canPersistSession,
  deleteProjectInSpace,
  listSessionsByProject,
  shouldPersistSession,
} from "../../sessions/data/sessionStore";
import type { Session } from "../../sessions/model/session";
import { spacesView } from "../../spaces/model/activeSpace";
import { closeLeaf, leafIds, type WorkspaceTab } from "../../workspace/model/layout";
import { filterTabsForProject } from "../../workspace/model/workspaceTabGroups";
import { clearProjectSettings } from "./projectData";
import { forgetProjectLocation } from "./projectLocation";
import {
  forgetProject,
  isRemoteProjectPath,
  loadRecents,
  sameProjectPath,
  type RecentProject,
} from "./recents";

/**
 * Deleting a project's data from a Space (spec T10, T11, T16, I5). The backend
 * owns the delete and names the rows it removed. The window acts only on those:
 * another Space's chat, running or not, keeps its turn, its tab and its place in
 * memory.
 */
export type ProjectPurge = {
  deleted: ReadonlySet<string>;
  /** T11: the shared logo, settings and rail entry go too. */
  lastSpace: boolean;
  /** False after a failure halfway: the project stays listed and open. */
  complete: boolean;
};

type OpenSession = Session;

/**
 * Deletes this Space's rows and reports what went. The caller has run the guards.
 * `failure` is set after a failure halfway: `deleted` holds the rows that went,
 * and the project stays listed.
 */
export async function deleteProjectChats(
  path: string,
): Promise<ProjectPurge & { failure?: unknown }> {
  const rows = await listSessionsByProject(path);
  const spaceId = spacesView().activeSpaceId;
  const { deletedSessionIds, lastSpace, failure } = await deleteProjectInSpace(
    path,
    rows.filter((row) => row.spaceId === spaceId).map((row) => row.id),
  );
  return {
    deleted: new Set(deletedSessionIds),
    lastSpace,
    complete: failure === undefined,
    failure,
  };
}

/**
 * An open session with no row, so nobody else's: a blank chat joins this
 * window's Space on its first save, and a remote chat is never saved (T16 closes
 * the window's own tabs). A Mono's chat is never closed.
 */
function isOwnedWithoutRow(session: OpenSession): boolean {
  if (isMonoSession(session.id)) return false;
  if ((session.queuedMessages?.length ?? 0) > 0) return false;
  return (
    isRemoteProjectPath(session.cwd) ||
    (canPersistSession(session) && !shouldPersistSession(session))
  );
}

/**
 * What a purge may change in the window: only the sessions the backend deleted,
 * and those with no row (H2). A running session is never among them. A tab that
 * also holds another Space's session stays and loses only the deleted pane. A
 * tab with only files closes with the project's last Space.
 */
export function purgeWindowPlan(
  tabs: readonly WorkspaceTab[],
  sessions: OpenSession[],
  path: string,
  purge: ProjectPurge,
): {
  tabs: WorkspaceTab[];
  trimmed: Map<string, WorkspaceTab>;
  sessionIds: Set<string>;
} {
  const open = new Set(sessions.map((session) => session.id));
  const touched = new Set(
    sessions
      .filter(
        (session) =>
          !session.busy &&
          (purge.deleted.has(session.id) ||
            (purge.complete &&
              sameProjectPath(session.cwd, path) &&
              isOwnedWithoutRow(session))),
      )
      .map((session) => session.id),
  );
  const closing: WorkspaceTab[] = [];
  const trimmed = new Map<string, WorkspaceTab>();
  for (const tab of tabs) {
    const leaves = leafIds(tab.layout);
    const gone = leaves.filter((id) => touched.has(id));
    const others = leaves.some((id) => open.has(id) && !touched.has(id));
    if (gone.length > 0 && !others) {
      closing.push(tab);
    } else if (gone.length > 0) {
      trimmed.set(
        tab.id,
        gone.reduce((current, id) => closeLeaf(current, id) ?? current, tab),
      );
    } else if (
      !others &&
      purge.lastSpace &&
      filterTabsForProject([tab], sessions, path).length > 0
    ) {
      closing.push(tab);
    }
  }
  return { tabs: closing, trimmed, sessionIds: touched };
}

/**
 * The shared data a purge may clear, and the rail after it. Only T11 clears
 * anything: while another Space has the project these stay, or that Space
 * would lose its row, logo and settings.
 */
export function forgetPurgedProject(
  path: string,
  purge: ProjectPurge,
): RecentProject[] {
  if (!purge.lastSpace) return loadRecents();
  forgetProjectLocation(path);
  void clearProjectSettings(path);
  return forgetProject(path);
}
