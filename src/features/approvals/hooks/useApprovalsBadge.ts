import { useSyncExternalStore } from "react";
import {
  approvalsBadge,
  openCounts,
  type ApprovalsBadge,
} from "../model/approvals";

let openInEveryProject = 0;
const listeners = new Set<() => void>();

async function load() {
  try {
    openInEveryProject = (await openCounts()).total;
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
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size > 0) return;
    window.removeEventListener("focus", reloadOnReturn);
    document.removeEventListener("visibilitychange", reloadOnReturn);
  };
}

const snapshot = () => openInEveryProject;

export function useApprovalsBadge(): ApprovalsBadge {
  return approvalsBadge(useSyncExternalStore(subscribe, snapshot));
}
