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
    /** The rows mono_message_submit wrote or a test made, by id. */
    rows?: Record<string, Record<string, unknown>>;
    /** What mono_messages_unqueued answers once, by session. */
    unqueued?: Record<string, unknown[]>;
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
  mono_messages_unqueued: (_store, { sessionId }) => {
    const rows = window.unqueued?.[sessionId as string] ?? [];
    delete window.unqueued?.[sessionId as string];
    return rows;
  },
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
  extra: MountOptions["handlers"] = {},
  more: SessionSummary[] = [],
) {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        session("lead", "Fix login"),
        session("worker", "Write the tests", { leadSessionId: "lead" }),
        ...more,
      ],
    },
    { handlers: { ...handlers, ...extra } },
  );
  await page.evaluate(
    ({ ids, blocks }) => {
      for (const id of ids) window.appBackend!.store.blocks[id] = blocks;
    },
    { ids: withAccess, blocks: OPERATOR },
  );
}

/** `monocode app <action>` from `sessionId`'s running turn, as control.rs delivers it. */
async function callFrom(
  page: Page,
  app: AppShell,
  sessionId: string,
  action: string,
  input: Record<string, unknown>,
) {
  const id = `${action}-${sessionId}`;
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId,
    requestId: "r1",
    action,
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
  const brief = await callFrom(page, app, "lead", "messages.send", {
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
  const report = await callFrom(page, app, "worker", "messages.send", {
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
  const response = await callFrom(page, app, "worker", "messages.send", {
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

/** A mono_messages.rs row as the fake hands it to the app. */
type FakeRow = { id: string; handledBySession: string } & Record<
  string,
  unknown
>;

/** A row `from` sent to `to` that no queue holds yet, as mono_messages.rs stores it. */
const waitingRow = (
  id: string,
  from: string,
  to: string,
  body: string,
): FakeRow => ({
  id,
  requestId: "r1",
  part: 0,
  tool: "send_message",
  fromMonoId: null,
  fromSessionId: from,
  to: { kind: "session", sessionId: to },
  addressed: to,
  body,
  bodyChars: body.length,
  hop: 0,
  createdAt: 1,
  status: "delivered",
  wantingReply: false,
  intent: "new",
  replyTo: null,
  scope: null,
  handledBySession: to,
  delivery: "queue",
  slot: null,
});

/** Rust's `mono-message-notices` for a row this window has not queued yet. */
async function rowArrives(page: Page, app: AppShell, row: FakeRow) {
  await page.evaluate((row) => {
    (window.rows ??= {})[row.id] = row;
    (window.unqueued ??= {})[row.handledBySession] = [
      { message: row, target: null },
    ];
  }, row);
  await app.emit("mono-message-notices", { sessions: [row.handledBySession] });
}

// PR7-6 review r1 nit 2. Defect: a report from a worker that this window
// never opened reads as a deleted Mono's, because only loaded sessions count.
test("a report from a worker this window has not loaded names the worker", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead", "worker"]);
  await page.getByText("Fix login").click();
  await rowArrives(
    page,
    app,
    waitingRow("m_aaaaaaaaaaaa", "worker", "lead", "Login tests pass"),
  );
  await expect
    .poll(() => prompts(page, "lead"))
    .toEqual([
      expect.stringContaining(
        '<mono_message message_id="m_aaaaaaaaaaaa" from="Write the tests" session_id="worker" level="worker" wanting_reply="false">',
      ),
    ]);
});

// Design 13, PR7-6 review r1 nit 1. Defect: a row whose sender session is
// gone gets a session tag it cannot back, or its hand-off fails.
test("a row whose sender session is gone reads as a deleted Mono's", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead"]);
  await page.getByText("Fix login").click();
  await rowArrives(
    page,
    app,
    waitingRow("m_aaaaaaaaaaaa", "gone", "lead", "Old report"),
  );
  await expect
    .poll(() => prompts(page, "lead"))
    .toEqual([
      expect.stringContaining(
        '<mono_message message_id="m_aaaaaaaaaaaa" from="a deleted Mono" wanting_reply="false">\nOld report\n</mono_message>',
      ),
    ]);
});

/** `gh` lists one open pull request of the project's repo. */
const inboxHandlers: MountOptions["handlers"] = {
  git_github_repositories: () => ["acme/acme"],
  git_github_repo: () => "acme/acme",
  git_github_work_items: () => [
    {
      kind: "pr",
      number: 7,
      title: "Fix the flaky login",
      url: "https://github.com/acme/acme/pull/7",
      state: "open",
      stateReason: "",
      createdAt: "2026-10-18T00:00:00Z",
      updatedAt: "2026-10-18T00:00:00Z",
      labels: [],
      assignees: [],
      draft: false,
      repo: "acme/acme",
    },
  ],
};

// Design 13.1, PR 7 review r2 n5, PR7-6 review r1 m1. Defect: an Inbox Ask
// counts as a project session, so the gate lets a lead message it, or
// sessions.list shows it (loaded, or by its saved record).
test("an Inbox Ask is no project session for the gate or sessions.list", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead"], inboxHandlers);
  await page.getByText("Fix login").click();
  await page.getByRole("button", { name: "Inbox" }).first().click();
  await page.getByText("Fix the flaky login").first().click();
  await page.getByRole("button", { name: "Ask", exact: true }).first().click();
  await expect(page.getByLabel(/Ask about/)).toBeVisible();
  await callFrom(page, app, "lead", "messages.send", {
    message: "Run the login tests",
    send_to: "worker",
    wanting_reply: false,
    intent: "new",
  });
  const [call] = await submitted(page);
  const live = call.live as { sessionId: string; ownProject: boolean }[];
  const asks = live.filter(
    (entry) => entry.sessionId !== "lead" && entry.sessionId !== "worker",
  );
  expect(asks).toEqual([expect.objectContaining({ ownProject: false })]);
  const askId = asks[0].sessionId;
  // session_store.rs list_by_project filters only the legacy inbox_ask
  // column, so the Ask's saved hidden row lists with the project's sessions.
  await page.evaluate(
    ({ askId, project }) => {
      window.appBackend!.store.sessions.push({
        id: askId,
        cwd: project,
        harness: "pi",
        model: "pi:anthropic/claude-sonnet-4-5",
        runtimeMode: "supervised",
        title: "Ask · Fix the flaky login",
        createdAt: 2,
        updatedAt: 2,
        sidebarHidden: true,
        worktreeRemoved: false,
        additions: 0,
        deletions: 0,
        archived: false,
        pinned: false,
        draft: false,
      });
    },
    { askId, project: PROJECT },
  );
  const listed = await callFrom(page, app, "lead", "sessions.list", {});
  expect(listed).toMatchObject({ ok: true });
  const ids = JSON.stringify(listed);
  expect(ids).toContain('"worker"');
  expect(ids).not.toContain(askId);
});

/** `sessions.start` from the lead's running turn under request id r1. */
async function startFromLead(page: Page, app: AppShell, id: string) {
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId: "lead",
    requestId: "r1",
    action: "sessions.start",
    input: { harness: "pi", model: "pi:default", prompt: "Write the tests" },
    turnId: "turn-1",
  });
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.controlReplies?.find((reply) => reply.id === id),
        id,
      ),
    )
    .toBeDefined();
  return page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!.response,
    id,
  );
}

// Design 9: every app launch stamps its first block (appLaunchRepeat). PR7-11.
// A restart drops the window's receipts, so only the stamp tells the
// agent's retry from the user's own session. Defect: the window launches an
// app start without its request id stamp, so the retry after a restart is
// refused as request_id_reused, or launches a second agent turn.
test("a lead's start repeated under its request id after a restart answers the same session and launches once", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead"]);
  await page.getByText("Fix login").click();
  const first = await startFromLead(page, app, "start-1");
  expect(first).toMatchObject({ ok: true, result: { id: "app-lead-r1" } });
  await expect
    .poll(() => prompts(page, "app-lead-r1"))
    .toEqual(["Write the tests"]);
  await app.restart();
  await page.getByText("Fix login").first().click();
  const again = await startFromLead(page, app, "start-2");
  expect(again).toMatchObject({ ok: true, result: { id: "app-lead-r1" } });
  expect(await prompts(page, "app-lead-r1")).toEqual([]);
});

// Design 9, PR7-11 review r1 m2. A launched session that is stored but not
// open (a Mono's start opens no tab) is seen only by host.start's check,
// which reads the stored session. Defect: that check is skipped or reads
// only open sessions, so the agent's retry launches the session again.
test("a start repeated into its stored, unopened session answers that session and launches nothing", async ({
  page,
  app,
}) => {
  await mountSessions(page, app, ["lead"], {}, [
    session("app-lead-r1", "Write the tests", { leadSessionId: "lead" }),
  ]);
  await page.evaluate(() => {
    window.appBackend!.store.blocks["app-lead-r1"] = [
      {
        id: "u1",
        role: "user",
        text: "Write the tests",
        appRequestId: "app-lead-r1",
        startedBy: "lead",
      },
    ];
  });
  await page.getByText("Fix login").click();
  const again = await startFromLead(page, app, "start-1");
  expect(again).toMatchObject({ ok: true, result: { id: "app-lead-r1" } });
  expect(await prompts(page, "app-lead-r1")).toEqual([]);
});
