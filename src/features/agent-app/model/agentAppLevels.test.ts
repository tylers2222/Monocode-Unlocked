// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import { __monosTestReset, loadMonoRoster } from "../../monos/model/mono";
import { levelGate, type ActionChecked } from "../../monos/model/monoCaller";
import { setHarnessModels } from "../../sessions/model/models";
import { newSession, type Session } from "../../sessions/model/session";
import fixture from "./appActionCheck.fixtures.json";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { AppActionError } from "./appActionError";

// The calls and replies of `fixture` are the contract with Rust: this file
// checks that the real `levelGate` sends exactly `args` and acts on `reply`,
// and mono_messages.rs replays every `args` through the real
// `mono_app_action_check` and checks that it answers `reply`.

const command = vi.hoisted(() => ({
  answer: undefined as ((args: { requestId: string }) => unknown) | undefined,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (name: string, args: { requestId: string }) => {
    if (name === "monos_list")
      return {
        revision: 1,
        monos: fixture.monos.map((mono) => ({
          id: mono.id,
          role: "mono",
          data: {
            sessionId: mono.sessionId,
            mascot: "crab",
            color: "red",
            projects: mono.projects,
          },
        })),
      };
    if (name === "mono_app_action_check") return command.answer!(args);
    throw new Error(`unexpected ${name}`);
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex",
}));

const PROJECT = "/code/app";
const ALLOWED = { outcome: "allowed", leadSessionId: null };

beforeEach(async () => {
  vi.mocked(invoke).mockClear();
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
  });
  setHarnessModels("codex", [
    { id: "codex:test", harness: "codex", name: "Test model" },
  ]);
  __monosTestReset();
  await loadMonoRoster();
  command.answer = (args) =>
    fixture.cases.find((entry) => entry.args.requestId === args.requestId)
      ?.reply ?? ALLOWED;
});

/** The fixture's sessions as this window has them; lead, worker and ghost have /operator access. */
function world(caller: string) {
  const sessions = new Map<string, Session>(
    fixture.sessions.map((entry) => [
      entry.id,
      {
        ...newSession("codex", entry.cwd, "codex:test"),
        id: entry.id,
        ...(entry.leadSessionId ? { leadSessionId: entry.leadSessionId } : {}),
        blocks:
          entry.id === "a-chat"
            ? []
            : [{ id: "u1", role: "user", text: "Fix CI", monocode: true }],
      },
    ]),
  );
  const source = sessions.get(caller)!;
  const listing = (session: Session) => ({
    id: session.id,
    title: session.title,
    harness: session.harness,
    model: session.model,
    busy: false,
    hasDraft: false,
    archived: false,
  });
  const actions = {
    start: vi.fn(async () => {}),
    send: vi.fn(async () => ({ alreadySubmitted: false, delivery: "sent" })),
    draft: vi.fn(async () => ({ alreadySaved: false, draft: true })),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    postCard: vi.fn(),
    notes: vi.fn(async () => []),
    saveNote: vi.fn(async () => ({})),
    createWorktree: vi.fn(async () => ({})),
    messageSend: vi.fn(async () => ({ message_id: "m_bbbbbbbbbbbb" })),
  };
  const host = {
    ...actions,
    sessions: vi.fn(async (cwd: string) =>
      [...sessions.values()]
        .filter((session) => session.cwd === cwd)
        .map(listing),
    ),
    session: vi.fn(async (id: string) => sessions.get(id) ?? null),
    worktrees: vi.fn(async () => ({ worktrees: [], defaultRoot: "/tmp" })),
    isMono: (id: string) => id === "a-chat",
    isHabitRun: () => false,
    monoOf: (id: string) =>
      id === "a-chat" ? { id: "mono-a", projects: [PROJECT] } : undefined,
    messages: {
      getMonos: async () => ({ monos: [] }),
      send: actions.messageSend,
      reply: async () => ({}),
      get: async () => ({}),
    },
    levels: levelGate(source, "turn-1", (id) => sessions.get(id)?.leadSessionId),
  } as unknown as AgentAppHost;
  const run = (action: string, input: Record<string, unknown>, requestId = "r1") =>
    handleAgentApp(source, requestId, action, input, host);
  return { run, host, actions, sessions };
}

const checks = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === "mono_app_action_check")
    .map(([, args]) => args);

// Design 6.6 and 13.3, the Rust contract. Defect: an app action runs with no
// level check, or with a check that names another action, target, lead copy
// or input than Rust reads; Rust's refusal does not stop the action or loses
// its code, hint or row id; or the web app decides the new session's lead
// itself instead of taking Rust's answer.
it.each(fixture.cases.map((entry) => [entry.name, entry] as const))(
  "%s: sends the fixture's check and acts on its reply",
  async (_name, entry) => {
    const { run, host, actions } = world(entry.caller);
    const reply = entry.reply as ActionChecked;
    const answer = run(entry.action, entry.input, entry.args.requestId);
    if (reply.outcome === "failed") {
      await expect(answer).rejects.toMatchObject({
        code: reply.error.code,
        hint: reply.error.hint,
        details: { message_id: reply.error.message_id },
      });
      for (const action of Object.values(actions))
        expect(action).not.toHaveBeenCalled();
    } else {
      await answer;
      if (entry.ran)
        expect(actions[entry.ran as keyof typeof actions]).toHaveBeenCalledOnce();
    }
    expect(checks()).toEqual([entry.args]);
    if (entry.action === "sessions.start" && reply.outcome === "allowed")
      expect(vi.mocked(host.start).mock.calls[0][0].leadSessionId).toBe(
        reply.leadSessionId ?? undefined,
      );
  },
);

// PR 7 review r1 B1. Defect: a blank id, an id with a space or an id over 256
// characters reaches the level check as no target or as another reading.
it.each([["  "], ["work er"], [` ${"w".repeat(257)}`], [42]])(
  "refuses sessionId %j as invalid_input before the level check",
  async (sessionId) => {
    const { run, actions } = world("a-chat");
    await expect(
      run("sessions.send", { sessionId, prompt: "Hi" }),
    ).rejects.toMatchObject({ code: "invalid_input" });
    expect(checks()).toEqual([]);
    expect(actions.send).not.toHaveBeenCalled();
  },
);

// PR 7 review r1 B1. Defect: the check reads the parsed id while the action
// reads the raw input, so a padded id whose trimmed form is 256 characters
// passes the check and the action refuses it or acts on another reading.
it("sends to a padded id whose trimmed form is 256 characters", async () => {
  const { run, actions, sessions } = world("a-chat");
  const id = "w".repeat(256);
  sessions.set(id, { ...newSession("codex", PROJECT, "codex:test"), id });
  expect(await run("sessions.send", { sessionId: ` ${id}`, prompt: "Hi" })).toMatchObject(
    { sessionId: id, submitted: true },
  );
  expect(checks()).toMatchObject([{ target: id, input: { sessionId: id } }]);
  expect(vi.mocked(actions.send).mock.calls[0]?.[0]).toBe(id);
});

// PR 7 review r2 m10. Defect: every chat.card must name a session, so a pr,
// choices or habit card is refused; or a session card's id is not the check's
// target.
it("posts a Mono's cards with and without a sessionId", async () => {
  const { run, actions } = world("a-chat");
  const cards = [
    { type: "pr", repo: "owner/repo", number: 123 },
    { type: "choices", options: ["Review", "Ship"] },
    {
      type: "habit",
      name: "Check CI",
      instructions: "Check CI",
      schedule: { kind: "daily", time: "09:00" },
    },
    { type: "session", sessionId: " worker", note: "See this" },
  ];
  for (const card of cards)
    expect(await run("chat.card", card), card.type).toMatchObject({
      posted: card.type,
    });
  expect(actions.postCard).toHaveBeenCalledTimes(cards.length);
  expect(checks().map((args) => (args as { target: unknown }).target)).toEqual([
    null,
    null,
    null,
    "worker",
  ]);
});

// Design 13.5. Defect: a prompt with U+0000 reaches Rust, whose row of it
// fails the length CHECK, so the refusal is an SQLite error.
it("refuses a prompt with U+0000 as invalid_input before the level check", async () => {
  const { run, actions } = world("worker");
  await expect(
    run("sessions.start", {
      harness: "codex",
      model: "codex:test",
      prompt: "Fix\u0000it",
    }),
  ).rejects.toMatchObject({ code: "invalid_input" });
  expect(checks()).toEqual([]);
  expect(actions.start).not.toHaveBeenCalled();
});

// PR7-9a review r1 point 4. Defect: when mono_app_action_check fails (a
// retried refusal hits the request id's UNIQUE row until PR 11), the action
// runs anyway.
it("runs nothing when the level check itself fails", async () => {
  const { run, actions } = world("lead");
  command.answer = () => {
    throw "UNIQUE constraint failed: mono_messages.from_session_id";
  };
  await expect(
    run("sessions.start", {
      harness: "codex",
      model: "codex:test",
      prompt: "Fix the flaky test",
    }),
  ).rejects.toBe("UNIQUE constraint failed: mono_messages.from_session_id");
  expect(actions.start).not.toHaveBeenCalled();
});

// #81 review n5. Defect: messages.* go to the app-action gate too, so a
// worker's message to its lead gets worker_scope and an app_action row
// instead of its own gate in mono_message_submit.
it("sends messages.* to their own gate, never to the level check", async () => {
  const { run, actions } = world("worker");
  await run("messages.send", {
    message: "Done",
    send_to: "lead",
    wanting_reply: false,
    intent: "new",
  });
  expect(actions.messageSend).toHaveBeenCalledOnce();
  expect(checks()).toEqual([]);
});

// Design 13.4. Defect: a session with no app access reaches Rust as a caller.
it("refuses a session with no app access with turn_inactive and no check", async () => {
  const { run, sessions } = world("lead");
  sessions.get("lead")!.blocks = [];
  const refused = await run("models.list", {}).catch((error: unknown) => error);
  expect(refused).toBeInstanceOf(AppActionError);
  expect(refused).toMatchObject({ code: "turn_inactive" });
  expect(checks()).toEqual([]);
});
