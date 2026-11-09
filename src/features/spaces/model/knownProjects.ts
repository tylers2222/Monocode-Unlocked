import {
  listKnownProjects,
  subscribeSpaceProjects,
  type KnownProject,
} from "./spaces";

/**
 * Every project any Space has, for the add list and a Mono's choices. Read at
 * boot and again on each write. An empty list until the first answer, and after
 * a failed read: the add list hides, and a Mono's choices stay the rail's.
 */
let known: readonly KnownProject[] = [];
let latest = 0;
const listeners = new Set<() => void>();

export function loadKnownProjects(): Promise<void> {
  const request = ++latest;
  return listKnownProjects()
    .then((reply) => (Array.isArray(reply) ? reply : []))
    .catch(() => [])
    .then((reply) => {
      if (request !== latest) return;
      known = reply;
      for (const listener of listeners) listener();
    });
}

export function knownProjects(): readonly KnownProject[] {
  return known;
}

let stopWatching: (() => void) | undefined;

/** Keeps the list current from now on. Safe to call more than once. */
export function watchKnownProjects(): void {
  if (stopWatching) return;
  stopWatching = subscribeSpaceProjects(() => void loadKnownProjects());
  void loadKnownProjects();
}

/** For suites that import this module statically. */
export function __knownProjectsTestReset(): void {
  stopWatching?.();
  stopWatching = undefined;
  known = [];
  latest = 0;
  listeners.clear();
}

export function subscribeKnownProjects(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
