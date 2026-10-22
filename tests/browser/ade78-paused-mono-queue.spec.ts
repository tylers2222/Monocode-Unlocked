import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-78. The ship 6 swap (MonoCode 2, 2026-10-21 08:49) quit the app while
// the Fork Orchestrator ran a turn with General's messages queued behind it.
// The relaunch paused that queue ("Paused when MonoCode quit"), so every
// message General sent for the next 29 hours waited for a Resume, while
// messages.send answered `delivery: "queue"` and monos.get looked like any
// queue. `ade78-ship6.realdb.json` is reduced from the .backup the swap took
// at 08:48:51 and from the database after it: both Monos, their chats on
// omp:default, and General's real messages. The model was not the cause: a
// chat on a model id the catalog does not list still runs, so no send to it
// is held.

type Mono = {
  id: string;
  role: "mono" | "giga";
  data: { name: string; projects: string[]; sessionId: string };
  chat: { summary: SessionSummary };
};
type Message = { id: string; body: string };
const db = JSON.parse(
  readFileSync(
    new URL("./fixtures/ade78-ship6.realdb.json", import.meta.url),
    "utf8",
  ),
) as {
  fork: Mono;
  general: Mono;
  beforeQuit: Message[];
  afterRelaunch: Message;
};
const FORK = db.fork;
const GENERAL = db.general;

type ControlResponse =
  { ok: true; result: Record<string, unknown> } | { ok: false; error: string };
type OmpLine = { sessionId: string; type?: string; message?: string };
declare global {
  interface Window {
    ompLines?: OmpLine[];
    controlReplies?: { id: string; response: ControlResponse }[];
    /** The real id of the next row mono_message_submit writes. */
    nextRowId?: string;
    /** The row mono_message_submit wrote last. */
    lastRow?: Record<string, unknown>;
    /** Each send_to Mono's chat, as mono_gate.rs reads it from `monos`. */
    monoChats?: Record<string, string>;
  }
}

const handlers: MountOptions["handlers"] = {
  harness_resolve_omp: () => ({ path: "/usr/local/bin/omp" }),
  harness_spawn: () => 4242,
  // Omp in RPC mode accepts every request; no turn ends on its own.
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as {
      id?: string;
      type?: string;
      message?: string;
    };
    (window.ompLines ??= []).push({
      sessionId: sessionId as string,
      type: request.type,
      message: request.message,
    });
    if (request.id)
      void window.appBackend!.emit("harness-stdout", {
        sessionId,
        line: JSON.stringify({
          type: "response",
          id: request.id,
          command: request.type,
          success: true,
          data:
            request.type === "get_state"
              ? { sessionId: `provider-${sessionId}` }
              : {},
        }),
      });
    return null;
  },
  omp_session_interjections: () => [],
  omp_active_assistant_texts: () => [],
  session_checkpoint_keep: () => null,
  mono_chat_keep_alive: () => false,
  control_owns_session: () => true,
  control_reply: (_store, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  // mono_messages.rs for one accepted send from a Mono to a Mono's chat. The
  // gate is tested in mono_gate.rs.
  mono_message_submit: (_store, { caller, requestId, input }) => {
    const from = caller as { monoId: string; sessionId: string };
    const send = input as { message: string; send_to: string };
    const chat = window.monoChats![send.send_to];
    const row = {
      id: window.nextRowId,
      requestId,
      part: 0,
      tool: "send_message",
      fromMonoId: from.monoId,
      fromSessionId: from.sessionId,
      to: { kind: "mono", monoId: send.send_to },
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
      handledBySession: chat,
      delivery: "queue",
      slot: null,
    };
    window.lastRow = row;
    return {
      outcome: "accepted",
      messages: [{ message: row, target: null }],
      answered: [],
      duplicate: false,
      routes: [{ sessionId: chat, via: "queue", messageIds: [row.id] }],
    };
  },
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
  // The idle receiver's queue pump opens a turn with the row it was sent.
  mono_message_assign_slot: (_store, { turnId, kind }) => ({
    message: {
      ...window.lastRow,
      delivery: kind,
      slot: { turnId, deliveredAt: null },
    },
    target: null,
  }),
  mono_message_mark_delivery: () => null,
  "plugin:dialog|message": () => "Ok",
  resolve_project_location: (_store, { path }) => ({ path, identity: path }),
};

/** The chat map is set on every load, so it outlives `app.restart()`. */
async function mountMonos(
  app: AppShell,
  page: Page,
  forkModel = FORK.chat.summary.model,
) {
  await page.addInitScript(
    (chats) => {
      window.monoChats = chats;
    },
    { [FORK.id]: FORK.data.sessionId, [GENERAL.id]: GENERAL.data.sessionId },
  );
  await app.mount(
    {
      projects: [...FORK.data.projects, ...GENERAL.data.projects],
      sessions: [
        { ...FORK.chat.summary, model: forkModel },
        GENERAL.chat.summary,
      ].map((summary) => ({ ...summary, createdAt: 1, updatedAt: 1 })),
      monos: [FORK, GENERAL].map(({ id, role, data }) => ({ id, role, data })),
    },
    { handlers },
  );
}

async function openMono(page: Page, mono: Mono) {
  await page
    .getByRole("button", { name: new RegExp(`^${mono.data.name}, `) })
    .click();
  await expect(
    page.getByPlaceholder(`Message ${mono.data.name}`),
  ).toBeVisible();
}

/** One `monocode app` call from General's running turn, as control.rs delivers it. */
async function fromGeneral(
  app: AppShell,
  page: Page,
  requestId: string,
  action: string,
  input: Record<string, unknown>,
) {
  const id = `control-${requestId}`;
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId: GENERAL.data.sessionId,
    requestId,
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
  const response = await page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!.response,
    id,
  );
  if (!response.ok) throw new Error(`${action} failed: ${response.error}`);
  return response.result;
}

async function send(app: AppShell, page: Page, message: Message) {
  await page.evaluate((id) => {
    window.nextRowId = id;
  }, message.id);
  return fromGeneral(app, page, `send-${message.id}`, "messages.send", {
    message: message.body,
    send_to: FORK.id,
    wanting_reply: false,
    intent: "new",
  });
}

/** The prompts omp got for the Fork Orchestrator's chat since this page loaded. */
const forkPrompts = (page: Page) =>
  page.evaluate(
    (chat) =>
      (window.ompLines ?? [])
        .filter((line) => line.sessionId === chat && line.type === "prompt")
        .map((line) => line.message ?? ""),
    FORK.data.sessionId,
  );

test("ship 6: a Mono queue the relaunch paused is reported to its senders and in monos.get and monos.list", async ({
  app,
  page,
}) => {
  await mountMonos(app, page);
  await openMono(page, FORK);
  await page
    .getByPlaceholder(`Message ${FORK.data.name}`)
    .fill("Run the ship 6 swap.");
  await page.getByPlaceholder(`Message ${FORK.data.name}`).press("Enter");
  await expect.poll(() => forkPrompts(page)).toHaveLength(1);

  await openMono(page, GENERAL);
  for (const message of db.beforeQuit)
    expect(
      await send(app, page, message),
      "a message for a busy Mono waits for its turn end, as before",
    ).toEqual({
      message_id: message.id,
      status: "delivered",
      delivery: "queue",
      hop: 0,
    });
  expect(
    await fromGeneral(app, page, "get-before", "monos.get", {
      monoId: FORK.id,
    }),
  ).toMatchObject({
    busy: true,
    queued: db.beforeQuit.length,
    model: "omp:default",
  });

  await app.restart();
  await openMono(page, GENERAL);
  expect(
    await send(app, page, db.afterRelaunch),
    "General is told the message waits for a Resume",
  ).toEqual({
    message_id: db.afterRelaunch.id,
    status: "delivered",
    delivery: "paused",
    hop: 0,
  });
  expect(
    await fromGeneral(app, page, "get-after", "monos.get", {
      monoId: FORK.id,
    }),
  ).toMatchObject({ busy: false, queuePaused: true, model: "omp:default" });
  const { monos } = (await fromGeneral(
    app,
    page,
    "list-after",
    "monos.list",
    {},
  )) as {
    monos: { id: string; queuePaused: boolean }[];
  };
  expect(
    monos.map(({ id, queuePaused }) => [id, queuePaused]),
    "only the paused Mono's queue is reported paused",
  ).toEqual([
    [FORK.id, true],
    [GENERAL.id, false],
  ]);
  expect(
    await forkPrompts(page),
    "the paused queue holds the interrupted turn's Continue and every message",
  ).toEqual([]);
});

test("a Mono whose saved model the catalog does not list still runs the messages it is sent", async ({
  app,
  page,
}) => {
  await mountMonos(app, page, "omp:anthropic/claude-gone-1");
  await openMono(page, GENERAL);
  const { harnesses } = (await fromGeneral(
    app,
    page,
    "models",
    "models.list",
    {},
  )) as {
    harnesses: { id: string; models: { id: string }[] }[];
  };
  expect(
    harnesses.find(({ id }) => id === "omp")!.models.map(({ id }) => id),
  ).not.toContain("omp:anthropic/claude-gone-1");

  expect(await send(app, page, db.afterRelaunch)).toMatchObject({
    delivery: "submit",
  });
  await expect
    .poll(() => forkPrompts(page))
    .toEqual([expect.stringContaining(db.afterRelaunch.body.slice(0, 40))]);
  expect(
    await page.evaluate(
      (chat) =>
        window
          .appBackend!.calls.filter(
            ({ command, args }) =>
              command === "harness_spawn" &&
              (args as { sessionId: string }).sessionId === chat,
          )
          .map(({ args }) => (args as { args: string[] }).args),
      FORK.data.sessionId,
    ),
    "omp gets the saved model and reports any failure as a turn error",
  ).toEqual([expect.arrayContaining(["--model", "anthropic/claude-gone-1"])]);
});
