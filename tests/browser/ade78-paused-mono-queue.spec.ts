import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";
import { bridgeOmp, type ModelReply, needsOmp, ompHandlers } from "./app/ompBridge";

// ADE-78. The ship 6 swap (MonoCode 2, 2026-10-21 08:49) quit the app while
// the Fork Orchestrator ran a turn with General's messages queued behind it.
// The relaunch paused that queue ("Paused when MonoCode quit"), so every
// message General sent for the next 29 hours waited for a Resume, while
// messages.send answered `delivery: "queue"` and monos.get looked like any
// queue. `ade78-ship6.realdb.json` keeps the state shape of the .backup the
// swap took at 08:48:51 and of the database after it: both Monos, their chats
// on omp:default, the queued ids and the queue status. Names, paths, titles
// and message text are made up. The model was not the cause: a
// chat on a model id the catalog does not list still runs, so no send to it
// is held.
//
// ADE-108 then makes the app wake that queue itself: a pause the quit or a
// failed turn made lifts with no click, a few times in a row at most, and a
// pause the user made still waits for Resume. ADE-111 is the stall that
// paused the same chat on 2026-10-23: omp retried it in the same turn, which
// ended well, but the app still paused the queue for it.

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
  // gate is tested in mono_gate.rs. Rows stay in sessionStorage, as Rust
  // keeps them in monocode.db, so a relaunch still hands them off.
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
    const rows = JSON.parse(sessionStorage.getItem("ade78-rows") ?? "{}");
    sessionStorage.setItem(
      "ade78-rows",
      JSON.stringify({ ...rows, [row.id!]: row }),
    );
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
  mono_message_assign_slot: (_store, { id, turnId, kind }) => ({
    message: {
      ...JSON.parse(sessionStorage.getItem("ade78-rows") ?? "{}")[id as string],
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
  {
    forkModel = FORK.chat.summary.model,
    omp = false,
  }: { forkModel?: string; omp?: boolean } = {},
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
    { handlers: omp ? { ...handlers, ...ompHandlers } : handlers },
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

let calls = 0;

/** The Fork Orchestrator as monos.get shows it to General. */
const forkState = (app: AppShell, page: Page) =>
  fromGeneral(app, page, `get-${++calls}`, "monos.get", { monoId: FORK.id });

/** Which Monos monos.list reports with a paused queue. */
async function pausedQueues(app: AppShell, page: Page) {
  const { monos } = (await fromGeneral(
    app,
    page,
    `list-${++calls}`,
    "monos.list",
    {},
  )) as { monos: { id: string; queuePaused: boolean }[] };
  return monos.map(({ id, queuePaused }) => [id, queuePaused]);
}

/** A real omp starts, and a turn reaches its model, in a few seconds on CI. */
const OMP = { timeout: 30_000 };

/**
 * Real omp for every chat, on a local model that `reply` answers from the
 * text of the last user message omp sends it. Title jobs run without tools.
 * Returns that text for each agent request, in order.
 */
async function realOmp(
  page: Page,
  app: AppShell,
  reply: (prompt: string) => ModelReply | Promise<ModelReply>,
  env: NodeJS.ProcessEnv = {},
): Promise<string[]> {
  const prompts: string[] = [];
  await bridgeOmp(page, app, {
    dir: mkdtempSync(join(tmpdir(), "ade108-")),
    env,
    model: ({ messages, tools }) => {
      if (tools.length === 0) return { text: "Fork chat" };
      const content = messages.findLast(({ role }) => role === "user")?.content;
      const prompt =
        typeof content === "string"
          ? content
          : (content as { text?: string }[]).map(({ text }) => text ?? "").join("");
      prompts.push(prompt);
      return reply(prompt);
    },
  });
  return prompts;
}

const sent = (prompts: string[], message: Message) =>
  prompts.some((prompt) => prompt.includes(message.body.slice(0, 40)));

// TEMPORARY mutant run for PR #341 (removed in the next commit). Each mutant
// rewrites one rule in the module Vite serves; the scenario named in `kills`
// must then fail (expected failure). A mutant with no `kills` runs every
// scenario, and each one that passes shows as an unexpected pass: a survivor.
type Mutant = { name: string; path: string; from: RegExp; to: string; kills?: string };
const QUEUE = "/src/features/sessions/model/messageQueue.ts";
const MUTANTS: Mutant[] = [
  { name: "M1 ADE-111 reverted: a healthy end keeps the held error", path: "/src/integrations/harness/providers/pi/piFamily.ts", from: /if \(turnError !== (?:void 0|undefined)\)/, to: "if (turnError != null)", kills: "ADE-111" },
  { name: "M2 the quit pause is not woken", path: QUEUE, from: /!!queuePauseNotice\(session\)/, to: "false", kills: "ship 6" },
  { name: "M3 the error pause is not woken", path: QUEUE, from: /session\.queuePause === "error" \|\|/, to: "false ||", kills: "ADE-108" },
  { name: "M4 a pause the user made is woken", path: QUEUE, from: /\(session\.queuePause === "error" \|\| !!queuePauseNotice\(session\)\)/, to: "true", kills: "ship 6" },
  { name: "M5 no wake limit", path: QUEUE, from: /\(session\.queueWakes \?\? 0\) < QUEUE_WAKE_LIMIT/, to: "true", kills: "ADE-108" },
  { name: "M6 a good turn does not clear the count", path: "/src/app/App.tsx", from: /queueWakes: (?:void 0|undefined)/, to: "queueWakes: ended.queueWakes", kills: "ADE-108" },
  { name: "M7 project sessions wake too", path: QUEUE, from: /mono &&\s*!session\.busy/, to: "!session.busy" },
  { name: "M8 a busy session wakes", path: QUEUE, from: /!session\.busy &&\s*!session\.usageLimit/, to: "!session.usageLimit" },
  { name: "M9 a usage-limited session wakes", path: QUEUE, from: /!session\.usageLimit &&\s*session\.queueStatus/, to: "session.queueStatus" },
];

for (const mutant of [undefined, ...MUTANTS]) test.describe(mutant?.name ?? "on a real omp", () => {
  needsOmp();
  test.setTimeout(120_000);
  if (mutant)
    test.beforeEach(async ({ page, browserName }, testInfo) => {
      test.skip(browserName !== "chromium", "mutants run on one browser");
      test.skip(!!mutant.kills && !testInfo.title.startsWith(mutant.kills), `${mutant.kills} kills this mutant`);
      const source = await (await page.request.get(mutant.path)).text();
      test.skip(!mutant.from.test(source), `the pattern does not match ${mutant.path}`);
      testInfo.fail();
      await page.route(`**${mutant.path}*`, async (route) => {
        const response = await route.fetch();
        await route.fulfill({ response, body: (await response.text()).replace(mutant.from, mutant.to) });
      });
    });

  test("ship 6: the queue the relaunch paused sends with no click; a queue the user paused waits for Resume and is reported paused", async ({
    app,
    page,
  }) => {
    const opener = "Run the ship 6 swap.";
    // Made-up messages; ids are base32, as Rust mints them.
    const later = { id: "m_wakelaterqqq", body: "Queue this behind the running turn." };
    const last = { id: "m_wakelastqqqq", body: "Send this when the queue resumes." };
    const again = { id: "m_wakeagainqqq", body: "One more after the second relaunch." };
    // The quit cuts the opener's turn short, and the user stops the next one.
    const prompts = await realOmp(page, app, (prompt) =>
      prompt.includes(opener) || sent([prompt], db.afterRelaunch)
        ? new Promise<ModelReply>(() => {})
        : { text: "Done." },
    );
    await mountMonos(app, page, { omp: true });
    await openMono(page, FORK);
    await page.getByPlaceholder(`Message ${FORK.data.name}`).fill(opener);
    await page.getByPlaceholder(`Message ${FORK.data.name}`).press("Enter");
    await expect.poll(() => prompts.length, OMP).toBe(1);

    await openMono(page, GENERAL);
    for (const queued of db.beforeQuit)
      expect(
        await send(app, page, queued),
        "a message for a busy Mono waits for its turn end, as before",
      ).toEqual({
        message_id: queued.id,
        status: "delivered",
        delivery: "queue",
        hop: 0,
      });
    expect(await forkState(app, page)).toMatchObject({
      busy: true,
      queued: db.beforeQuit.length,
      model: "omp:default",
    });

    // Rust stops the harnesses before the app exits.
    await app.restart(() =>
      page.evaluate((chat) => window.ompKill(chat), FORK.data.sessionId),
    );
    await openMono(page, GENERAL);
    await expect
      .poll(
        () => db.beforeQuit.every((queued) => sent(prompts, queued)),
        { message: "the queue the relaunch paused sends with no click", ...OMP },
      )
      .toBe(true);
    await expect
      .poll(() => forkState(app, page), OMP)
      .toMatchObject({ busy: false, queued: 0, queuePaused: false });

    expect(await send(app, page, db.afterRelaunch)).toMatchObject({
      delivery: "submit",
    });
    await expect.poll(() => sent(prompts, db.afterRelaunch), OMP).toBe(true);
    expect(await send(app, page, later)).toMatchObject({ delivery: "queue" });
    await openMono(page, FORK);
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    await openMono(page, GENERAL);
    expect(
      await send(app, page, last),
      "General is told the message waits for a Resume",
    ).toEqual({
      message_id: last.id,
      status: "delivered",
      delivery: "paused",
      hop: 0,
    });
    expect(await forkState(app, page)).toMatchObject({
      busy: false,
      queued: 2,
      queuePaused: true,
    });
    expect(
      await pausedQueues(app, page),
      "only the paused Mono's queue is reported paused",
    ).toEqual([
      [FORK.id, true],
      [GENERAL.id, false],
    ]);

    await app.restart();
    await openMono(page, GENERAL);
    expect(
      await send(app, page, again),
      "the pause the user made outlives the relaunch",
    ).toMatchObject({ delivery: "paused" });
    await openMono(page, FORK);
    expect(
      [later, last, again].filter((queued) => sent(prompts, queued)),
      "nothing the user paused is sent before Resume",
    ).toEqual([]);
    await page.getByRole("button", { name: "Resume", exact: true }).click();
    await expect
      .poll(() => [later, last, again].every((queued) => sent(prompts, queued)), OMP)
      .toBe(true);
  });

  test("ADE-111: a stream stall omp retries in the same turn leaves the queue sending", async ({
    app,
    page,
  }) => {
    const opener = "Check the stalled build.";
    const queued = { id: "m_stallqueuedq", body: "Read the logs once the turn ends." };
    const release = Promise.withResolvers<void>();
    let stalled = false;
    const prompts = await realOmp(
      page,
      app,
      async (prompt) => {
        if (!prompt.includes(opener)) return { text: "Done." };
        if (!stalled) {
          stalled = true;
          return { stall: true };
        }
        await release.promise;
        return { text: "The build is fine." };
      },
      // As the Anthropic stream stalled on 2026-10-23, but in half a second.
      { PI_STREAM_IDLE_TIMEOUT_MS: "500" },
    );
    await mountMonos(app, page, { omp: true });
    await openMono(page, FORK);
    await page.getByPlaceholder(`Message ${FORK.data.name}`).fill(opener);
    await page.getByPlaceholder(`Message ${FORK.data.name}`).press("Enter");
    await expect
      .poll(() => prompts.filter((prompt) => prompt.includes(opener)).length, {
        message: "omp ends the stalled attempt and retries it in the same turn",
        ...OMP,
      })
      .toBe(2);

    await openMono(page, GENERAL);
    expect(await send(app, page, queued)).toMatchObject({ delivery: "queue" });
    release.resolve();
    await expect
      .poll(() => sent(prompts, queued), {
        message: "the message queued behind the stalled turn runs at its end, with no Resume",
        ...OMP,
      })
      .toBe(true);
    await expect
      .poll(() => forkState(app, page), OMP)
      .toMatchObject({ busy: false, queued: 0, queuePaused: false });
    await openMono(page, FORK);
    await expect(
      page.getByText(/stream stalled/),
      "the turn that recovered shows no error",
    ).toHaveCount(0);
  });

  test("ADE-108: a Mono whose turns all fail is woken three times, then waits for Resume; a turn that ends well lets it wake again", async ({
    app,
    page,
  }) => {
    // Made-up messages A to I; the model refuses A to E and H.
    const steps = [..."abcdefghi"].map((step) => ({
      id: `m_wakeupstepq${step}`,
      body: `Run check ${step.toUpperCase()} now.`,
    }));
    const failing = new Set([0, 1, 2, 3, 4, 7]);
    // A and H hold until the messages behind them are queued.
    const holds = new Map([0, 7].map((step) => [step, Promise.withResolvers<void>()]));
    const which = (prompt: string) =>
      steps.findIndex((step) => sent([prompt], step));
    const prompts = await realOmp(page, app, async (prompt) => {
      await holds.get(which(prompt))?.promise;
      return failing.has(which(prompt))
        ? { refuse: "The model is down." }
        : { text: "Done." };
    });
    await mountMonos(app, page, { omp: true });
    await openMono(page, GENERAL);
    expect(await send(app, page, steps[0])).toMatchObject({ delivery: "submit" });
    await expect.poll(() => prompts.length, OMP).toBe(1);
    for (const step of steps.slice(1, 6))
      expect(await send(app, page, step)).toMatchObject({ delivery: "queue" });

    holds.get(0)!.resolve();
    await expect
      .poll(() => forkState(app, page), {
        message: "each failed turn pauses the queue and the app wakes it, three times",
        ...OMP,
      })
      .toMatchObject({ busy: false, queued: 2, queuePaused: true });
    expect(prompts.map(which), "the first turn and three woken turns").toEqual([
      0, 1, 2, 3,
    ]);

    await openMono(page, FORK);
    await page.getByRole("button", { name: "Resume", exact: true }).click();
    await expect
      .poll(() => forkState(app, page), OMP)
      .toMatchObject({ busy: false, queued: 1, queuePaused: true });
    await openMono(page, GENERAL);
    expect(
      await send(app, page, steps[6]),
      "the turn Resume ran failed too, and the app does not wake the queue again",
    ).toMatchObject({ delivery: "paused" });
    expect(prompts.map(which), "Resume sends one more turn").toEqual([0, 1, 2, 3, 4]);

    await openMono(page, FORK);
    await page.getByRole("button", { name: "Resume", exact: true }).click();
    await expect
      .poll(() => forkState(app, page), OMP)
      .toMatchObject({ busy: false, queued: 0, queuePaused: false });
    await openMono(page, GENERAL);
    expect(await send(app, page, steps[7])).toMatchObject({ delivery: "submit" });
    await expect.poll(() => prompts.map(which).at(-1), OMP).toBe(7);
    expect(await send(app, page, steps[8])).toMatchObject({ delivery: "queue" });
    holds.get(7)!.resolve();
    await expect
      .poll(() => prompts.map(which), {
        message: "after turns that ended well, the next failed turn's pause is woken again",
        ...OMP,
      })
      .toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8]);
    await expect
      .poll(() => forkState(app, page), OMP)
      .toMatchObject({ busy: false, queued: 0, queuePaused: false });
  });
});

test("a Mono whose saved model the catalog does not list still runs the messages it is sent", async ({
  app,
  page,
}) => {
  await mountMonos(app, page, { forkModel: "omp:anthropic/claude-gone-1" });
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
