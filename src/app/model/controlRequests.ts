import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/** One CLI call that `control.rs` sent to this window to run. */
export type ControlRequest = {
  id: string;
  namespace: string;
  sessionId: string;
  requestId: string;
  action: string;
  input: Record<string, unknown>;
  /** The turn that authorized the call, when the session runs one (design 2 step 4). */
  turnId: string | null;
};

/**
 * Runs the CLI calls sent to this window. Scoped to this window: Rust sends
 * each call to the window of its grant, and a global `listen` (target `Any`)
 * would run it in every window, as `main.tsx` notes for `quit_confirm`.
 */
export function listenForControlRequests(
  handle: (request: ControlRequest) => void,
): Promise<() => void> {
  return getCurrentWebviewWindow().listen<ControlRequest>(
    "monocode-control-request",
    ({ payload }) => handle(payload),
  );
}
