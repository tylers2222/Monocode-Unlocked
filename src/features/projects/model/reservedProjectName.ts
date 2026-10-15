/**
 * "MonoCode" names the Giga workspace at `~/MonoCode`. No other folder may
 * become a project under that name, and no project may be labelled with it,
 * so a Giga and its user never mistake one for the workspace.
 */
import { knownHomeDir, pathKey } from "../../../shared/lib/paths";
import { gigaWorkspaceRoot } from "../../monos/model/gigaWorkspace";
import {
  loadRecents,
  normalizeProjectPath,
  sameProjectPath,
} from "./recents";

export const RESERVED_PROJECT_NAME_ERROR =
  "MonoCode is reserved for the Giga workspace. Rename the folder or choose another.";

const NOTICED_KEY = "monocode.reservedProjectNamesNoticed";

function isReservedName(name: string): boolean {
  return name.trim().toLowerCase() === "monocode";
}

/**
 * `~/MonoCode` on this computer. A project on another machine never is: its
 * `remote://` key never equals a local path.
 */
export function isGigaWorkspaceProject(
  path: string,
  home: string | undefined,
): boolean {
  return (
    home !== undefined &&
    sameProjectPath(normalizeProjectPath(path), gigaWorkspaceRoot(home))
  );
}

/** Why a folder may not become a project, or `null` when it may. */
export function reservedProjectNameError(
  path: string,
  home: string | undefined,
): string | null {
  const name = normalizeProjectPath(path).split("/").pop() ?? "";
  return isReservedName(name) && !isGigaWorkspaceProject(path, home)
    ? RESERVED_PROJECT_NAME_ERROR
    : null;
}

/** Why a project may not show `label` as its name, or `null` when it may. */
export function reservedProjectLabelError(
  path: string,
  label: string,
  home: string | undefined,
): string | null {
  return isReservedName(label) && !isGigaWorkspaceProject(path, home)
    ? RESERVED_PROJECT_NAME_ERROR
    : null;
}

/**
 * "Open folder…": opens every picked folder the rule allows, and reports the
 * refusal for the rest.
 */
export async function openPickedFolders(
  pick: () => Promise<string[]>,
  open: (paths: string[]) => void,
  refuse: (error: string) => void,
): Promise<void> {
  const home = knownHomeDir();
  const picked = await pick();
  const allowed = picked.filter((path) => !reservedProjectNameError(path, home));
  open(allowed);
  if (allowed.length < picked.length) refuse(RESERVED_PROJECT_NAME_ERROR);
}

/**
 * Projects added before the name was reserved keep working. Each one named
 * MonoCode outside `~/MonoCode` is told about once, ever.
 */
export function noticeReservedProjects(show: (text: string) => void): void {
  const home = knownHomeDir();
  let noticed: string[];
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(NOTICED_KEY) ?? "[]");
    noticed = Array.isArray(parsed)
      ? parsed.filter((key): key is string => typeof key === "string")
      : [];
  } catch {
    noticed = [];
  }
  const due = loadRecents()
    .map((project) => project.path)
    .filter(
      (path) =>
        reservedProjectNameError(path, home) !== null &&
        !noticed.includes(pathKey(path)),
    );
  if (due.length === 0) return;
  try {
    localStorage.setItem(
      NOTICED_KEY,
      JSON.stringify([...noticed, ...due.map(pathKey)]),
    );
  } catch {
    // private mode / quota: told again next launch
  }
  show(
    `MonoCode is reserved for the Giga workspace at ~/MonoCode. These projects keep working, but rename their folders so they are not mistaken for it:\n\n${due.join("\n")}`,
  );
}
