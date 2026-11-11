import { basename } from "../../../platform/tauri/fs";
import { projectKey } from "../../../shared/lib/paths";
import {
  listSessionsByProject,
  type SessionSummary,
} from "../../sessions/data/sessionStore";
import type { Session } from "../../sessions/model/session";
import { loadSpaces, spacesView } from "../../spaces/model/activeSpace";
import {
  checkProjectArchive as checkArchive,
  previewProjectDelete,
  refusalMessage,
} from "../../spaces/model/spaces";
import {
  loadTabGroupLabels,
  resolveTabGroupLabel,
} from "../../workspace/model/tabGroups";
import { isRemoteProjectPath, sameProjectPath } from "./recents";

/**
 * Whether a project's Delete or Archive may go ahead.
 * The backend owns every guard it can see (spec T7-T11); this adds what only the
 * window knows, its queued messages and busy remote sessions.
 */
type OpenSession = Session;

export const SPACE_UNREAD_REFUSAL =
  "This window's Space could not be read. Try again.";

let openSessions = (): readonly OpenSession[] => [];

/** The window's sessions: only it knows their queued messages. */
export function setOpenSessionsSource(
  source: () => readonly OpenSession[],
): void {
  openSessions = source;
}

/** The name a refusal uses for the project, as its menu shows it. */
export function projectDisplayName(path: string): string {
  return resolveTabGroupLabel(
    projectKey(path),
    loadTabGroupLabels(),
    basename(path),
  );
}

/**
 * T8's window half. The backend reads running turns; only the window knows its
 * queued messages and its busy remote sessions, which have no row. A queue
 * counts unless a stored row says it is another Space's. `anySpace` (T9, T11)
 * counts every running session, and names the Space when none is here.
 */
export function windowRunningRefusal(
  path: string,
  spaceId: string,
  rows: readonly Pick<SessionSummary, "id" | "spaceId">[],
  sessions: readonly OpenSession[],
  anySpace: boolean,
): string | null {
  const spaceOf = new Map(rows.map((row) => [row.id, row.spaceId]));
  const running = sessions.filter(
    (session) =>
      sameProjectPath(session.cwd, path) &&
      (session.busy || (session.queuedMessages?.length ?? 0) > 0),
  );
  const here = (session: OpenSession) => {
    const stored = spaceOf.get(session.id);
    if (stored !== undefined) return stored === spaceId;
    return !session.busy || isRemoteProjectPath(session.cwd) || anySpace;
  };
  const name = projectDisplayName(path);
  if (running.some(here)) {
    return `A session is running in ${name} here. Stop it first.`;
  }
  const other = anySpace
    ? running.map((session) => spaceOf.get(session.id)).find(Boolean)
    : undefined;
  return other
    ? `A session is running in ${name} in ${spaceName(other)}. Stop it first.`
    : null;
}

function spaceName(id: string): string {
  return spacesView().spaces.find((space) => space.id === id)?.name ?? id;
}

/**
 * The Space this window shows. It reads again if no read has succeeded, and
 * throws if it still has no answer: a guard never passes for lack of one.
 */
async function readWindowSpace(): Promise<string> {
  if (!spacesView().loaded) await loadSpaces();
  const { loaded, activeSpaceId } = spacesView();
  if (!loaded) throw new Error(SPACE_UNREAD_REFUSAL);
  return activeSpaceId;
}

/**
 * T8's window half for this Space, for an event with no dialog (T5, T6). It
 * throws if the window's Space cannot be read.
 */
export async function checkWindowRunning(path: string): Promise<string | null> {
  return windowRunningRefusal(
    path,
    await readWindowSpace(),
    await listSessionsByProject(path),
    openSessions(),
    false,
  );
}

/**
 * Why Delete is refused right now, before its dialog opens and again before the
 * delete. The backend answers first: a Mono's refusal comes before a running
 * session's. On the last Space (T11) a session running in any Space refuses.
 */
export async function checkProjectDelete(path: string): Promise<string | null> {
  try {
    const { lastSpace } = await previewProjectDelete(path);
    return windowRunningRefusal(
      path,
      await readWindowSpace(),
      await listSessionsByProject(path),
      openSessions(),
      lastSpace,
    );
  } catch (error) {
    return refusalMessage(error);
  }
}

/**
 * Why Archive is refused right now (T9). Archive is global and closes tabs, so
 * a session running in P in any Space refuses it.
 */
export async function checkProjectArchive(path: string): Promise<string | null> {
  try {
    await checkArchive(path);
    return windowRunningRefusal(
      path,
      await readWindowSpace(),
      await listSessionsByProject(path),
      openSessions(),
      true,
    );
  } catch (error) {
    return refusalMessage(error);
  }
}
