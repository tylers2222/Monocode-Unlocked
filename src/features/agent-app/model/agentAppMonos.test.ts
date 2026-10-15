// @vitest-environment happy-dom
import { expect, it, vi } from "vitest";
import { newSession, type Block } from "../../sessions/model/session";
import {
  sessionConversationPage,
  type SessionReadOptions,
} from "./sessionConversation";
import { handleAgentApp, type AgentAppHost } from "./agentApp";

const captain = {
  id: "mono-1",
  name: "Captain Jack",
  projects: [],
  busy: false,
  queued: 0,
  hasConversation: true,
};
const shroom = {
  id: "mono-2",
  name: "Shroom",
  projects: ["/code/app"],
  busy: true,
  queued: 1,
  hasConversation: true,
  updatedAt: 1_760_000_000_000,
};

/**
 * Captain Jack, who has no projects, and Shroom. The caller is Captain Jack's
 * own chat, one of its habit runs, or a plain project session.
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
    ...newSession("codex", "/code/app"),
    id: "shroom-chat",
    blocks,
  };
  const monos: NonNullable<AgentAppHost["monos"]> = {
    list: vi.fn(async () => [captain, shroom]),
    read: vi.fn(async (monoId: string, options: SessionReadOptions) => {
      if (monoId !== shroom.id)
        throw new Error("Unknown Mono; run monos.list for IDs");
      return sessionConversationPage(shroomChat, options);
    }),
    send: vi.fn(async () => ({
      alreadySubmitted: false,
      delivery: "sent" as const,
    })),
  };
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

it.each([
  ["monos.list", {}],
  ["monos.read", { monoId: "mono-2" }],
  ["monos.send", { monoId: "mono-2", prompt: "Review PR 12" }],
])("refuses %s from a session that is not a Mono's", async (action, input) => {
  const { run, send } = fixture("session");
  await expect(run(action, input)).rejects.toThrow(
    "Only a Mono can use monos.* actions",
  );
  expect(send).not.toHaveBeenCalled();
});

it("lists every Mono to a Mono with no projects and marks only itself", async () => {
  const { run } = fixture();
  expect(await run("monos.list")).toEqual({
    monos: [
      { ...captain, self: true },
      { ...shroom, self: false },
    ],
  });
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
