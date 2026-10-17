// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { QuickLaunch } from "../../quick-composer/model/quickComposer";
import {
  resetHarnessModelOverlays,
  setHarnessModels,
} from "../../sessions/model/models";
import {
  newSession,
  type Block,
  type RuntimeMode,
  type Session,
} from "../../sessions/model/session";
import type { Note } from "../../notes";
import {
  parseMessageId,
  type RejectCode,
} from "../../monos/model/monoMessages";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { AppActionError, appErrorResponse } from "./appActionError";

vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex",
}));

const storedValues = new Map<string, string>();
vi.stubGlobal("localStorage", {
  clear: () => storedValues.clear(),
  getItem: (key: string) => storedValues.get(key) ?? null,
  setItem: (key: string, value: string) => storedValues.set(key, value),
  removeItem: (key: string) => storedValues.delete(key),
});

beforeEach(() => {
  localStorage.clear();
  setHarnessModels("codex", [
    { id: "codex:test", harness: "codex", name: "Test model" },
  ]);
});

afterEach(() => {
  resetHarnessModelOverlays();
});

const PROJECT = "/code/app";
const CHOICE = { harness: "codex", model: "codex:test" } as const;
const REUSED =
  "This request id was already used with different input. Use a new request id.";

const note: Note = {
  id: "note-1",
  slug: "release-checklist",
  title: "Release checklist",
  body: "Ship on Friday.",
  tags: [],
  slugPending: false,
  createdAt: 1,
  updatedAt: 1,
};

/** A `mono_messages` row, as far as the level rules read it. */
type Row = {
  id: string;
  tool: "send_message" | "start_session" | "app_action";
  status: "delivered" | "rejected";
  fromSessionId: string;
  requestId: string;
  /** Stands in for the sha256 of `[action, input]`. */
  hash: string;
  toSessionId: string | null;
  gateResult?: RejectCode;
};

/**
 * A Mono's chat, the lead it started in its project, and that lead's
 * worker. The caller is one of them. The host keeps the rows that Rust
 * keeps: the level check (design 6.6 A1-A7, without A8), the start prior
 * and the start row, and the sends of messages.send.
 */
function world(
  caller: "mono" | "lead" | "worker",
  { runtimeMode = "supervised" }: { runtimeMode?: RuntimeMode } = {},
) {
  const stamped = (startedBy: string): Block[] => [
    { id: "first", role: "user", text: "Start", startedBy },
  ];
  const at = (id: string, cwd: string, extra: Partial<Session> = {}) => ({
    ...newSession("codex", cwd, "codex:test", runtimeMode),
    id,
    ...extra,
  });
  const sessions = new Map<string, Session>([
    ["mono-chat", at("mono-chat", "/Users/me")],
    ["lead", at("lead", PROJECT, { blocks: stamped("mono-chat") })],
    [
      "worker",
      at("worker", PROJECT, { leadSessionId: "lead", blocks: stamped("lead") }),
    ],
  ]);
  const source = sessions.get(caller === "mono" ? "mono-chat" : caller)!;
  const mono = caller === "mono";
  const rows: Row[] = [];
  const inbox = new Map<string, string[]>();
  const hash = (action: string, input: unknown) =>
    JSON.stringify([action, input]);
  const keyed = (requestId: string) =>
    rows.find(
      (row) => row.fromSessionId === source.id && row.requestId === requestId,
    );
  const write = (row: Omit<Row, "id" | "fromSessionId">) => {
    const written = {
      ...row,
      id: `m_${String.fromCharCode(97 + rows.length).repeat(12)}`,
      fromSessionId: source.id,
    };
    rows.push(written);
    return written;
  };
  const levels: NonNullable<AgentAppHost["levels"]> = {
    check: vi.fn(async (requestId, { action, target, input }) => {
      const asked = hash(action, input);
      const prior = keyed(requestId);
      if (prior) {
        if (prior.hash !== asked)
          throw new AppActionError("request_id_reused", REUSED);
        if (prior.gateResult)
          throw new AppActionError(prior.gateResult, `${prior.gateResult} hint`, {
            message_id: parseMessageId(prior.id),
            duplicate: true,
          });
        return { leadSessionId: mono ? null : prior.fromSessionId };
      }
      const reject = (code: RejectCode, tool: Row["tool"]): never => {
        const row = write({
          tool,
          status: "rejected",
          requestId,
          hash: asked,
          toSessionId: tool === "app_action" ? target : null,
          gateResult: code,
        });
        throw new AppActionError(code, `${code} hint`, {
          message_id: parseMessageId(row.id),
        });
      };
      const level = mono ? undefined : (source.leadSessionId ?? null);
      if (typeof level === "string")
        reject(
          action === "sessions.start" ? "worker_cannot_spawn" : "worker_scope",
          action === "sessions.start" ? "start_session" : "app_action",
        );
      const targetLead = target ? sessions.get(target)?.leadSessionId : undefined;
      if (
        (action === "sessions.send" || action === "sessions.draft") &&
        targetLead &&
        targetLead !== source.id
      )
        reject("steer_via_lead", "app_action");
      return {
        leadSessionId:
          action === "sessions.start" && level === null ? source.id : null,
      };
    }),
    startPrior: vi.fn(async (requestId, input) => {
      const prior = keyed(requestId);
      if (!prior) return "none" as const;
      return prior.hash === hash("sessions.start", input)
        ? { same: prior.id }
        : ("reused" as const);
    }),
    recordStart: vi.fn(async (requestId, sessionId, input) => {
      const asked = hash("sessions.start", input);
      const row =
        keyed(requestId) ??
        write({
          tool: "start_session",
          status: "delivered",
          requestId,
          hash: asked,
          toSessionId: sessionId,
        });
      if (row.hash !== asked)
        throw new AppActionError("request_id_reused", REUSED);
      return row.id;
    }),
  };
  const host = {
    // As App.tsx: a repeated id launches nothing and answers no error,
    // unless its text differs or it submits what was a draft.
    start: vi.fn(async (launch: QuickLaunch, id: string) => {
      const existing = sessions.get(id);
      if (existing) {
        const first = existing.blocks[0];
        if (first.text !== launch.prompt || (!launch.draft && first.draft))
          throw new Error("Request ID was already used for another session launch");
        return;
      }
      sessions.set(id, {
        ...newSession(launch.harness, launch.cwd, launch.model, launch.runtimeMode),
        id,
        ...(launch.leadSessionId ? { leadSessionId: launch.leadSessionId } : {}),
        blocks: [
          {
            id: "first",
            role: "user",
            text: launch.prompt,
            ...(launch.draft ? { draft: true } : {}),
            ...(launch.startedBy ? { startedBy: launch.startedBy } : {}),
          },
        ],
      });
    }),
    sessions: vi.fn(async (cwd: string) =>
      [...sessions.values()]
        .filter((session) => session.cwd === cwd)
        .map((session) => ({
          id: session.id,
          title: session.title,
          harness: session.harness,
          model: session.model,
          busy: false,
          hasDraft: false,
          archived: false,
        })),
    ),
    session: vi.fn(async (id: string) => sessions.get(id) ?? null),
    send: vi.fn(async (id: string, prompt: string) => {
      inbox.set(id, [...(inbox.get(id) ?? []), prompt]);
      return { alreadySubmitted: false, delivery: "steered" as const };
    }),
    notes: vi.fn(async () => [note]),
    isMono: (id: string) => id === "mono-chat",
    isHabitRun: () => false,
    monoOf: (id: string) =>
      id === "mono-chat" ? { id: "mono-1", projects: [PROJECT] } : undefined,
    messages: {
      getMonos: async () => ({ monos: [] }),
      send: async (requestId: string, input: { send_to: string }) => {
        const asked = hash("messages.send", input);
        const prior = keyed(requestId);
        if (prior && prior.hash !== asked)
          throw new AppActionError("request_id_reused", REUSED);
        const row =
          prior ??
          write({
            tool: "send_message",
            status: "delivered",
            requestId,
            hash: asked,
            toSessionId: input.send_to,
          });
        return { message_id: row.id };
      },
      reply: async () => ({ message_ids: [] }),
      get: async () => ({}),
    },
    levels,
  } as unknown as AgentAppHost;
  const run = (
    action: string,
    input: Record<string, unknown>,
    requestId = "r1",
  ) => handleAgentApp(source, requestId, action, input, host);
  return { run, host, levels, rows, sessions, inbox };
}

/** What the CLI prints for a call that fails. */
async function failure(call: Promise<unknown>) {
  return appErrorResponse(
    await call.then(
      () => undefined,
      (error: unknown) => error,
    ),
  );
}

// Design 13.3. Defect: a lead's start makes a session with no lead id, so
// the worker saves as a lead and may start sessions and message Monos.
it("starts a worker of a lead with the lead's id and its start row", async () => {
  const { run, host, rows, sessions } = world("lead");
  const answer = await run("sessions.start", {
    ...CHOICE,
    prompt: "Fix the flaky test",
  });
  expect(host.start).toHaveBeenCalledWith(
    expect.objectContaining({ leadSessionId: "lead", startedBy: "lead" }),
    "app-lead-r1",
  );
  expect(sessions.get("app-lead-r1")?.leadSessionId).toBe("lead");
  expect(rows).toEqual([
    expect.objectContaining({
      tool: "start_session",
      status: "delivered",
      fromSessionId: "lead",
      toSessionId: "app-lead-r1",
    }),
  ]);
  expect(answer).toMatchObject({
    id: "app-lead-r1",
    submitted: true,
    message_id: rows[0].id,
  });
});

// Design 13.3. Defect: a Mono's start names the Mono's chat as a lead, so
// the session it starts is a worker that cannot report to the Mono.
it("starts a lead with no lead id from a Mono's chat", async () => {
  const { run, host, levels, sessions } = world("mono");
  await run("sessions.start", {
    ...CHOICE,
    prompt: "Fix the flaky test",
    notifyOnComplete: false,
  });
  const launch = vi.mocked(host.start).mock.calls[0][0];
  expect(launch.startedBy).toBe("mono-chat");
  expect(launch).not.toHaveProperty("leadSessionId");
  expect(sessions.get("app-mono-chat-r1")).not.toHaveProperty("leadSessionId");
  expect(levels.check).not.toHaveBeenCalled();
});

// Design 6.6 A3. Defect: a worker's start launches a session before, or
// without, the level check.
it("refuses a worker's start with worker_cannot_spawn and starts nothing", async () => {
  const { run, host, rows } = world("worker");
  expect(
    await failure(
      run("sessions.start", { ...CHOICE, prompt: "Split the work" }),
    ),
  ).toMatchObject({ ok: false, code: "worker_cannot_spawn" });
  expect(host.start).not.toHaveBeenCalled();
  expect(rows).toEqual([
    expect.objectContaining({
      tool: "start_session",
      status: "rejected",
      gateResult: "worker_cannot_spawn",
      fromSessionId: "worker",
    }),
  ]);
});

// Design 6.6 (levels review r1). Defect: the start writes its row before
// its fields are checked, or the missing model gets a code it does not have.
it("answers a lead's start without a model with the requireHarnessModel error, no row and no session", async () => {
  const { run, host, rows, sessions } = world("lead");
  expect(
    await failure(run("sessions.start", { harness: "codex", prompt: "Fix it" })),
  ).toEqual({
    ok: false,
    error: "harness and model are required; run models.list for exact IDs",
  });
  expect(rows).toEqual([]);
  expect(host.start).not.toHaveBeenCalled();
  expect(sessions.has("app-lead-r1")).toBe(false);
});

// Design 6.6 A5. Defect: a Mono steers a worker past its lead, because the
// level check runs only for session callers.
it("refuses a Mono's sessions.send to a worker with steer_via_lead and sends it nothing", async () => {
  const { run, host, rows, inbox } = world("mono");
  expect(
    await failure(
      run("sessions.send", {
        sessionId: "worker",
        prompt: "Stop and rebase",
        whenBusy: "steer",
      }),
    ),
  ).toMatchObject({ ok: false, code: "steer_via_lead" });
  expect(host.send).not.toHaveBeenCalled();
  expect(inbox.get("worker")).toBeUndefined();
  expect(rows).toEqual([
    expect.objectContaining({
      tool: "app_action",
      status: "rejected",
      gateResult: "steer_via_lead",
      fromSessionId: "mono-chat",
      toSessionId: "worker",
    }),
  ]);
});

// Design 6.6 A4. Defect: the check runs only for sessions.*, so a worker
// reads every note of the user.
it("refuses a worker's notes.list with worker_scope", async () => {
  const { run, host, rows } = world("worker");
  expect(await failure(run("notes.list", {}))).toMatchObject({
    ok: false,
    code: "worker_scope",
  });
  expect(host.notes).not.toHaveBeenCalled();
  expect(rows).toEqual([
    expect.objectContaining({
      tool: "app_action",
      status: "rejected",
      gateResult: "worker_scope",
      fromSessionId: "worker",
    }),
  ]);
});

// Design 6.6 (levels review r1). Defect: every Mono notes.*, memory.* and
// sessions.list call opens a write transaction for a check it always passes.
it("lists a Mono's notes without a level check", async () => {
  const { run, levels } = world("mono");
  expect(await run("notes.list", {})).toMatchObject({
    total: 1,
    notes: [expect.objectContaining({ id: "note-1" })],
  });
  expect(levels.check).not.toHaveBeenCalled();
});

// Design 6.6 runtime ceiling. Defect: a started lead in supervised starts
// a worker in full-access.
it("refuses a full-access worker from a started lead in supervised", async () => {
  const { run, host } = world("lead");
  expect(
    await failure(
      run("sessions.start", {
        ...CHOICE,
        prompt: "Deploy",
        runtimeMode: "full-access",
      }),
    ),
  ).toEqual({
    ok: false,
    error: "runtimeMode cannot be more permissive than this session's (supervised)",
  });
  expect(host.start).not.toHaveBeenCalled();
});

// Design 6.6 runtime ceiling. Defect: the ceiling refuses every mode other
// than the lead's own, a stricter one too.
it("starts a supervised worker from a started lead in auto", async () => {
  const { run, sessions } = world("lead", { runtimeMode: "auto" });
  await run("sessions.start", {
    ...CHOICE,
    prompt: "Review",
    runtimeMode: "supervised",
  });
  expect(sessions.get("app-lead-r1")?.runtimeMode).toBe("supervised");
});

// Design 9 (intents review r3). Defect: the start launches and only then
// fails on the reused key, so a retry with a new id starts a second session.
it("refuses a Mono's start whose request id a messages.send used, before any launch", async () => {
  const { run, host, rows, sessions } = world("mono");
  await run("messages.send", {
    message: "Is CI green?",
    send_to: "mono-2",
    wanting_reply: false,
    intent: "new",
  });
  expect(
    await failure(run("sessions.start", { ...CHOICE, prompt: "Fix CI" })),
  ).toEqual({
    ok: false,
    code: "request_id_reused",
    hint: REUSED,
    error: REUSED,
  });
  expect(host.start).not.toHaveBeenCalled();
  expect(sessions.has("app-mono-chat-r1")).toBe(false);
  expect(rows.map((row) => row.tool)).toEqual(["send_message"]);
});

// Design 9 (intents review r3). Defect: a draft retry of a submitted start
// answers success with no message_id, as if the draft were saved.
it("refuses a draft retry of a submitted start with request_id_reused", async () => {
  const { run, host } = world("mono");
  const input = { ...CHOICE, prompt: "Fix CI", notifyOnComplete: false };
  expect(await run("sessions.start", input)).toMatchObject({
    submitted: true,
    message_id: expect.any(String),
  });
  expect(
    await failure(run("sessions.start", { ...input, draft: true })),
  ).toMatchObject({ ok: false, code: "request_id_reused" });
  expect(host.start).toHaveBeenCalledOnce();
});

// Design 9 (intents review r2). Defect: a retry of a start is refused, or
// never learns the message id that its steers name.
it("answers a repeated Mono start with the first message_id and one row", async () => {
  const { run, rows } = world("mono");
  const input = { ...CHOICE, prompt: "Fix CI", notifyOnComplete: false };
  const first = await run("sessions.start", input);
  expect(await run("sessions.start", input)).toEqual(first);
  expect(rows).toHaveLength(1);
  expect(first).toMatchObject({ message_id: rows[0].id });
});

// Design 9 (intents review r2). Defect: a draft, which runs no work, gets a
// start row that a steer could name.
it("writes no start row for a draft start and answers no message_id", async () => {
  const { run, levels, rows } = world("mono");
  const answer = await run("sessions.start", {
    ...CHOICE,
    prompt: "Fix CI",
    draft: true,
  });
  expect(answer).toMatchObject({ submitted: false, draft: true });
  expect(answer).not.toHaveProperty("message_id");
  expect(levels.recordStart).not.toHaveBeenCalled();
  expect(rows).toEqual([]);
});
