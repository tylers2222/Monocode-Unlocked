import { isRemoteProjectPath } from "./recents";
import { remoteMachineFor } from "../../connections/model/connections";
import { parseRemotePath } from "../../connections/model/remoteProjects";
import { spacesView } from "../../spaces/model/activeSpace";
import { spaceProjectKey } from "../../spaces/model/spaceProjects";
import {
  listKnownProjects,
  listSpaceProjects,
  removeProjectFromSpace,
  refusalMessage,
} from "../../spaces/model/spaces";
import { checkWindowRunning } from "./projectDeleteCheck";

/** The name of the Space this window shows. */
export function activeSpaceName(): string {
  const { spaces, activeSpaceId } = spacesView();
  return spaces.find((space) => space.id === activeSpaceId)?.name ?? activeSpaceId;
}

/** The connected machine's name, for what a remote project's text says. */
export async function remoteMachineName(path: string): Promise<string> {
  const parsed = parseRemotePath(path);
  const machine = parsed
    ? await remoteMachineFor(parsed.environmentId).catch(() => undefined)
    : undefined;
  return machine?.name ?? "the machine";
}

/** What a project row's menu needs to know. The backend still decides. */
export type ProjectRowFacts = {
  /** Another Space has it, or a Mono holds it here: Remove is offered. */
  offersRemove: boolean;
  /** Set on a remote project. */
  machine?: string;
};

/** Offers Remove for a row in another Space, or one a Mono holds here (T7). */
export function rowOffersRemove(
  path: string,
  spaceId: string,
  known: readonly { path: string; spaces: string[] }[],
  monoHeld: readonly string[],
): boolean {
  const key = spaceProjectKey(path);
  const spaces =
    known.find((item) => spaceProjectKey(item.path) === key)?.spaces ?? [];
  return (
    spaces.some((space) => space !== spaceId) ||
    monoHeld.some((held) => spaceProjectKey(held) === key)
  );
}

/** A failed read fails open: Remove shows, and the backend answers it. */
export async function readProjectRowFacts(
  path: string,
): Promise<ProjectRowFacts> {
  const machine = isRemoteProjectPath(path)
    ? await remoteMachineName(path)
    : undefined;
  try {
    const [list, known] = await Promise.all([
      listSpaceProjects(),
      listKnownProjects(),
    ]);
    return {
      offersRemove: rowOffersRemove(path, list.spaceId, known, list.monoHeld),
      machine,
    };
  } catch {
    return { offersRemove: true, machine };
  }
}

/**
 * T5 or T6. The window refuses what only it can see, unless a Mono holds the
 * project: that refusal comes first, and it is the backend's. The backend then
 * decides in one transaction. `removed` false: it was not in this Space.
 */
export async function removeProjectFromActiveSpace(
  path: string,
): Promise<{ refusal: string } | { removed: boolean }> {
  try {
    const key = spaceProjectKey(path);
    const held = await listSpaceProjects().then(
      (list) => list.monoHeld.some((item) => spaceProjectKey(item) === key),
      () => false,
    );
    const refusal = held ? null : await checkWindowRunning(path);
    if (refusal) return { refusal };
    return { removed: await removeProjectFromSpace(path) };
  } catch (error) {
    return { refusal: refusalMessage(error) };
  }
}
