import { useEffect, useSyncExternalStore } from "react";
import { knownHomeDir } from "../../../shared/lib/paths";
import { isInGigaWorkspace } from "../../projects/model/reservedProjectName";
import {
  loadArchivedProjects,
  loadRecents,
  looksLikeProject,
  subscribeArchivedProjects,
  subscribeProjectPathsChanged,
} from "../../projects/model/recents";
import { spacesView, subscribeSpaces } from "./activeSpace";
import {
  knownProjects,
  subscribeKnownProjects,
  watchKnownProjects,
} from "./knownProjects";
import { spaceProjectKey } from "./spaceProjects";
import { FIRST_SPACE, type KnownProject } from "./spaces";

export type AddableProject = { path: string; spaceId: string };

/**
 * "Add from another Space…": every other Space's projects, plus recents in
 * Space 1 by the no-row rule, minus what this Space has, archived and workspace
 * paths. A recent with no entry has no stored fact, so it is in Space 1.
 */
export function addableProjects(input: {
  known: readonly KnownProject[];
  recents: readonly { path: string }[];
  archived: readonly { path: string }[];
  activeSpaceId: string;
  home: string | undefined;
}): AddableProject[] {
  const { activeSpaceId, home } = input;
  const key = (path: string) => spaceProjectKey(path, home);
  const archived = new Set(input.archived.map((item) => key(item.path)));
  const entries = new Set(input.known.map((item) => key(item.path)));
  const fromKnown = input.known.flatMap(({ path, spaces }) =>
    spaces.length > 0 && !spaces.includes(activeSpaceId)
      ? [{ path, spaceId: spaces[0] }]
      : [],
  );
  const fromRecents =
    activeSpaceId === FIRST_SPACE
      ? []
      : input.recents
          .filter((item) => !entries.has(key(item.path)))
          .map((item) => ({ path: item.path, spaceId: FIRST_SPACE }));
  return [...fromKnown, ...fromRecents].filter(
    ({ path }) =>
      looksLikeProject(path) &&
      !archived.has(key(path)) &&
      !isInGigaWorkspace(path, home),
  );
}

function subscribeLists(listener: () => void): () => void {
  const stops = [
    subscribeKnownProjects(listener),
    subscribeProjectPathsChanged(listener),
    subscribeArchivedProjects(listener),
  ];
  return () => {
    for (const stop of stops) stop();
  };
}

/**
 * Bumped by every list this reads, so `useSyncExternalStore` re-renders; the
 * lists themselves come from storage and are new arrays on each read.
 */
let revision = 0;
const subscribeRevision = (listener: () => void) =>
  subscribeLists(() => {
    revision += 1;
    listener();
  });

/** The add list for the active Space, kept current; empty hides the entry. */
export function useAddableProjects(): AddableProject[] {
  const spaces = useSyncExternalStore(subscribeSpaces, spacesView);
  useSyncExternalStore(subscribeRevision, () => revision);
  useEffect(watchKnownProjects, []);
  if (!spaces.loaded) return [];
  return addableProjects({
    known: knownProjects(),
    recents: loadRecents(),
    archived: loadArchivedProjects(),
    activeSpaceId: spaces.activeSpaceId,
    home: knownHomeDir(),
  });
}
