/**
 * Views by id that a component reads one at a time, so a view that changes
 * every second (a hold's countdown) re-renders only the row showing it.
 */
export type ViewStore<V> = {
  get: (id: string) => V | undefined;
  subscribe: (listener: () => void) => () => void;
};

export function createViewStore<V>(
  initial: ReadonlyMap<string, V> = new Map(),
): ViewStore<V> & { set: (views: ReadonlyMap<string, V>) => void } {
  let views = initial;
  const listeners = new Set<() => void>();
  return {
    get: (id) => views.get(id),
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    set(next) {
      views = next;
      for (const listener of listeners) listener();
    },
  };
}
