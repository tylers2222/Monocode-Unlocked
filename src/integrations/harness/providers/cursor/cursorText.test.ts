import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { HarnessEvent } from "../../core/types";

const sent: string[] = [];
let onLine: ((line: string) => void) | undefined;
// Rust places a helper and names the folder (ADE-189); it is never "/repo".
const spawnHelperChild = vi.hoisted(() =>
  vi.fn<(...args: unknown[]) => Promise<string>>(async () => "/placed"),
);

vi.mock("../../core/child", () => ({
  resolveCursorBinary: async () => ({ path: "/fake/cursor-agent" }),
  spawnHelperChild,
  killChild: async () => undefined,
  unwatchChild: () => undefined,
  watchChild: (_id: string, line: (value: string) => void) => {
    onLine = line;
  },
  writeChild: async (_id: string, line: string) => {
    sent.push(line);
  },
}));

const { runCursorTextPrompt, stopCursorTextPrompt, warmupCursorText } =
  await import("./cursorText");

function messages() {
  return sent.map((line) => JSON.parse(line) as Record<string, unknown>);
}

function outbound(method: string) {
  return messages().find((message) => message.method === method);
}

function reply(method: string, result: unknown) {
  const request = outbound(method);
  onLine?.(JSON.stringify({ jsonrpc: "2.0", id: request?.id, result }));
}

async function waitFor(predicate: () => boolean, label: string) {
  for (let index = 0; index < 200; index += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${label}`);
}

beforeEach(() => {
  sent.length = 0;
  onLine = undefined;
});

/** Answers a fresh text child's setup; returns its `session/new` params. */
async function startsTextChild() {
  for (const method of ["initialize", "authenticate"]) {
    await waitFor(() => !!outbound(method), method);
    reply(method, {});
  }
  await waitFor(() => !!outbound("session/new"), "session/new");
  const params = outbound("session/new")?.params;
  reply("session/new", { sessionId: "cursor_text", configOptions: [] });
  for (const method of ["session/set_mode", "session/set_config_option"]) {
    await waitFor(() => !!outbound(method), method);
    reply(method, {});
  }
  return params;
}

// ADE-189/192: a warm child is told the folder it was placed for, so a
// prompt for another folder or session starts a child of its own.
it("reuses a warm text child only for the folder and session it was placed for", async () => {
  spawnHelperChild.mockClear();
  const warm = warmupCursorText("/repo");
  expect(await startsTextChild()).toMatchObject({ cwd: "/placed" });
  await warm;
  // Another model in the same folder reopens the warm child, not a new one.
  sent.length = 0;
  const same = runCursorTextPrompt({ cwd: "/repo", model: "gpt-5", prompt: "q" });
  await waitFor(() => !!outbound("session/new"), "session/new");
  expect(outbound("session/new")?.params).toMatchObject({ cwd: "/placed" });
  reply("session/new", { sessionId: "cursor_text", configOptions: [] });
  for (const method of ["session/set_mode", "session/set_config_option"]) {
    await waitFor(() => !!outbound(method), method);
    reply(method, {});
  }
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  expect(spawnHelperChild).toHaveBeenCalledTimes(1);
  reply("session/prompt", {});
  await same;

  sent.length = 0;
  const again = warmupCursorText("/repo");
  await startsTextChild();
  await again;
  spawnHelperChild.mockResolvedValueOnce("/placed-other");
  sent.length = 0;
  const other = runCursorTextPrompt({ cwd: "/other", prompt: "q" });
  expect(await startsTextChild()).toMatchObject({ cwd: "/placed-other" });
  expect(spawnHelperChild).toHaveBeenCalledTimes(3);
  expect(spawnHelperChild.mock.calls[2]?.[4]).toBe("/other");
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  reply("session/prompt", {});
  await other;

  // ADE-192: a warm child started for no session never serves a session's
  // prompt, even for another model in its own folder: the session gets its
  // own child, placed for it, and its agent is told that folder.
  sent.length = 0;
  const third = warmupCursorText("/repo");
  await startsTextChild();
  await third;
  spawnHelperChild.mockResolvedValueOnce("/space-x");
  sent.length = 0;
  const owned = runCursorTextPrompt({
    cwd: "/repo",
    owner: "chat-x",
    model: "gpt-5",
    prompt: "q",
  });
  expect(await startsTextChild()).toMatchObject({ cwd: "/space-x" });
  expect(spawnHelperChild).toHaveBeenCalledTimes(5);
  expect(spawnHelperChild.mock.calls[4]?.[0]).toBe("chat-x");
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  reply("session/prompt", {});
  await owned;
});

afterEach(async () => {
  await stopCursorTextPrompt();
});

it("forwards Cursor text deltas without duplicating snapshots", async () => {
  const events: HarnessEvent[] = [];
  const result = runCursorTextPrompt({
    cwd: "/repo",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => !!outbound("initialize"), "initialize");
  // ADE-189: the text child is a helper of no session, so Rust places it.
  expect(spawnHelperChild).toHaveBeenCalledWith(
    undefined,
    "monocode-text",
    "/fake/cursor-agent",
    ["acp"],
    "/repo",
    undefined,
    "cursor",
  );
  reply("initialize", {});
  await waitFor(() => !!outbound("authenticate"), "authenticate");
  reply("authenticate", {});
  await waitFor(() => !!outbound("session/new"), "session/new");
  expect(outbound("session/new")?.params).toMatchObject({ cwd: "/placed" });
  reply("session/new", {
    sessionId: "cursor_text",
    configOptions: [{ id: "model", category: "model" }],
  });
  await waitFor(() => !!outbound("session/set_mode"), "session/set_mode");
  reply("session/set_mode", {});
  await waitFor(
    () => !!outbound("session/set_config_option"),
    "session/set_config_option",
  );
  reply("session/set_config_option", {});
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");

  onLine?.(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId: "cursor_text",
        update: { sessionUpdate: "agent_message_chunk", content: "Hel" },
      },
    }),
  );
  onLine?.(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId: "cursor_text",
        update: { sessionUpdate: "agent_message", content: "Hello" },
      },
    }),
  );
  reply("session/prompt", {});

  await expect(result).resolves.toBe("Hello");
  expect(events).toEqual([
    { type: "message.delta", text: "Hel" },
    { type: "message.delta", text: "lo" },
  ]);
});
