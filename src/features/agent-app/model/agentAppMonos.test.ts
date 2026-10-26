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
    { id: "r2", role: "reasoning", text: "Tag first, then publish." },
    {
      id: "t2",
      role: "tool",
      text: "npm publish",
      tool: { kind: "shell", status: "completed", detail: "+ app@2.1.0" },
    },
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
  } as NonNullable<AgentAppHost["monos"]>;
  const messages: NonNullable<AgentAppHost["messages"]> = {
    getMonos: vi.fn(async () => ({ monos: [] })),
    send: vi.fn(async () => ({ message_id: "m_aaaaaaaaaaaa" })),
    ask: vi.fn(async () => ({ message_id: "m_aaaaaaaaaaaa" })),
    ack: vi.fn(async () => ({ message_id: "m_bbbbbbbbbbbb" })),
    question: vi.fn(async () => ({ message_id: "m_cccccccccccc" })),
    answer: vi.fn(async () => ({ message_id: "m_dddddddddddd" })),
    update: vi.fn(async () => ({ message_id: "m_eeeeeeeeeeee" })),
    reply: vi.fn(async () => ({ message_ids: [] })),
    get: vi.fn(async () => ({ message: {} })),
  };
  const host = {
    levels: {
      check: async () => ({ leadSessionId: null }),
      recordStart: async () => "m_aaaaaaaaaaaa",
    },
    isMono: (id: string) => kind === "mono" && id === source.id,
    isHabitRun: (id: string) => kind === "habit" && id === source.id,
    monoOf: (id: string) =>
      kind !== "session" && id === source.id
        ? { id: captain.id, name: captain.name, projects: [] }
        : undefined,
    monos,
    messages,
  } as unknown as AgentAppHost;
  const run = (
    action: string,
    input: Record<string, unknown> = {},
    requestId = "req-1",
  ) => handleAgentApp(source, requestId, action, input, host);
  return {
    run,
    send: vi.mocked(messages.send),
    get: vi.mocked(messages.get),
    ask: vi.mocked(messages.ask),
    ack: vi.mocked(messages.ack),
    talk: {
      question: vi.mocked(messages.question),
      answer: vi.mocked(messages.answer),
      update: vi.mocked(messages.update),
    },
    shroomChat,
  };
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
])("refuses %s from a session that is not a Mono's", async (action, input) => {
  const { run } = fixture("session");
  expect(await failure(run(action, input))).toEqual({
    ok: false,
    code: "not_a_mono",
    hint: "Only a Mono can use monos.* actions",
    error: "Only a Mono can use monos.* actions",
  });
});

const captainListing = {
  id: "mono-1",
  name: "Captain Jack",
  role: "giga",
  projects: [],
  busy: false,
  queued: 0,
  queuePaused: false,
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
  queuePaused: false,
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
        queuePaused: false,
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
    messageAllowlist: [],
  });
  // Defect: a Mono with no chat loaded reports another Mono's model.
  expect(await run("monos.get", { monoId: "mono-3" })).toMatchObject({
    id: "mono-3",
    hasConversation: false,
    harness: null,
    model: null,
  });
});

// ADE-78. Defect: after a relaunch paused a Mono's queue, monos.get showed
// an idle Mono with queued messages, and nothing said they wait for Resume.
it("reports which Mono's queue is paused in monos.get and monos.list", async () => {
  const { run, shroomChat } = fixture();
  shroomChat.busy = false;
  shroomChat.queueStatus = "paused";
  expect(await run("monos.get", { monoId: "mono-2" })).toMatchObject({
    busy: false,
    queued: 1,
    queuePaused: true,
  });
  expect(await run("monos.list")).toMatchObject({
    monos: [
      { id: "mono-1", queuePaused: false },
      { id: "mono-2", queuePaused: true },
      { id: "mono-3", queuePaused: false },
    ],
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

it("adds the thinking and tool rows to monos.read only with work: true", async () => {
  const { run } = fixture();
  const prose = await run("monos.read", { monoId: "mono-2", limit: 1 });
  expect(JSON.stringify(prose)).not.toMatch(/Tag first|npm publish|"work"/);
  expect(
    await run("monos.read", { monoId: "mono-2", limit: 1, work: true }),
  ).toMatchObject({
    turns: [
      {
        turnId: "u2",
        assistant: { text: "Shipped v2.1." },
        work: [
          { kind: "reasoning", text: "Tag first, then publish." },
          {
            kind: "tool",
            tool: "shell",
            status: "completed",
            input: { text: "npm publish" },
            result: { text: "+ app@2.1.0" },
          },
        ],
      },
    ],
  });
  expect(
    await failure(run("monos.read", { monoId: "mono-2", work: 1 })),
  ).toMatchObject({ code: "invalid_input", hint: "work must be true or false" });
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

// Defect: the removed action still delivers, or answers unknown_action
// without the name of the action that replaced it.
it("answers monos.send with action_removed and the name of messages.send", async () => {
  const { run, send } = fixture();
  expect(
    await failure(
      run("monos.send", { monoId: "mono-2", prompt: "Review PR 12" }),
    ),
  ).toEqual({
    ok: false,
    code: "action_removed",
    hint: "Use messages.send (MCP send_message).",
    error: "Use messages.send (MCP send_message).",
  });
  expect(send).not.toHaveBeenCalled();
});

// Defect: a send without wanting_reply reaches the gate, or gets a default.
it.each([
  [{ message: "Review PR 12", send_to: "mono-2" }, "wanting_reply"],
  [
    { message: "Review PR 12", send_to: "mono-2", wanting_reply: "yes" },
    "wanting_reply",
  ],
  [
    { message: "Hi", send_to: "mono-2", wanting_reply: false, from: "x" },
    "from",
  ],
  [
    {
      message: "Hi",
      send_to: "mono-2",
      wanting_reply: false,
      action_type: "now",
    },
    "action_type",
  ],
  // PR I2. Defect: the removed field is ignored and the call silently
  // changes meaning; the hint must name its replacement.
  [
    {
      message: "Hi",
      send_to: "mono-2",
      wanting_reply: false,
      intent: "new",
      action_type: "queue",
    },
    "use intent",
  ],
  [{ message: "Hi", send_to: "mono-2", wanting_reply: false }, "intent"],
])(
  "answers an invalid messages.send with invalid_input (%#)",
  async (input, named) => {
    const { run, send } = fixture();
    const answer = await failure(run("messages.send", input));
    expect(answer).toMatchObject({ ok: false, code: "invalid_input" });
    expect("hint" in answer && answer.hint).toContain(named);
    expect(send).not.toHaveBeenCalled();
  },
);

// Design 16.12.1.4 (whole-turn). Defect: the app boundary refuses scope as
// an unknown field, or drops it, so Rust sees a steer with no target.
it("passes a whole-turn steer's scope to messages.send", async () => {
  const { run, send } = fixture();
  await run("messages.send", {
    message: "Stop and commit",
    send_to: "mono-2",
    wanting_reply: false,
    intent: "steer",
    scope: "turn",
  });
  expect(send).toHaveBeenCalledWith(expect.anything(), {
    message: "Stop and commit",
    send_to: "mono-2",
    wanting_reply: false,
    intent: "steer",
    scope: "turn",
  });
});

// Design 16.4. Defect: a get_message call with another field, or with a
// value that is not one reference, reaches the store instead of
// invalid_input; or a valid one never reaches it.
it.each([
  [{ id: "m_aaaaaaaaaaaa" }, "Unknown messages.get fields: id"],
  [{ handle: ["#1"] }, 'one handle such as "#1"'],
  [{ handle: "1" }, 'one handle such as "#1"'],
  [{}, 'one handle such as "#1"'],
])(
  "answers an invalid messages.get with invalid_input (%#)",
  async (input, named) => {
    const { run, get } = fixture();
    const answer = await failure(run("messages.get", input));
    expect(answer).toMatchObject({ ok: false, code: "invalid_input" });
    expect("hint" in answer && answer.hint).toContain(named);
    expect(get).not.toHaveBeenCalled();
  },
);

it("passes messages.get with a handle or a message id to the store", async () => {
  const { run, get } = fixture();
  await run("messages.get", { handle: "#2" });
  await run("messages.get", { handle: "m_aaaaaaaaaaaa" });
  expect(get.mock.calls).toEqual([
    [{ handle: "#2" }],
    [{ handle: "m_aaaaaaaaaaaa" }],
  ]);
});

// R6, R14. Exists only for gate/mutation (R6 one-off); app.sh r6 proves ask
// and ack end to end, the S4 scenario the rest in Rust. Defect: a field of a
// call on an ask's key is refused as unknown, or one action reaches another
// action's host call or none.
it("passes each call on an ask's key with every field to its own call", async () => {
  const { run, send, ask, ack, talk } = fixture();
  const asked = { message: "Run it", send_to: "mono-2", due_minutes: 30 };
  const acked = { ask_id: "m_aaaaaaaaaaaa", message: "Done" };
  const questioned = { ask_id: "m_aaaaaaaaaaaa", message: "Which branch?" };
  const answered = { ask_id: "m_aaaaaaaaaaaa", seq: 1, message: "main" };
  const updated = { ask_id: "m_aaaaaaaaaaaa", message: "Still working" };
  await run("messages.ask", asked, "req-ask");
  await run("messages.ack", acked, "req-ack");
  await run("messages.question", questioned, "req-q");
  await run("messages.answer", answered, "req-a");
  await run("messages.update", updated, "req-u");
  expect(ask.mock.calls).toEqual([["req-ask", asked]]);
  expect(ack.mock.calls).toEqual([["req-ack", acked]]);
  expect(talk.question.mock.calls).toEqual([["req-q", questioned]]);
  expect(talk.answer.mock.calls).toEqual([["req-a", answered]]);
  expect(talk.update.mock.calls).toEqual([["req-u", updated]]);
  expect(send).not.toHaveBeenCalled();
});
