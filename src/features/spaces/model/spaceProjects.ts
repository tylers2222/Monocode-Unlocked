import { knownHomeDir, pathKey } from "../../../shared/lib/paths";
import {
  archiveProject,
  loadArchivedProjects,
  loadRecents,
  looksLikeProject,
  normalizeProjectPath,
  rememberProject,
  sameProjectPath,
  type ArchivedProject,
  type RecentProject,
} from "../../projects/model/recents";
import {
  isGigaWorkspaceProject,
  isInGigaWorkspace,
} from "../../projects/model/reservedProjectName";
import {
  addProjectToSpace,
  notifySpaceProjectsChanged,
  type SpaceProjectList,
} from "./spaces";

/** The window's list: not answered yet, unreadable (T14), or the reply. */
export type SpaceProjectState = SpaceProjectList | "loading" | "failed";

/** The window's form of `project_key` (I10): `/a`, `/a/` and `~/a` are one. */
export function spaceProjectKey(path: string, home = knownHomeDir()): string {
  const normalized = normalizeProjectPath(path);
  return pathKey(
    home && normalized.startsWith("~/")
      ? `${home}${normalized.slice(1)}`
      : normalized,
  );
}

/**
 * The one rule for the rail and the picker: `shown(S)` is the recents in the
 * Space, then every other project in `projects`, then the open project, which
 * keeps its row while open whatever its Space. Until the first answer only the
 * open project shows; a failed read shows every recent, so no work is hidden.
 * Under the Giga workspace nothing is listed; its root needs a Mono in S; an
 * archived project needs one too.
 */
export function shownProjects(
  recents: readonly RecentProject[],
  open: string,
  list: SpaceProjectState,
  archived: readonly ArchivedProject[] = loadArchivedProjects(),
  home: string | undefined = knownHomeDir(),
): RecentProject[] {
  const key = (path: string) => spaceProjectKey(path, home);
  const shown = new Map<string, RecentProject>();
  const underWorkspace = (path: string) =>
    isInGigaWorkspace(path, home) && !isGigaWorkspaceProject(path, home);

  if (list === "failed") {
    for (const item of recents) {
      if (!underWorkspace(item.path)) shown.set(key(item.path), item);
    }
  } else if (list !== "loading") {
    const listed = new Set(list.projects.map((item) => key(item.path)));
    const elsewhere = new Set(list.assignedElsewhere.map(key));
    const monoHeld = new Set(list.monoHeld.map(key));
    const hidden = new Set(archived.map((item) => key(item.path)));
    const inSpace = (path: string) => {
      const id = key(path);
      if (underWorkspace(path)) return false;
      if (monoHeld.has(id)) return true;
      if (isGigaWorkspaceProject(path, home) || hidden.has(id)) return false;
      return listed.has(id) || (list.noRowIsHere && !elsewhere.has(id));
    };
    for (const item of recents) {
      if (inSpace(item.path)) shown.set(key(item.path), item);
    }
    for (const { path } of list.projects) {
      if (!shown.has(key(path)) && inSpace(path)) {
        shown.set(key(path), { path, openedAt: 0 });
      }
    }
  }
  if (open && !shown.has(key(open))) {
    shown.set(key(open), { path: open, openedAt: 0 });
  }
  return [...shown.values()];
}

/** Where a window goes when it leaves `path`: the next project in `shown(S)`. */
export function projectAfterLeaving(
  recents: readonly RecentProject[],
  path: string,
  list: SpaceProjectState,
): RecentProject | undefined {
  const left = spaceProjectKey(path);
  return shownProjects(recents, "", list).find(
    (item) => looksLikeProject(item.path) && spaceProjectKey(item.path) !== left,
  );
}

/**
 * T1 and T2, for a path the user added on purpose: Open folder, the remote
 * dialog and restore. Selecting a project on the rail never lists it (H1), or
 * stepping through projects would undo T5. Run it before the path is remembered:
 * `knownToWindow` asks whether the window had it already.
 */
export async function listProjectInSpace(path: string): Promise<boolean> {
  const known = [...loadRecents(), ...loadArchivedProjects()].some((item) =>
    sameProjectPath(item.path, path),
  );
  try {
    await addProjectToSpace(path, { knownToWindow: known });
    return true;
  } catch (error) {
    console.warn(`monocode: ${path} was not listed:`, error);
    return false;
  }
}

/**
 * Open folder: lists each picked path in the Space, then opens them. Opening
 * clears an archive entry, so other windows read the archive again.
 */
export function openListedProjects(
  open: (paths: string[]) => void,
): (paths: string[]) => void {
  return (paths) => {
    for (const path of paths) void listProjectInSpace(path);
    open(paths);
    notifySpaceProjectsChanged();
  };
}

/** Archive writes only localStorage; other windows read the archive again. */
export function archiveProjectInSpace(path: string): RecentProject[] {
  const remaining = archiveProject(path);
  notifySpaceProjectsChanged();
  return remaining;
}

/** Restore (T2): the archive entry clears only once the Space has the project. */
export async function restoreProjectInSpace(
  path: string,
  remembered: (recents: RecentProject[]) => void,
  select: (path: string) => void,
): Promise<void> {
  if (!(await listProjectInSpace(path))) return;
  remembered(rememberProject(path));
  notifySpaceProjectsChanged();
  select(path);
}
