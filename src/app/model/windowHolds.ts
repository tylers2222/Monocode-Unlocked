import { invoke } from "@tauri-apps/api/core";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { NOTIFICATION_CLICK_EVENT } from "../../features/notifications/model/notifications";

/**
 * ADE-140/141: the chats this window has loaded that another window holds,
 * each with that window's label: Rust's last answer (`Inner::holder` decides:
 * the window of the chat's turn, else the first window to load it). Only that
 * window runs the chat's turns. An external store, as the Mono roster is
 * (`subscribeMonos`): only Rust's answers write it, and views and the queue
 * pump read it with `useSyncExternalStore`.
 */
let heldElsewhere: ReadonlyMap<string, string> = new Map();
const listeners = new Set<() => void>();

export function subscribeHolds(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function holdsSnapshot(): ReadonlyMap<string, string> {
  return heldElsewhere;
}

/**
 * "Go to that window" (ADE-141 A1-A2): the holding window focuses itself and
 * opens the chat, as a notification click does there.
 */
export function goToHolder(sessionId: string): void {
  const holder = heldElsewhere.get(sessionId);
  if (holder)
    void emitTo(holder, NOTIFICATION_CLICK_EVENT, sessionId).catch(
      console.error,
    );
}

/** Reports every chat this window has loaded, in place of its last report. */
export async function reportLoadedSessions(
  sessionIds: Iterable<string>,
): Promise<void> {
  const held = await invoke<Record<string, string>>("control_loaded_sessions", {
    sessions: [...sessionIds],
  });
  heldElsewhere = new Map(Object.entries(held));
  for (const listener of listeners) listener();
}

/**
 * `control-holds-changed`: a window let go of chats another window has
 * loaded. This window reports again, and `took` gets the loaded chats it
 * holds now.
 */
export function listenForHoldChanges(
  loaded: () => string[],
  took: (sessionIds: string[]) => void,
): () => void {
  const listening = getCurrentWebviewWindow().listen(
    "control-holds-changed",
    () => {
      const before = heldElsewhere;
      const ids = loaded();
      reportLoadedSessions(ids)
        .then(() =>
          took(ids.filter((id) => before.has(id) && !heldElsewhere.has(id))),
        )
        .catch(console.error);
    },
  );
  return () => void listening.then((unlisten) => unlisten());
}
