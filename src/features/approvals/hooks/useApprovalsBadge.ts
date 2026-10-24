import { useSyncExternalStore } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  APPROVALS_CHANGED,
  approvalsBadge,
  openCounts,
  type ApprovalsBadge,
  type OpenCounts,
} from "../model/approvals";

let counts: OpenCounts = { total: 0, byProject: {} };
const listeners = new Set<() => void>();
let changed: Promise<UnlistenFn | undefined> | undefined;
let latestRequest = 0;

async function load() {
  const request = ++latestRequest;
  try {
    const { total, byProject } = await openCounts();
    if (request !== latestRequest) return;
    counts = { total, byProject };
    for (const listener of listeners) listener();
  } catch (error) {
    console.error("monocode: could not count open approvals", error);
  }
}

function reloadOnReturn() {
  if (!document.hidden) void load();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1) {
    void load();
    window.addEventListener("focus", reloadOnReturn);
    document.addEventListener("visibilitychange", reloadOnReturn);
    changed = listen(APPROVALS_CHANGED, () => void load()).catch(
      () => undefined,
    );
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size > 0) return;
    window.removeEventListener("focus", reloadOnReturn);
    document.removeEventListener("visibilitychange", reloadOnReturn);
    void changed?.then((stop) => stop?.());
  };
}

export function useApprovalsBadge(): ApprovalsBadge {
  return approvalsBadge(useSyncExternalStore(subscribe, () => counts.total));
}

export function useOpenCounts(): OpenCounts {
  return useSyncExternalStore(subscribe, () => counts);
}
