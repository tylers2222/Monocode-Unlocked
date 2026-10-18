import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

const session = (
  id: string,
  title: string,
  extra: Partial<SessionSummary> = {},
): SessionSummary => ({
  id,
  cwd: PROJECT,
  harness: "pi",
  model: "pi:anthropic/claude-sonnet-4-5",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
  ...extra,
});

/** A submitted /operator turn: the thread has app access from then on. */
const OPERATOR: Block[] = [
  {
    id: "u1",
    role: "user",
    text: "Report to me when tests pass",
    monocode: true,
  },
];

type ControlResponse =
  | { ok: true; result: Record<string, unknown> }
  | { ok: false; code: string; hint: string };

declare global {
  interface Window {
    controlReplies?: { id: string; response: ControlResponse }[];
    /** What mono_message_submit answers; accepted when unset. */
    submitAnswer?: unknown;
    /** The rows mono_message_submit wrote, by id. */
    rows?: Record<string, Record<string, unknown>>;
  }
}

/**
 * mono_messages.rs as far as one accepted send goes: `submit` writes the row
 * the call names and routes it to its recipient's queue, unless
 * `window.submitAnswer` holds the gate's refusal. The gate is tested in
 * mono_gate.rs. Pi in RPC mode (piFamily.ts) accepts every request at once.
 */
const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as {
      id?: string;
      type?: string;
    };
    if (request.id)
      void window.appBackend!.emit("harness-stdout", {
        sessionId,
        line: JSON.stringify({
          type: "response",
          id: request.id,
          command: request.type,
          success: true,
          data: {},
        }),
      });
    return null;
  },
  control_reply: (_store, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  control_owns_session: () => true,
  mono_message_submit: (_store, { caller, requestId, input }) => {
    if (window.submitAnswer) return window.submitAnswer;
    const rows = (window.rows ??= {});
    const from = caller as { sessionId: string };
    const send = input as { message: string; send_to: string };
    const row = {
      id: `m_${String.fromCharCode(97 + Object.keys(rows).length).repeat(12)}`,
      requestId,
      part: 0,
      tool: "send_message",
      fromMonoId: null,
      fromSessionId: from.sessionId,
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
    rows[row.id] = row;
    return {
      outcome: "accepted",
      messages: [{ message: row, target: null }],
      answered: [],
      duplicate: false,
      routes: [{ sessionId: send.send_to, via: "queue", messageIds: [row.id] }],
    };
  },
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
  mono_message_assign_slot: (_store, { id, turnId, kind }) => {
    const row = window.rows![id as string];
    return {
      message: {
        ...row,
        delivery: kind,
        slot: { turnId, deliveredAt: null },
      },
      target: null,
    };
  },
  mono_message_mark_delivery: () => null,
};

/** Mounts the lead and its worker; `withAccess` gave /operator to each. */
async function mountSessions(
  page: Page,
  app: AppShell,
  withAccess: ("lead" | "worker")[],
) {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        session("lead", "Fix login"),
        session("worker", "Write the tests", { leadSessionId: "lead" }),
      ],
    },
    { handlers },
  );
  await page.evaluate(
    ({ ids, blocks }) => {
      for (const id of ids) window.appBackend!.store.blocks[id] = blocks;
    },
    { ids: withAccess, blocks: OPERATOR },
  );
}

/** `monocode app messages send` from `sessionId`'s running turn, as control.rs delivers it. */
async function sendFrom(
  page: Page,
  app: AppShell,
  sessionId: string,
  input: Record<string, unknown>,
) {
  const id = `send-${sessionId}`;
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId,
    requestId: "r1",
    action: "messages.send",
    input,
    turnId: "turn-1",
  });
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.controlReplies?.some((reply) => reply.id === id),
        id,
      ),
    )
    .toBe(true);
  return page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!.response,
    id,
  );
}

function submitted(page: Page) {
  return page.evaluate(() =>
    window
      .appBackend!.calls.filter(
        ({ command }) => command === "mono_message_submit",
      )
      .map(({ args }) => args as Record<string, unknown>),
  );
}

function prompts(page: Page, sessionId: string) {
  return page.evaluate(
    (sessionId) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_write" &&
            (args as { sessionId: string }).sessionId === sessionId,
        )
        .map(
          ({ args }) =>
            JSON.parse((args as { line: string }).line) as {
              type: string;
              message?: string;
            },
        )
        .filter((request) => request.type === "prompt")
        .map((request) => request.message ?? ""),
    sessionId,
  );
}

// Design 13.4, PR7-6. Defect: a lead or worker with /operator access
// answers not_a_mono, or its message reaches the other as a deleted Mono's,
// with no session id to answer to, or with the wrong level.
test("a lead's brief and its worker's report reach each other's turns named by title, session id and level", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead", "worker"]);
  await page.getByText("Fix login").click();
  const brief = await sendFrom(page, app, "lead", {
    message: "Run the login tests",
    send_to: "worker",
    wanting_reply: false,
    intent: "new",
  });
  expect(brief).toEqual({
    ok: true,
    result: expect.objectContaining({ message_id: "m_aaaaaaaaaaaa" }),
  });
  const [call] = await submitted(page);
  expect(call.caller).toEqual({
    kind: "session",
    sessionId: "lead",
    turnId: "turn-1",
    access: "operator",
    leadSessionId: null,
    cwd: PROJECT,
  });
  expect(call.live).toContainEqual(
    expect.objectContaining({ sessionId: "worker", leadSessionId: "lead" }),
  );
  await expect
    .poll(() => prompts(page, "worker"))
    .toEqual([
      expect.stringContaining(
        '<mono_message message_id="m_aaaaaaaaaaaa" from="Fix login" session_id="lead" level="lead" wanting_reply="false">\nRun the login tests\n</mono_message>',
      ),
    ]);
  const report = await sendFrom(page, app, "worker", {
    message: "Login tests pass",
    send_to: "lead",
    wanting_reply: false,
    intent: "new",
  });
  expect(report).toMatchObject({ ok: true });
  await expect
    .poll(() => prompts(page, "lead"))
    .toEqual([
      expect.stringContaining(
        '<mono_message message_id="m_bbbbbbbbbbbb" from="Write the tests" session_id="worker" level="worker" wanting_reply="false">\nLogin tests pass\n</mono_message>',
      ),
    ]);
});

// Design 13.4, PR7-5b codes. Defect: the gate's level refusal of a worker
// never reaches it, or reaches it without its code and hint.
test("a worker's send that the gate refuses answers worker_scope with the gate's hint", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["worker"]);
  await page.getByText("Write the tests").click();
  const hint =
    'A worker can message only its lead. Message your lead (lead): messages.send with send_to "lead".';
  await page.evaluate((hint) => {
    window.submitAnswer = {
      outcome: "failed",
      error: { code: "worker_scope", hint, message_id: "m_bbbbbbbbbbbb" },
    };
  }, hint);
  const response = await sendFrom(page, app, "worker", {
    message: "Can you review this?",
    send_to: "mono-1",
    wanting_reply: true,
    intent: "new",
  });
  expect(response).toMatchObject({
    ok: false,
    code: "worker_scope",
    hint,
    message_id: "m_bbbbbbbbbbbb",
  });
  const [call] = await submitted(page);
  expect(call.caller).toMatchObject({
    kind: "session",
    sessionId: "worker",
    leadSessionId: "lead",
  });
});
