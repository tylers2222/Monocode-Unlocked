import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/**
 * ADE-140/141: the chats this window has loaded that another window holds,
 * each with that window's label, as Rust last answered (`Inner::holder`: the
 * window of the chat's turn, else the first window to load it). Only the
 * holder binds a chat and starts its harness child: a second child under the
 * same id would end the holder's.
 */
let heldElsewhere: ReadonlyMap<string, string> = new Map();

export function holderElsewhere(sessionId: string): string | undefined {
  return heldElsewhere.get(sessionId);
}

/** Reports every chat this window has loaded, in place of its last report. */
export async function reportLoadedSessions(
  sessionIds: Iterable<string>,
): Promise<void> {
  const held = await invoke<Record<string, string>>(
    "control_loaded_sessions",
    { sessions: [...sessionIds] },
  );
  heldElsewhere = new Map(Object.entries(held));
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
