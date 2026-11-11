/**
 * Views by id that a component reads one at a time, so a view that changes
 * every second (a hold's countdown) re-renders only the row showing it.
 */
export type ViewStore<V> = {
  get: (id: string) => V | undefined;
  /** Wakes `listener` only when the view for `id` is replaced. */
  subscribe: (id: string, listener: () => void) => () => void;
};

export function createViewStore<V>(
  initial: ReadonlyMap<string, V> = new Map(),
): ViewStore<V> & { set: (views: ReadonlyMap<string, V>) => void } {
  let views = initial;
  // Keyed by id: every tool row reads the holds store, and a countdown tick
  // must wake only the one row whose hold it is (#543 fable N5).
  const listeners = new Map<string, Set<() => void>>();
  return {
    get: (id) => views.get(id),
    subscribe(id, listener) {
      const forId = listeners.get(id) ?? new Set();
      listeners.set(id, forId.add(listener));
      return () => {
        forId.delete(listener);
        if (forId.size === 0) listeners.delete(id);
      };
    },
    set(next) {
      const previous = views;
      views = next;
      for (const [id, forId] of listeners)
        if (previous.get(id) !== next.get(id))
          for (const listener of [...forId]) listener();
    },
  };
}
