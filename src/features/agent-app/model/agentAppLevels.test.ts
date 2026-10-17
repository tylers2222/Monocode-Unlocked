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
import { appLaunchRepeat } from "../../../app/model/quickLaunchSession";
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
  const levels: AgentAppHost["levels"] = {
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
      if (prior && prior.hash !== hash("sessions.start", input))
        throw new AppActionError("request_id_reused", REUSED);
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
    // As App.tsx: every launch stamps its request id on its first block,
    // and a repeated id launches nothing unless appLaunchRepeat refuses it.
    start: vi.fn(async (launch: QuickLaunch, id: string) => {
      if (appLaunchRepeat(sessions.get(id), launch, id)) return;
      sessions.set(id, {
        ...newSession(launch.harness, launch.cwd, launch.model, launch.runtimeMode),
        id,
        ...(launch.leadSessionId ? { leadSessionId: launch.leadSessionId } : {}),
        blocks: [
          {
            id: "first",
            role: "user",
            text: launch.prompt,
            appRequestId: id,
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
    draft: vi.fn(async (id: string, prompt: string) => {
      inbox.set(id, [...(inbox.get(id) ?? []), prompt]);
      return { alreadySaved: false, draft: true };
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

/** A Mono chat, a habit run of its Mono, and an /operator lead that is not the worker's lead. */
const NOT_THE_WORKERS_LEAD = [
  {
    who: "a Mono chat",
    open: () => world("mono"),
  },
  {
    who: "a habit run",
    open: () => {
      const opened = world("mono");
      const roles = opened.host as unknown as {
        isMono: (id: string) => boolean;
        isHabitRun: (id: string) => boolean;
      };
      roles.isMono = () => false;
      roles.isHabitRun = (id) => id === "mono-chat";
      return opened;
    },
  },
  {
    who: "an /operator lead",
    open: () => {
      const opened = world("lead");
      opened.sessions.get("lead")!.blocks = [
        { id: "op", role: "user", text: "Turn on the app CLI", monocode: true },
      ];
      opened.sessions.get("worker")!.leadSessionId = "lead-2";
      return opened;
    },
  },
];

// Design 6.6 A5, PR 7 review r1 B1. Defect: the level check reads the id
// as given and the action trims it, so " worker" passes A5 as no target
// and the text reaches the worker.
it.each(NOT_THE_WORKERS_LEAD)(
  "refuses $who's padded sessions.send and sessions.draft to a worker with a row and delivers nothing",
  async ({ open }) => {
    for (const [action, sessionId] of [
      ["sessions.send", " worker"],
      ["sessions.draft", "worker\n"],
    ] as const) {
      const { run, host, rows, inbox } = open();
      expect(
        await failure(run(action, { sessionId, prompt: "Stop and rebase" })),
        action,
      ).toMatchObject({ ok: false, code: "steer_via_lead" });
      expect(host.send).not.toHaveBeenCalled();
      expect(host.draft).not.toHaveBeenCalled();
      expect(inbox.size).toBe(0);
      expect(rows).toEqual([
        expect.objectContaining({
          tool: "app_action",
          gateResult: "steer_via_lead",
          toSessionId: "worker",
        }),
      ]);
    }
  },
);

// PR 7 review r1 B1. Defect: a blank id after the trim reaches the level
// check as no target.
it("refuses a blank sessionId as invalid_input before the level check", async () => {
  const { run, levels, rows } = world("mono");
  expect(
    await failure(run("sessions.send", { sessionId: "  ", prompt: "Hi" })),
  ).toMatchObject({ ok: false, code: "invalid_input" });
  expect(levels.check).not.toHaveBeenCalled();
  expect(rows).toEqual([]);
});

// Design 6.6 A5, PR 7 review r1 m13. Defect: a Mono's sessions.draft into
// a worker skips the level check that its sessions.send gets.
it("refuses a Mono's sessions.draft into a worker with steer_via_lead", async () => {
  const { run, host, rows } = world("mono");
  expect(
    await failure(
      run("sessions.draft", { sessionId: "worker", prompt: "Rebase first" }),
    ),
  ).toMatchObject({ ok: false, code: "steer_via_lead" });
  expect(host.draft).not.toHaveBeenCalled();
  expect(rows).toEqual([
    expect.objectContaining({
      tool: "app_action",
      gateResult: "steer_via_lead",
      toSessionId: "worker",
    }),
  ]);
});

// Design 6.6 runtime ceiling (a caller whose access is started), PR 7
// review r1 m13. Defect: the ceiling also binds an /operator lead.
it("starts a full-access worker from an /operator lead in supervised", async () => {
  const { run, sessions } = world("lead");
  sessions.get("lead")!.blocks = [
    { id: "op", role: "user", text: "Turn on the app CLI", monocode: true },
  ];
  await run("sessions.start", {
    ...CHOICE,
    prompt: "Deploy",
    runtimeMode: "full-access",
  });
  expect(sessions.get("app-lead-r1")?.runtimeMode).toBe("full-access");
});

// Design 9 (intents review r3), PR 7 review r1 m13. Defect: a habit run
// skips the prior read, so it launches under a key a send used.
it("refuses a habit run's start whose request id a messages.send used, before any launch", async () => {
  const { run, host } = NOT_THE_WORKERS_LEAD[1].open();
  await run("messages.send", {
    message: "Is CI green?",
    send_to: "mono-2",
    wanting_reply: false,
    intent: "new",
  });
  expect(
    await failure(
      run("sessions.start", {
        ...CHOICE,
        prompt: "Fix CI",
        notifyOnComplete: false,
      }),
    ),
  ).toMatchObject({ ok: false, code: "request_id_reused" });
  expect(host.start).not.toHaveBeenCalled();
});

// PR 7 review r1 m3. Defect: SQLite counts a prompt only up to U+0000, so
// a prompt with one passes every check, launches, and its row then fails.
it.each(["mono", "lead"] as const)(
  "refuses a %s's start whose prompt holds U+0000 before any check or launch",
  async (who) => {
    const { run, host, levels, rows } = world(who);
    expect(
      await failure(run("sessions.start", { ...CHOICE, prompt: "Fix\u0000it" })),
    ).toMatchObject({ ok: false, code: "invalid_input" });
    expect(levels.check).not.toHaveBeenCalled();
    expect(levels.startPrior).not.toHaveBeenCalled();
    expect(host.start).not.toHaveBeenCalled();
    expect(rows).toEqual([]);
  },
);

// PR 7 review r1 m5 and n6. Defect: after a failed record, a retry under
// the key with other input meets the launched session, launches nothing
// and is answered as a start with a row for work that never ran; the first
// failure does not say the session runs.
it("answers a changed retry after a failed record with an error, not a row", async () => {
  const { run, levels, rows, sessions } = world("lead");
  vi.mocked(levels.recordStart).mockRejectedValueOnce(
    new Error("database is locked"),
  );
  expect(
    await failure(run("sessions.start", { ...CHOICE, prompt: "Fix the flaky test" })),
  ).toEqual({
    ok: false,
    error:
      "database is locked. Session app-lead-r1 was started anyway, so do not start it again; read or steer it by its id.",
  });
  expect(
    await failure(
      run("sessions.start", { ...CHOICE, prompt: "Delete the flaky test" }),
    ),
  ).toEqual({
    ok: false,
    error: "Request ID was already used for another session launch",
  });
  expect(rows).toEqual([]);
  expect(sessions.get("app-lead-r1")?.blocks[0].text).toBe("Fix the flaky test");
});

// PR 7 review r1 m5. Defect: a draft start that the user edited and sent
// gets a start row on a submitted retry, so the user's own turn becomes a
// Mono steer target.
it("refuses a submitted retry of a draft start that the user sent", async () => {
  const { run, rows, sessions } = world("mono");
  const input = { ...CHOICE, prompt: "Fix CI", notifyOnComplete: false };
  await run("sessions.start", { ...input, draft: true });
  const launched = sessions.get("app-mono-chat-r1")!;
  // The user's send replaces the draft block with one that has no stamp.
  sessions.set(launched.id, {
    ...launched,
    blocks: [{ id: "user", role: "user", text: "Fix CI, but only the lint job" }],
  });
  expect(await failure(run("sessions.start", input))).toEqual({
    ok: false,
    error: "Request ID was already used for another session launch",
  });
  expect(rows).toEqual([]);
});
