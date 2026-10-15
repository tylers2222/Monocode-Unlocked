// @vitest-environment happy-dom
import { expect, it, vi } from "vitest";
import { newSession, type Block } from "../../sessions/model/session";
import type { Mono } from "../../monos/model/mono";
import {
  sessionConversationPage,
  type SessionReadOptions,
} from "./sessionConversation";
import { handleAgentApp, repeatedAppCall, type AgentAppHost } from "./agentApp";
import { appErrorResponse } from "./appActionError";

/** The caller's Mono, a Giga with no projects and no chat loaded. */
const captain: Mono = {
  id: "mono-1",
  name: "Captain Jack",
  mascot: "skull",
  color: "#fff",
  projects: [],
  sessionId: "captain-chat",
  giga: true,
};
const shroom: Mono = {
  id: "mono-2",
  mascot: "mushroom",
  color: "#9c9",
  projects: ["/code/app"],
  sessionId: "shroom-chat",
};
/** Never opened, so it has no chat. */
const scout: Mono = {
  id: "mono-3",
  name: "Scout",
  mascot: "crab",
  color: "#ccc",
  projects: [],
};

/**
 * Captain Jack, Shroom (busy, one message queued) and Scout. The caller is
 * Captain Jack's own chat, one of its habit runs, or a plain project session.
 */
function fixture(kind: "mono" | "habit" | "session" = "mono") {
  const source = newSession("codex", "/Users/me", "codex:test");
  source.id = "caller";
  // Shroom's chat: two exchanges, the newest last.
  const blocks: Block[] = [
    { id: "u1", role: "user", text: "Is CI green?" },
    { id: "a1", role: "assistant", text: "Yes, every check passes." },
    { id: "u2", role: "user", text: "Ship it?" },
    { id: "a2", role: "assistant", text: "Shipped v2.1." },
  ];
  const shroomChat = {
    ...newSession("claude", "/code/app", "claude:opus"),
    id: "shroom-chat",
    busy: true,
    queuedMessages: [{ id: "q1", text: "Next", attachments: [] }],
    blocks,
  };
  const entries = [
    { mono: captain },
    { mono: shroom, chat: shroomChat, updatedAt: 1_760_000_000_000 },
    { mono: scout },
  ];
  const monos: NonNullable<AgentAppHost["monos"]> = {
    list: vi.fn(async () => entries),
    get: vi.fn(async (monoId: string) =>
      entries.find((entry) => entry.mono.id === monoId),
    ),
    read: vi.fn(async (monoId: string, options: SessionReadOptions) => {
      if (monoId !== shroom.id)
        throw new Error("Unknown Mono; run monos.list for IDs");
      return sessionConversationPage(shroomChat, options);
    }),
    send: vi.fn(async () => ({
      alreadySubmitted: false,
      delivery: "sent" as const,
    })),
  } as NonNullable<AgentAppHost["monos"]>;
  const host = {
    isMono: (id: string) => kind === "mono" && id === source.id,
    isHabitRun: (id: string) => kind === "habit" && id === source.id,
    monoOf: (id: string) =>
      kind !== "session" && id === source.id
        ? { id: captain.id, name: captain.name, projects: [] }
        : undefined,
    monos,
  } as unknown as AgentAppHost;
  const run = (
    action: string,
    input: Record<string, unknown> = {},
    requestId = "req-1",
  ) => handleAgentApp(source, requestId, action, input, host);
  return { run, send: vi.mocked(monos.send) };
}

/** What the CLI prints for a call that fails. */
async function failure(call: Promise<unknown>) {
  return appErrorResponse(
    await call.then(
      () => undefined,
      (error) => error,
    ),
  );
}

it.each([
  ["monos.list", {}],
  ["monos.get", { monoId: "mono-2" }],
  ["monos.read", { monoId: "mono-2" }],
  ["monos.send", { monoId: "mono-2", prompt: "Review PR 12" }],
])("refuses %s from a session that is not a Mono's", async (action, input) => {
  const { run, send } = fixture("session");
  expect(await failure(run(action, input))).toEqual({
    ok: false,
    code: "not_a_mono",
    hint: "Only a Mono can use monos.* actions",
    error: "Only a Mono can use monos.* actions",
  });
  expect(send).not.toHaveBeenCalled();
});

const captainListing = {
  id: "mono-1",
  name: "Captain Jack",
  role: "giga",
  projects: [],
  busy: false,
  queued: 0,
  hasConversation: true,
  self: true,
};
const shroomListing = {
  id: "mono-2",
  name: "MonoShroom",
  role: "mono",
  projects: ["/code/app"],
  busy: true,
  queued: 1,
  hasConversation: true,
  updatedAt: 1_760_000_000_000,
  self: false,
};

it("lists every Mono with its role to a Mono with no projects and marks only itself", async () => {
  const { run } = fixture();
  expect(await run("monos.list")).toEqual({
    monos: [
      captainListing,
      shroomListing,
      {
        id: "mono-3",
        name: "Scout",
        role: "mono",
        projects: [],
        busy: false,
        queued: 0,
        hasConversation: false,
        self: false,
      },
    ],
  });
});

it("gets one Mono as monos.list shows it, with the harness and model its chat runs on", async () => {
  const { run } = fixture("habit");
  expect(await run("monos.get", { monoId: "mono-2" })).toEqual({
    ...shroomListing,
    harness: "claude",
    model: "claude:opus",
  });
  // Defect: a Mono with no chat loaded reports another Mono's model.
  expect(await run("monos.get", { monoId: "mono-3" })).toMatchObject({
    id: "mono-3",
    hasConversation: false,
    harness: null,
    model: null,
  });
});

it("answers monos.get for an unknown Mono with unknown_mono", async () => {
  const { run } = fixture();
  expect(await failure(run("monos.get", { monoId: "mono-9" }))).toEqual({
    ok: false,
    code: "unknown_mono",
    hint: "Unknown Mono; run monos.list for IDs",
    error: "Unknown Mono; run monos.list for IDs",
  });
  expect(await failure(run("monos.get", {}))).toMatchObject({
    code: "invalid_input",
  });
});

it("names monos.update in place of monos.configure, and never offers monos.delete", async () => {
  const { run } = fixture();
  expect(
    await failure(run("monos.configure", { monoId: "mono-2", name: "X" })),
  ).toEqual({
    ok: false,
    code: "action_removed",
    hint: "Use monos.update.",
    error: "Use monos.update.",
  });
  expect(
    await failure(run("monos.delete", { monoId: "mono-2" })),
  ).toMatchObject({ code: "unknown_action" });
});

it("reads another Mono's chat a page at a time", async () => {
  const { run } = fixture();
  expect(await run("monos.read", { monoId: "mono-2", limit: 1 })).toMatchObject(
    {
      sessionId: "shroom-chat",
      turns: [
        {
          turnId: "u2",
          user: { text: "Ship it?" },
          assistant: { text: "Shipped v2.1." },
        },
      ],
      nextBefore: "u2",
    },
  );
  expect(
    await run("monos.read", { monoId: "mono-2", before: "u2" }),
  ).toMatchObject({
    turns: [{ turnId: "u1", user: { text: "Is CI green?" } }],
    nextBefore: null,
  });
});

it("answers monos.read with a before that is not a turn ID with invalid_input", async () => {
  const { run } = fixture();
  expect(
    await failure(run("monos.read", { monoId: "mono-2", before: "a1" })),
  ).toMatchObject({
    code: "invalid_input",
    hint: "before is not a turn ID in this session",
  });
});

it("answers a reused request ID with different input with request_id_reused", async () => {
  const prior = { signature: "a", promise: Promise.resolve("first") };
  expect(repeatedAppCall(undefined, "a")).toBeUndefined();
  expect(await repeatedAppCall(prior, "a")).toBe("first");
  expect(
    await failure(Promise.resolve().then(() => repeatedAppCall(prior, "b"))),
  ).toEqual({
    ok: false,
    code: "request_id_reused",
    hint: "This request id was already used with different input. Use a new request id.",
    error:
      "This request id was already used with different input. Use a new request id.",
  });
});

it("sends a Mono back to its own chat instead of messaging itself", async () => {
  const { run, send } = fixture();
  await expect(
    run("monos.send", { monoId: "mono-1", prompt: "Remind me later" }),
  ).rejects.toThrow("Use your own conversation to continue it");
  expect(send).not.toHaveBeenCalled();
});

it.each([
  [
    { monoId: "mono-2", prompt: "Review PR 12", whenBusy: "later" },
    "req-1",
    'whenBusy must be "steer" or "followUp"',
  ],
  [
    { monoId: "mono-2", prompt: "Review PR 12", project: "/code/app" },
    "req-1",
    "Unknown monos.send fields: project",
  ],
  [
    { monoId: "mono-2", prompt: "Review PR 12" },
    "bad/id",
    "Invalid request ID",
  ],
])("rejects an invalid monos.send (%#)", async (input, requestId, message) => {
  const { run, send } = fixture();
  await expect(run("monos.send", input, requestId)).rejects.toThrow(message);
  expect(send).not.toHaveBeenCalled();
});

it("delivers the prompt marked as coming from the sending Mono", async () => {
  const { run, send } = fixture();
  send.mockResolvedValueOnce({
    alreadySubmitted: false,
    delivery: "queued",
    queuedMessageId: "q1",
  });
  expect(
    await run(
      "monos.send",
      { monoId: "mono-2", prompt: "Review PR 12", whenBusy: "followUp" },
      "ask-1",
    ),
  ).toEqual({
    monoId: "mono-2",
    submitted: true,
    alreadySubmitted: false,
    delivery: "queued",
    queuedMessageId: "q1",
  });
  expect(send).toHaveBeenCalledWith(
    "mono-2",
    expect.any(String),
    "app-caller-ask-1",
    { whenBusy: "followUp" },
  );
  // The rules for answering live once in <mono_messaging_rules>, not in
  // every message.
  expect(send.mock.calls[0][1]).toBe(
    '<mono_message from="Captain Jack" monoId="mono-1">\nReview PR 12\n</mono_message>',
  );
});

it("lets a habit run message another Mono on behalf of its Mono", async () => {
  const { run, send } = fixture("habit");
  expect(
    await run(
      "monos.send",
      { monoId: "mono-2", prompt: "CI is red on main" },
      "habit-1",
    ),
  ).toEqual({
    monoId: "mono-2",
    submitted: true,
    alreadySubmitted: false,
    delivery: "sent",
  });
  expect(send).toHaveBeenCalledWith(
    "mono-2",
    expect.stringContaining(
      '<mono_message from="Captain Jack" monoId="mono-1">',
    ),
    "app-caller-habit-1",
    {},
  );
});
