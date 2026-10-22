import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, type Handler, test } from "./app/fixture";

/** Lane D's builder row as session_store.rs get_session returns it. */
type SavedRecord = Pick<
  SessionSummary,
  "harness" | "model" | "runtimeMode" | "title"
> & { blocks: Block[] };
const laneD = JSON.parse(
  readFileSync(
    new URL("./fixtures/ade73-lane-d.realdb.json", import.meta.url),
    "utf8",
  ),
) as { record: SavedRecord };

const PROJECT = "/Users/test/acme";
/** What omp reported when the Anthropic rate limit stopped lane D's turn. */
const RATE_LIMIT = laneD.record.blocks.find(
  (block) => block.notice === "error",
)!.text;

const session = (id: string, title: string): SessionSummary => ({
  id,
  cwd: PROJECT,
  harness: "claude",
  model: "sonnet",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

/** A submitted /operator turn: the orchestrator has app access from then on. */
const OPERATOR: Block[] = [
  { id: "u1", role: "user", text: "Run the builders", monocode: true },
];

type ControlResponse =
  | { ok: true; result: Record<string, unknown> }
  | { ok: false; error: string; code?: string };

declare global {
  interface Window {
    controlReplies?: { id: string; response: ControlResponse }[];
    savedBuilder?: unknown;
    monoRow?: Record<string, unknown>;
  }
}

/**
 * Claude Code on PATH: it answers `initialize` and nothing else, so a turn
 * runs until the test prints its result. control.rs takes the CLI replies.
 */
const handlers: Record<string, Handler> = {
  harness_resolve_claude: () => ({ path: "/usr/local/bin/claude" }),
  harness_spawn: () => 4242,
  harness_write: (_, { sessionId, line }) => {
    const input = JSON.parse(line as string) as {
      request_id?: string;
      request?: { subtype: string };
    };
    if (input.request?.subtype === "initialize")
      setTimeout(() =>
        window.appBackend!.emit("harness-stdout", {
          sessionId,
          lines: [
            JSON.stringify({
              type: "control_response",
              response: {
                subtype: "success",
                request_id: input.request_id,
                response: {},
              },
            }),
          ],
        }),
      );
    return null;
  },
  control_reply: (_, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  control_owns_session: () => true,
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
};

async function mount(
  page: Page,
  app: AppShell,
  builder: SessionSummary = session("builder", "Fix the parser"),
  extra: Record<string, Handler> = {},
  more: SessionSummary[] = [],
  blocks: Record<string, Block[]> = {},
) {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [session("orchestrator", "Run the builders"), builder, ...more],
      blocks,
    },
    { handlers: { ...handlers, ...extra } },
  );
  await page.evaluate((blocks) => {
    window.appBackend!.store.blocks.orchestrator = blocks;
  }, OPERATOR);
  await page.getByText("Run the builders").click();
}

/** `from` emits `monocode app <action>`, as control.rs delivers it. */
async function request(
  app: AppShell,
  requestId: string,
  action: string,
  input: Record<string, unknown>,
  from = "orchestrator",
) {
  await app.emit("monocode-control-request", {
    id: requestId,
    namespace: "app",
    sessionId: from,
    requestId,
    action,
    input,
    turnId: "turn-1",
  });
}

async function reply(page: Page, requestId: string) {
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.controlReplies?.some((reply) => reply.id === id),
        requestId,
      ),
    )
    .toBe(true);
  return page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!.response,
    requestId,
  );
}

async function send(
  page: Page,
  app: AppShell,
  requestId: string,
  prompt: string,
  whenBusy?: "followUp",
) {
  await request(app, requestId, "sessions.send", {
    sessionId: "builder",
    prompt,
    notifyOnComplete: false,
    ...(whenBusy ? { whenBusy } : {}),
  });
  return reply(page, requestId);
}

/** "new turn" per turn the app opened, then each prompt it wrote to Claude. */
function turns(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    window.appBackend!.calls.flatMap(({ command, args }) => {
      const call = args as { sessionId?: string; line?: string };
      if (call.sessionId !== "builder") return [];
      if (command === "session_checkpoint_begin_turn") return ["new turn"];
      if (command !== "harness_write") return [];
      const input = JSON.parse(call.line!) as {
        type: string;
        message?: { content: { text: string }[] };
      };
      return input.type === "user"
        ? input.message!.content.map((part) => part.text)
        : [];
    }),
  );
}

/** Claude Code prints the turn's result; Rust forwards stdout in `harness-stdout`. */
function endTurn(app: AppShell, failure?: string) {
  return app.emit("harness-stdout", {
    sessionId: "builder",
    lines: [
      JSON.stringify(
        failure
          ? {
              type: "result",
              subtype: "error_during_execution",
              is_error: true,
              errors: [failure],
            }
          : {
              type: "result",
              subtype: "success",
              is_error: false,
              num_turns: 1,
              result: "Done.",
            },
      ),
    ],
  });
}

const delivered = (delivery: string) => ({
  ok: true,
  result: expect.objectContaining({ delivery }),
});

test("a follow-up queued before a rate-limited turn runs after the next turn", async ({
  app,
  page,
}) => {
  await mount(page, app);
  expect(await send(page, app, "r1", "Fix the parser")).toEqual(
    delivered("sent"),
  );
  await expect.poll(() => turns(page)).toEqual(["new turn", "Fix the parser"]);

  expect(
    await send(page, app, "r2", "Also cover the empty input", "followUp"),
  ).toEqual(delivered("queued"));
  await endTurn(app, RATE_LIMIT);

  expect(await send(page, app, "r3", "Your turn stopped; go on")).toEqual(
    delivered("sent"),
  );
  await expect
    .poll(() => turns(page), "the resume prompt opens a turn")
    .toEqual([
      "new turn",
      "Fix the parser",
      "new turn",
      "Your turn stopped; go on",
    ]);

  await endTurn(app);
  await expect
    .poll(() => turns(page), "the accepted follow-up runs after that turn")
    .toEqual([
      "new turn",
      "Fix the parser",
      "new turn",
      "Your turn stopped; go on",
      "new turn",
      "Also cover the empty input",
    ]);
});

test("an error pause and each item's queue time survive a restart, and the next turn still lifts the pause", async ({
  app,
  page,
}) => {
  await mount(page, app);
  await send(page, app, "r1", "Fix the parser");
  await send(page, app, "r2", "Also cover the empty input", "followUp");
  await endTurn(app, RATE_LIMIT);

  await app.restart();
  expect(
    await page.evaluate(() => window.appBackend!.store.queues.builder),
    "the saved row keeps the reason and the time",
  ).toEqual(
    expect.objectContaining({
      queueStatus: "paused",
      queuePause: "error",
      queuedMessages: [
        expect.objectContaining({
          text: "Also cover the empty input",
          queuedAt: expect.any(Number),
        }),
      ],
    }),
  );

  await page.getByText("Run the builders").first().click();
  expect(await send(page, app, "r3", "Your turn stopped; go on")).toEqual(
    delivered("sent"),
  );
  await expect
    .poll(() => turns(page))
    .toEqual(["new turn", "Your turn stopped; go on"]);
  await endTurn(app);
  await expect
    .poll(() => turns(page), "the follow-up from before the restart runs")
    .toEqual([
      "new turn",
      "Your turn stopped; go on",
      "new turn",
      "Also cover the empty input",
    ]);
});

test("sessions.queue.clear drops the queue for the session that started it and refuses anyone else", async ({
  app,
  page,
}) => {
  await mount(page, app, undefined, {}, [session("stranger", "Fix the docs")], {
    stranger: OPERATOR,
    builder: [
      {
        id: "b0",
        role: "user",
        text: "Fix the parser",
        appRequestId: "app-orchestrator-r0",
        startedBy: "orchestrator",
      },
    ],
  });
  await page.getByText("Fix the docs").click();
  await send(page, app, "r1", "Run the tests");
  await send(page, app, "r2", "Also cover the empty input", "followUp");

  await request(
    app,
    "r3",
    "sessions.queue.clear",
    { sessionId: "builder" },
    "stranger",
  );
  expect(await reply(page, "r3")).toMatchObject({
    ok: false,
    code: "not_session_owner",
  });
  await request(app, "r4", "sessions.queue.clear", { sessionId: "builder" });
  expect(await reply(page, "r4")).toEqual({
    ok: true,
    result: { sessionId: "builder", cleared: 1 },
  });

  await endTurn(app);
  expect(await send(page, app, "r5", "Update the changelog")).toEqual(
    delivered("sent"),
  );
  await expect
    .poll(() => turns(page), "the cleared follow-up never runs")
    .toEqual(["new turn", "Run the tests", "new turn", "Update the changelog"]);
  expect(
    await page.evaluate(
      () => window.appBackend!.store.queues.builder?.queuedMessages,
    ),
    "the saved row has no queue",
  ).toBeUndefined();
});

/**
 * mono_messages.rs for one accepted send: it writes the row and routes it to
 * the recipient's queue. The gate is tested in mono_gate.rs.
 */
const monoRoute: Record<string, Handler> = {
  mono_message_submit: (_, { requestId, input }) => {
    const send = input as { message: string; send_to: string };
    const row = {
      id: "m_aaaaaaaaaaaa",
      requestId,
      part: 0,
      tool: "send_message",
      fromMonoId: null,
      fromSessionId: "orchestrator",
      to: { kind: "session", sessionId: send.send_to },
      addressed: send.send_to,
      body: send.message,
      bodyChars: send.message.length,
      hop: 0,
      createdAt: 1,
      status: "delivered",
      wantingReply: false,
      intent: "new",
      replyTo: null,
      scope: null,
      handledBySession: send.send_to,
      delivery: "queue",
      slot: null,
    };
    window.monoRow = row;
    return {
      outcome: "accepted",
      messages: [{ message: row, target: null }],
      answered: [],
      duplicate: false,
      routes: [{ sessionId: send.send_to, via: "queue", messageIds: [row.id] }],
    };
  },
  mono_message_assign_slot: (_, { turnId, kind }) => ({
    message: {
      ...window.monoRow,
      delivery: kind,
      slot: { turnId, deliveredAt: null },
    },
    target: null,
  }),
  mono_message_mark_delivery: () => null,
};

test("a Mono message queued behind a rate-limited turn runs after the next turn too", async ({
  app,
  page,
}) => {
  await mount(page, app, undefined, monoRoute);
  await send(page, app, "r1", "Fix the parser");
  await send(page, app, "r2", "Also cover the empty input", "followUp");
  await endTurn(app, RATE_LIMIT);

  await request(app, "r3", "messages.send", {
    message: "Then run the tests",
    send_to: "builder",
    wanting_reply: false,
    intent: "new",
  });
  expect(await reply(page, "r3")).toMatchObject({ ok: true });
  expect(await send(page, app, "r4", "Your turn stopped; go on")).toEqual(
    delivered("sent"),
  );
  await expect
    .poll(() => turns(page), "the resume prompt opens a turn")
    .toHaveLength(4);

  await endTurn(app);
  await expect
    .poll(() => turns(page), "the CLI follow-up runs after that turn")
    .toHaveLength(6);
  await endTurn(app);
  await expect
    .poll(
      async () => (await turns(page)).slice(4),
      "then the Mono message, in queue order",
    )
    .toEqual([
      "new turn",
      "Also cover the empty input",
      "new turn",
      expect.stringContaining("Then run the tests"),
    ]);
});

test("a follow-up for a busy session whose queue the user paused is refused", async ({
  app,
  page,
}) => {
  await mount(page, app);
  await send(page, app, "r1", "Fix the parser");
  await send(page, app, "r2", "Also cover the empty input", "followUp");
  await request(app, "r3", "sessions.stop", { sessionId: "builder" });
  expect(await reply(page, "r3")).toMatchObject({ ok: true });

  expect(await send(page, app, "r4", "Start over")).toEqual(delivered("sent"));
  expect(
    await send(page, app, "r5", "Then run the tests", "followUp"),
  ).toMatchObject({
    ok: false,
    error: expect.stringContaining("queue is paused"),
  });
});

test("real state: lane D's paused queue refuses a follow-up instead of holding it for good", async ({
  app,
  page,
}) => {
  const saved = laneD.record;
  await page.addInitScript((record) => {
    window.savedBuilder = record;
  }, saved);
  await mount(
    page,
    app,
    {
      ...session("builder", saved.title),
      harness: saved.harness,
      model: saved.model,
      runtimeMode: saved.runtimeMode,
    },
    {
      harness_resolve_omp: () => ({ path: "/usr/local/bin/omp" }),
      // omp in RPC mode (piFamily.ts) accepts every request at once.
      harness_write: (_, { sessionId, line }) => {
        const rpc = JSON.parse(line as string) as {
          id?: string;
          type?: string;
        };
        if (rpc.id)
          void window.appBackend!.emit("harness-stdout", {
            sessionId,
            line: JSON.stringify({
              type: "response",
              id: rpc.id,
              command: rpc.type,
              success: true,
              data: {},
            }),
          });
        return null;
      },
      // session_store.rs get_session: the builder's saved row, as read from the copy.
      session_get: (store, { sessionId }) =>
        sessionId === "builder"
          ? window.savedBuilder
          : {
              id: sessionId,
              cwd: "/Users/test/acme",
              harness: "claude",
              model: "sonnet",
              modelSettings: {},
              runtimeMode: "supervised",
              title: "Run the builders",
              blocks: store.blocks[sessionId as string] ?? [],
              createdAt: 1,
              updatedAt: 1,
            },
    },
  );

  expect(await send(page, app, "r1", "Your turn stopped; go on")).toEqual(
    delivered("sent"),
  );
  expect(
    await send(page, app, "r2", "Then run the tests", "followUp"),
  ).toMatchObject({
    ok: false,
    error: expect.stringContaining("queue is paused"),
  });
});

test("a follow-up waits for the turn end, even one sent as the turn ends, and runs at once on an idle session", async ({
  app,
  page,
}) => {
  await mount(page, app);
  await send(page, app, "r1", "Fix the parser");
  await expect.poll(() => turns(page)).toEqual(["new turn", "Fix the parser"]);

  expect(
    await send(page, app, "r2", "Also cover the empty input", "followUp"),
  ).toEqual(delivered("queued"));
  await Promise.all([
    request(app, "r3", "sessions.send", {
      sessionId: "builder",
      prompt: "Then run the tests",
      notifyOnComplete: false,
      whenBusy: "followUp",
    }),
    endTurn(app),
  ]);
  expect(await reply(page, "r3")).toMatchObject({ ok: true });
  await expect
    .poll(() => turns(page), "the turn end starts the next turn")
    .toHaveLength(4);
  await endTurn(app);
  await expect
    .poll(
      async () => (await turns(page)).slice(2),
      "each follow-up runs once, in a turn of its own",
    )
    .toEqual(["new turn", expect.any(String), "new turn", expect.any(String)]);
  expect(
    (await turns(page)).filter((_, index) => index === 3 || index === 5).sort(),
  ).toEqual(["Also cover the empty input", "Then run the tests"]);
  await endTurn(app);

  expect(
    await send(page, app, "r4", "Update the changelog", "followUp"),
  ).toEqual(delivered("sent"));
  await expect
    .poll(
      async () => (await turns(page)).slice(6),
      "an idle session runs it at once",
    )
    .toEqual(["new turn", "Update the changelog"]);
});
