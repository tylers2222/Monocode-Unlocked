import { useCallback, useSyncExternalStore } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  APPROVALS_CHANGED,
  listApprovals,
  type Approval,
} from "../model/approvals";

export type CardList = { cards: Approval[] } | { error: string };

let lists: Record<string, CardList> = {};
const watchers = new Map<string, Set<() => void>>();
const newestRequest = new Map<string, number>();
let changed: Promise<UnlistenFn | undefined> | undefined;

async function load(project: string) {
  const request = (newestRequest.get(project) ?? 0) + 1;
  newestRequest.set(project, request);
  let list: CardList;
  try {
    list = { cards: await listApprovals(project) };
  } catch (error) {
    list = { error: String(error) };
  }
  if (newestRequest.get(project) !== request) return;
  lists = { ...lists, [project]: list };
  for (const listener of watchers.get(project) ?? []) listener();
}

/** Reloads every list a board shows; a refused decide means the list is stale. */
export function reloadApprovalLists() {
  for (const project of watchers.keys()) void load(project);
}

function watch(project: string, listener: () => void) {
  if (watchers.size === 0)
    changed = listen(APPROVALS_CHANGED, reloadApprovalLists).catch(
      () => undefined,
    );
  const mine = watchers.get(project) ?? new Set();
  watchers.set(project, mine.add(listener));
  void load(project);
  return () => {
    mine.delete(listener);
    if (mine.size === 0) watchers.delete(project);
    if (watchers.size === 0) void changed?.then((stop) => stop?.());
  };
}

export function useApprovalList(project: string): CardList | undefined {
  const subscribe = useCallback(
    (listener: () => void) => watch(project, listener),
    [project],
  );
  return useSyncExternalStore(subscribe, () => lists[project]);
}
