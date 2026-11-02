import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { HarnessEvent } from "../../core/types";

let onStdout: ((line: string) => void) | undefined;
let onSseEvent: ((event: Record<string, unknown>) => void) | undefined;
let finishPrompt:
  ((value: { status: number; body: string }) => void) | undefined;
let promptStarted = false;

const harnessHttp = vi.fn(
  async (input: {
    url: string;
    method: string;
  }): Promise<{ status: number; body: string }> => {
    const url = new URL(input.url);
    if (
      input.method === "POST" &&
      (url.pathname === "/session" || url.pathname === "/api/session")
    ) {
      return { status: 200, body: JSON.stringify({ id: "text_session" }) };
    }
    if (
      input.method === "POST" &&
      url.pathname === "/session/text_session/message"
    ) {
      promptStarted = true;
      return new Promise((resolve) => {
        finishPrompt = resolve;
      });
    }
    return { status: 204, body: "" };
  },
);

// Rust places a helper and names the folder (ADE-189); it is never "/repo".
// v1's `serve` child prints the server URL as it starts.
const spawnHelperChild = vi.hoisted(() =>
  vi.fn<(...args: unknown[]) => Promise<string>>(async () => {
    onStdout?.("opencode server listening on http://127.0.0.1:4096");
    return "/placed";
  }),
);
const execHelper = vi.hoisted(() =>
  vi.fn<(...args: unknown[]) => Promise<{ stdout: string; cwd: string }>>(
    async () => ({ stdout: "opencode 1.14.19", cwd: "/neutral" }),
  ),
);

vi.mock("../../core/child", () => ({
  closeHarnessSse: async () => undefined,
  // The v2 background service, which is machine-wide.
  execChild: async (_path: string, args: string[]) =>
    args[1] === "status" ? "http://127.0.0.1:4097" : "secret",
  execHelper,
  freeHarnessPort: async () => 4096,
  harnessHttp,
  killChild: async () => undefined,
  openHarnessSse: async () => undefined,
  resolveOpenCodeBinary: async () => ({ path: "/fake/opencode" }),
  spawnHelperChild,
  unwatchChild: () => undefined,
  watchChild: (_id: string, stdout: (line: string) => void) => {
    onStdout = stdout;
  },
  watchSse: (_id: string, event: (data: string) => void) => {
    onSseEvent = (value) => event(JSON.stringify(value));
  },
}));

const { runOpenCodeTextPrompt, stopOpenCodeTextPrompt } =
  await import("./opencodeText");

async function waitFor(predicate: () => boolean, label: string) {
  for (let index = 0; index < 200; index += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${label}`);
}

function message(id: string, role: "assistant" | "user") {
  onSseEvent?.({
    type: "message.updated",
    properties: {
      info: { id, role, sessionID: "text_session" },
    },
  });
}

function part(messageID: string, text: string, ended = false) {
  onSseEvent?.({
    type: "message.part.updated",
    properties: {
      part: {
        id: `part_${messageID}`,
        messageID,
        sessionID: "text_session",
        type: "text",
        text,
        time: ended ? { start: 1, end: 2 } : { start: 1 },
      },
    },
  });
}

function delta(messageID: string, text: string) {
  onSseEvent?.({
    type: "message.part.delta",
    properties: {
      sessionID: "text_session",
      partID: `part_${messageID}`,
      delta: text,
    },
  });
}

beforeEach(() => {
  onStdout = undefined;
  onSseEvent = undefined;
  finishPrompt = undefined;
  promptStarted = false;
  harnessHttp.mockClear();
});

afterEach(async () => {
  await stopOpenCodeTextPrompt();
});

it("forwards only incremental OpenCode assistant text", async () => {
  const events: HarnessEvent[] = [];
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openrouter/anthropic/claude-haiku",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => promptStarted, "prompt");
  message("user_message", "user");
  part("user_message", "question");
  message("assistant_message", "assistant");
  part("assistant_message", "Hel");
  part("assistant_message", "Hello");
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({
      info: {},
      parts: [{ type: "text", text: "Hello" }],
    }),
  });

  await expect(result).resolves.toBe("Hello");
  expect(harnessHttp).toHaveBeenCalledWith(
    expect.objectContaining({
      method: "DELETE",
      url: "http://127.0.0.1:4096/session/text_session?directory=%2Fplaced",
    }),
  );
  expect(events).toEqual([
    { type: "message.delta", text: "Hel" },
    { type: "message.delta", text: "lo" },
  ]);
});

it("does not replay a delta after a stale snapshot", async () => {
  const events: HarnessEvent[] = [];
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openrouter/anthropic/claude-haiku",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => promptStarted, "prompt");
  message("assistant_message", "assistant");
  part("assistant_message", "Hello");
  part("assistant_message", "Hel");
  delta("assistant_message", "!");
  message("assistant_message", "assistant");
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({
      info: {},
      parts: [{ type: "text", text: "Hello!" }],
    }),
  });

  await expect(result).resolves.toBe("Hello!");
  expect(events).toEqual([
    { type: "message.delta", text: "Hello" },
    { type: "message.delta", text: "!" },
  ]);
});

it("does not replay a delta after an out-of-order completed snapshot", async () => {
  const events: HarnessEvent[] = [];
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openrouter/anthropic/claude-haiku",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => promptStarted, "prompt");
  message("assistant_message", "assistant");
  part("assistant_message", "Hello", true);
  part("assistant_message", "");
  delta("assistant_message", "lo");
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({
      info: {},
      parts: [{ type: "text", text: "Hello" }],
    }),
  });

  await expect(result).resolves.toBe("Hello");
  expect(events).toEqual([{ type: "message.delta", text: "Hello" }]);
});

it("buffers a delta that arrives before its part snapshot", async () => {
  const events: HarnessEvent[] = [];
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openrouter/anthropic/claude-haiku",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => promptStarted, "prompt");
  delta("assistant_message", "Hel");
  message("assistant_message", "assistant");
  part("assistant_message", "");
  delta("assistant_message", "lo");
  part("assistant_message", "Hello", true);
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({
      info: {},
      parts: [{ type: "text", text: "Hello" }],
    }),
  });

  await expect(result).resolves.toBe("Hello");
  expect(events).toEqual([
    { type: "message.delta", text: "Hel" },
    { type: "message.delta", text: "lo" },
  ]);
});

it("deletes the generated-text session when the provider returns an error", async () => {
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openai/test",
    prompt: "Generate text",
  });
  await waitFor(() => promptStarted, "prompt");
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({ info: { error: { message: "Generation failed" } } }),
  });
  await expect(result).rejects.toThrow("Generation failed");
  expect(harnessHttp).toHaveBeenCalledWith(
    expect.objectContaining({
      method: "DELETE",
      url: "http://127.0.0.1:4096/session/text_session?directory=%2Fplaced",
    }),
  );
});

// ADE-189: v2 spawns no child, so the folder its `--version` exec is placed
// in is the `directory` each request names. The exec carries the text child's
// key, which INTERIM keeps in its caller's folder; Rust names that folder.
it("names the v2 directory where the keyed version exec was placed", async () => {
  execHelper.mockResolvedValueOnce({ stdout: "opencode 2.0.15", cwd: "/kept" });
  harnessHttp.mockClear();
  // The mocked v2 service answers the prompt with no text.
  await expect(
    runOpenCodeTextPrompt({ cwd: "/repo", prompt: "Generate text" }),
  ).rejects.toThrow("OpenCode returned empty output.");
  expect(execHelper).toHaveBeenLastCalledWith(
    "/fake/opencode",
    ["--version"],
    "/repo",
    "opencode",
    undefined,
    undefined,
    "monocode-opencode-text",
  );
  const [created] = harnessHttp.mock.calls[0]!;
  expect(new URL(created.url).searchParams.get("directory")).toBe("/kept");
});

it("deletes the generated-text session when generation is cancelled", async () => {
  const controller = new AbortController();
  const result = runOpenCodeTextPrompt({
    cwd: "/repo",
    model: "openai/test",
    prompt: "Generate text",
    signal: controller.signal,
  });
  await waitFor(() => promptStarted, "prompt");
  controller.abort();
  await expect(result).rejects.toThrow("By-the-way request cancelled");
  expect(harnessHttp).toHaveBeenCalledWith(
    expect.objectContaining({
      method: "DELETE",
      url: "http://127.0.0.1:4096/session/text_session?directory=%2Fplaced",
    }),
  );
});

// ADE-192: a prompt for chat-a aborted while its child starts leaves that
// child warm. chat-b's prompt in the same folder never reuses it: its
// `--version` exec and its `serve` child are placed for chat-b, and its
// session names chat-b's folder.
it("never reuses a text child left warm for another session", async () => {
  const first = new AbortController();
  execHelper.mockImplementationOnce(async () => {
    first.abort();
    return { stdout: "opencode 1.14.19", cwd: "/neutral" };
  });
  spawnHelperChild.mockClear();
  await expect(
    runOpenCodeTextPrompt({
      cwd: "/repo",
      owner: "chat-a",
      prompt: "A",
      signal: first.signal,
    }),
  ).rejects.toThrow();
  spawnHelperChild.mockImplementationOnce(async () => {
    onStdout?.("opencode server listening on http://127.0.0.1:4096");
    return "/space-b";
  });
  harnessHttp.mockClear();
  const other = runOpenCodeTextPrompt({
    cwd: "/repo",
    owner: "chat-b",
    prompt: "B",
  });
  await waitFor(() => promptStarted, "chat-b's prompt");
  finishPrompt?.({
    status: 200,
    body: JSON.stringify({ info: {}, parts: [{ type: "text", text: "B" }] }),
  });
  await expect(other).resolves.toBe("B");
  expect(spawnHelperChild.mock.calls.map((call) => call[0])).toEqual([
    "chat-a",
    "chat-b",
  ]);
  expect(execHelper.mock.calls.slice(-2).map((call) => call[5])).toEqual([
    "chat-a",
    "chat-b",
  ]);
  const created = harnessHttp.mock.calls
    .map(([request]) => new URL(request.url))
    .find((url) => url.pathname === "/session");
  expect(created?.searchParams.get("directory")).toBe("/space-b");
});
