import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { HarnessEvent } from "../../core/types";

const sent: string[] = [];
let onLine: ((line: string) => void) | undefined;
let onExit: (() => void) | undefined;
const execChild = vi.fn(async () => "");
const killChild = vi.fn(async () => undefined);

vi.mock("../../core/child", () => ({
  resolveGrokBinary: async () => ({ path: "/fake/grok" }),
  execChild,
  // Rust places a helper and names the folder (ADE-189); it is never "/repo".
  spawnHelperChild: async () => "/placed",
  killChild,
  unwatchChild: () => undefined,
  watchChild: (
    _id: string,
    line: (value: string) => void,
    exit: () => void,
  ) => {
    onLine = line;
    onExit = exit;
  },
  writeChild: async (_id: string, line: string) => {
    sent.push(line);
  },
}));

const { runGrokTextPrompt, stopGrokTextPrompt, warmupGrokText } =
  await import("./grokText");

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
  onExit = undefined;
  execChild.mockReset();
  execChild.mockResolvedValue("");
  killChild.mockClear();
});

afterEach(async () => {
  await stopGrokTextPrompt();
});

it("forwards Grok text deltas without duplicating snapshots", async () => {
  const events: HarnessEvent[] = [];
  const result = runGrokTextPrompt({
    cwd: "/repo",
    prompt: "question",
    onEvent: (event) => events.push(event),
  });

  await waitFor(() => !!outbound("initialize"), "initialize");
  reply("initialize", {});
  await waitFor(() => !!outbound("session/new"), "session/new");
  // ADE-189: the agent works in the folder Rust placed it in, and its session
  // is deleted there.
  expect(outbound("session/new")?.params).toMatchObject({ cwd: "/placed" });
  reply("session/new", { sessionId: "550e8400-e29b-41d4-a716-446655440000" });
  await waitFor(() => !!outbound("session/set_model"), "session/set_model");
  reply("session/set_model", {});
  await waitFor(() => !!outbound("session/set_mode"), "session/set_mode");
  reply("session/set_mode", {});
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");

  onLine?.(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId: "550e8400-e29b-41d4-a716-446655440000",
        update: { sessionUpdate: "agent_message_chunk", content: "Hel" },
      },
    }),
  );
  onLine?.(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId: "550e8400-e29b-41d4-a716-446655440000",
        update: { sessionUpdate: "agent_message", content: "Hello" },
      },
    }),
  );
  reply("session/prompt", {});

  await expect(result).resolves.toBe("Hello");
  expect(execChild).toHaveBeenCalledWith(
    "/fake/grok",
    [
      "--no-auto-update",
      "sessions",
      "delete",
      "550e8400-e29b-41d4-a716-446655440000",
    ],
    "/placed",
    "grok",
    undefined,
    undefined,
    "monocode-grok-text",
  );
  expect(killChild.mock.invocationCallOrder.at(-1)).toBeLessThan(
    execChild.mock.invocationCallOrder[0],
  );
  expect(events).toEqual([
    { type: "message.delta", text: "Hel" },
    { type: "message.delta", text: "lo" },
  ]);
});

async function openTextSession(
  sessionId = "550e8400-e29b-41d4-a716-446655440000",
) {
  await waitFor(() => !!outbound("initialize"), "initialize");
  reply("initialize", {});
  await waitFor(() => !!outbound("session/new"), "session/new");
  reply("session/new", { sessionId });
  await waitFor(() => !!outbound("session/set_model"), "session/set_model");
  reply("session/set_model", {});
  await waitFor(() => !!outbound("session/set_mode"), "session/set_mode");
  reply("session/set_mode", {});
}

it("deletes only the temporary session when generation fails", async () => {
  const result = runGrokTextPrompt({ cwd: "/repo", prompt: "Generate text" });
  const rejected = expect(result).rejects.toThrow("Generation failed");
  await openTextSession();
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  const request = outbound("session/prompt");
  onLine?.(
    JSON.stringify({
      jsonrpc: "2.0",
      id: request?.id,
      error: { message: "Generation failed" },
    }),
  );
  await waitFor(() => !!outbound("session/cancel"), "session/cancel");
  await rejected;
  expect(execChild).toHaveBeenCalledTimes(1);
  expect(execChild).toHaveBeenCalledWith(
    "/fake/grok",
    [
      "--no-auto-update",
      "sessions",
      "delete",
      "550e8400-e29b-41d4-a716-446655440000",
    ],
    "/placed",
    "grok",
    undefined,
    undefined,
    "monocode-grok-text",
  );
});

it("deletes the temporary session when generation is cancelled", async () => {
  const controller = new AbortController();
  const result = runGrokTextPrompt({
    cwd: "/repo",
    prompt: "Generate text",
    signal: controller.signal,
  });
  await openTextSession();
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  controller.abort();
  await expect(result).rejects.toThrow("By-the-way request cancelled");
  expect(execChild).toHaveBeenCalledWith(
    "/fake/grok",
    [
      "--no-auto-update",
      "sessions",
      "delete",
      "550e8400-e29b-41d4-a716-446655440000",
    ],
    "/placed",
    "grok",
    undefined,
    undefined,
    "monocode-grok-text",
  );
});

it("cleans up warmup sessions before starting with a different model", async () => {
  const warmup = warmupGrokText("/repo");
  await openTextSession("550e8400-e29b-41d4-a716-446655440001");
  await warmup;
  sent.length = 0;

  const result = runGrokTextPrompt({
    cwd: "/repo",
    model: "other-model",
    prompt: "Generate text",
  });
  await openTextSession();
  expect(execChild).toHaveBeenCalledWith(
    "/fake/grok",
    [
      "--no-auto-update",
      "sessions",
      "delete",
      "550e8400-e29b-41d4-a716-446655440001",
    ],
    "/placed",
    "grok",
    undefined,
    undefined,
    "monocode-grok-text",
  );
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  reply("session/prompt", {});
  await result;
  expect(execChild).toHaveBeenCalledTimes(2);
});

it("cleans up its session even if the provider exits before teardown", async () => {
  const warmup = warmupGrokText("/repo");
  await openTextSession();
  await warmup;
  onExit?.();
  await stopGrokTextPrompt();
  expect(execChild).toHaveBeenCalledWith(
    "/fake/grok",
    [
      "--no-auto-update",
      "sessions",
      "delete",
      "550e8400-e29b-41d4-a716-446655440000",
    ],
    "/placed",
    "grok",
    undefined,
    undefined,
    "monocode-grok-text",
  );
});

it("does not pass an invalid provider session id to the cleanup command", async () => {
  const warmup = warmupGrokText("/repo");
  await openTextSession("--all");
  await warmup;
  await stopGrokTextPrompt();
  expect(execChild).not.toHaveBeenCalled();
});

// ADE-192: the shared ACP prompt flow sends exactly this prompt, keeps the
// caller's own timeout, and cancels its session when the answer never comes.
it("sends the prompt and cancels its session when the answer times out", async () => {
  const result = runGrokTextPrompt({
    cwd: "/repo",
    prompt: "Generate text",
    timeoutMs: 20,
  });
  const rejected = expect(result).rejects.toThrow("session/prompt timed out");
  await openTextSession();
  await waitFor(() => !!outbound("session/prompt"), "session/prompt");
  expect(outbound("session/prompt")?.params).toEqual({
    sessionId: "550e8400-e29b-41d4-a716-446655440000",
    prompt: [{ type: "text", text: "Generate text" }],
  });
  await waitFor(() => !!outbound("session/cancel"), "session/cancel");
  expect(outbound("session/cancel")?.params).toEqual({
    sessionId: "550e8400-e29b-41d4-a716-446655440000",
  });
  await rejected;
});

it("warms up no child for no folder", async () => {
  await warmupGrokText("~");
  await warmupGrokText("");
  expect(sent).toEqual([]);
});
