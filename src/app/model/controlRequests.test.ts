import { expect, it, vi } from "vitest";
import {
  listenForControlRequests,
  type ControlRequest,
} from "./controlRequests";

/**
 * Tauri's event routing as `tauri/src/event/listener.rs` does it: a window's
 * listener gets only events sent to its label; a global `listen` (target
 * `Any`) gets every event, whatever its target.
 */
const bus = vi.hoisted(() => {
  const listeners: {
    target: string;
    event: string;
    handler: (event: { payload: unknown }) => void;
  }[] = [];
  return {
    current: "main",
    listeners,
    emitTo(label: string, event: string, payload: unknown) {
      for (const listener of listeners)
        if (
          listener.event === event &&
          (listener.target === "Any" || listener.target === label)
        )
          listener.handler({ payload });
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (event: { payload: unknown }) => void) => {
    bus.listeners.push({ target: "Any", event, handler });
    return Promise.resolve(() => undefined);
  },
}));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => {
    const label = bus.current;
    return {
      listen: (
        event: string,
        handler: (event: { payload: unknown }) => void,
      ) => {
        bus.listeners.push({ target: label, event, handler });
        return Promise.resolve(() => undefined);
      },
    };
  },
}));

// Review PR 4a r4 m20. Defect: a CLI call that Rust sends to the window of
// its grant also runs in every other window.
it("runs a CLI call only in the window Rust sent it to", async () => {
  const ran: string[] = [];
  bus.current = "grant";
  await listenForControlRequests((request) => ran.push(`grant:${request.id}`));
  bus.current = "other";
  await listenForControlRequests((request) => ran.push(`other:${request.id}`));
  const request: ControlRequest = {
    id: "c1",
    namespace: "app",
    sessionId: "s",
    requestId: "r1",
    action: "messages.get_monos",
    input: {},
    turnId: "turn-1",
  };
  bus.emitTo("grant", "monocode-control-request", request);
  expect(ran).toEqual(["grant:c1"]);
});
