import { useSyncExternalStore } from "react";
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
import { knownProjects, subscribeKnownProjects } from "./knownProjects";
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
  const fromKnown = input.known
    .filter(({ spaces }) => spaces.length > 0 && !spaces.includes(activeSpaceId))
    .map(({ path, spaces }) => ({ path, spaceId: spaces[0] }));
  const fromRecents = input.recents
    .filter(
      (item) => activeSpaceId !== FIRST_SPACE && !entries.has(key(item.path)),
    )
    .map((item) => ({ path: item.path, spaceId: FIRST_SPACE }));
  return [...fromKnown, ...fromRecents].filter(
    ({ path }) =>
      looksLikeProject(path) &&
      !archived.has(key(path)) &&
      !isInGigaWorkspace(path, home),
  );
}

/**
 * Replaced on every change to a list this reads, so `useSyncExternalStore`
 * re-renders; the lists themselves come from storage and are new arrays on
 * each read.
 */
let revision: object = {};

/** Calls `listener` after any change to the known, recent or archived lists. */
export function subscribeAddableProjects(listener: () => void): () => void {
  const onChange = () => {
    revision = {};
    listener();
  };
  const stops = [
    subscribeKnownProjects(onChange),
    subscribeProjectPathsChanged(onChange),
    subscribeArchivedProjects(onChange),
  ];
  return () => {
    for (const stop of stops) stop();
  };
}

/**
 * The add list for the active Space, kept current; empty hides the entry.
 * main.tsx keeps the known list current (`watchKnownProjects`).
 */
export function useAddableProjects(): AddableProject[] {
  const spaces = useSyncExternalStore(subscribeSpaces, spacesView);
  useSyncExternalStore(subscribeAddableProjects, () => revision);
  if (!spaces.loaded) return [];
  return addableProjects({
    known: knownProjects(),
    recents: loadRecents(),
    archived: loadArchivedProjects(),
    activeSpaceId: spaces.activeSpaceId,
    home: knownHomeDir(),
  });
}
