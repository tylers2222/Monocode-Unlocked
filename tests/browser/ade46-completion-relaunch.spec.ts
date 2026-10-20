import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-46. A Mono is told when a session it monitors stops. The ship 5 restart
// (10-20 17:19) quit the app while the Orchestrator monitored two running
// sessions; the relaunch continued both turns and the Orchestrator was never
// told they ended. `ade46-ship5.realdb.json` holds that moment, reduced from a
// .backup copy of the MonoCode 2 database: the real Mono, sessions, request
// ids, request order and which requests asked to be notified. Each case quits
// through `app.restart()` (quit_commit, quit_ready, a new page on the saved
// store).

type Fixture = {
  mono: {
    id: string;
    data: {
      name: string;
      mascot: string;
      projects: string[];
      sessionId: string;
    };
  };
  monoChat: SessionSummary;
  sessions: SessionSummary[];
  requests: {
    sessionId: string;
    requestId: string;
    prompt: string;
    notifyOnComplete: boolean;
  }[];
};
const db = JSON.parse(
  readFileSync(
    new URL("./fixtures/ade46-ship5.realdb.json", import.meta.url),
    "utf8",
  ),
) as Fixture;
const PROJECT = db.mono.data.projects[0];
const CHAT = db.monoChat.id;
const MONO = db.mono.data.name;
const [STARTED, SENT] = db.sessions.map((session) => session.id);
const NOTICE = "MonoCode completion notification";

type OmpLine = {
  sessionId: string;
  request: { id?: string; type?: string; message?: string };
};
type ControlResponse = { ok: boolean; error?: string };
declare global {
  interface Window {
    ompLines?: OmpLine[];
    controlReplies?: { id: string; response: ControlResponse }[];
  }
}

/**
 * Omp in RPC mode accepts every request. `get_state` names a provider
 * session, which is what lets a quit turn continue after a relaunch; a
 * session whose id ends in `-offline` has none.
 */
const handlers: MountOptions["handlers"] = {
  harness_resolve_omp: () => ({ path: "/usr/local/bin/omp" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as {
      id?: string;
      type?: string;
    };
    (window.ompLines ??= []).push({ sessionId: sessionId as string, request });
    if (request.id)
      void window.appBackend!.emit("harness-stdout", {
        sessionId,
        line: JSON.stringify({
          type: "response",
          id: request.id,
          command: request.type,
          success: true,
          data:
            request.type === "get_state" &&
            !String(sessionId).endsWith("-offline")
              ? { sessionId: `provider-${sessionId}` }
              : {},
        }),
      });
    return null;
  },
  omp_session_interjections: () => [],
  omp_active_assistant_texts: () => [],
  session_checkpoint_keep: () => null,
  control_owns_session: () => true,
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
  control_reply: (_store, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  // session_store.rs replace_in_flight: what a relaunch resumes.
  session_set_in_flight: (store, { sessions }) => {
    store.inFlight = sessions as { sessionId: string; cwd: string }[];
    return null;
  },
  // The real project folder is named monocode, which the app warns about at boot.
  "plugin:dialog|message": () => null,
};

/** Session ids as the lane B output names them: the last 8 characters. */
const at = (id: string) => id.slice(-8);

async function mountOrchestrator(
  app: AppShell,
  page: Page,
  extra: SessionSummary[] = [],
) {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        db.monoChat,
        ...db.sessions.filter(({ id }) => id === SENT),
        ...extra,
      ].map((session) => ({ ...session, createdAt: 1, updatedAt: 1 })),
      monos: [{ id: db.mono.id, role: "mono", data: db.mono.data }],
    },
    { handlers },
  );
  await openMono(page);
}

async function openMono(page: Page) {
  await page.getByRole("button", { name: new RegExp(`^${MONO}, `) }).click();
  await expect(page.getByPlaceholder(`Message ${MONO}`)).toBeVisible();
}

/** One `monocode app` call from the Orchestrator's chat, as control.rs delivers it. */
async function call(
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
    sessionId: CHAT,
    requestId,
    action,
    input,
    turnId: "turn-1",
  });
  await expect
    .poll(() =>
      page.evaluate((id) => {
        const reply = window.controlReplies?.find((reply) => reply.id === id);
        return reply && (reply.response.ok ? "ok" : reply.response.error);
      }, id),
    )
    .toBe("ok");
}

/** The prompts omp got for `sessionId` since this page loaded. */
const prompts = (page: Page, sessionId: string) =>
  page.evaluate(
    (sessionId) =>
      (window.ompLines ?? [])
        .filter(
          (line) =>
            line.sessionId === sessionId && line.request.type === "prompt",
        )
        .map((line) => line.request.message ?? ""),
    sessionId,
  );

/** The sessions each completion notice the Mono's chat was given reports, in order. */
async function notices(page: Page) {
  return (await prompts(page, CHAT))
    .filter((text) => text.includes(NOTICE))
    .map((text) =>
      [...text.matchAll(/"sessionId":"([^"]+)"/g)].map(([, id]) => at(id)),
    );
}

/** The user sends the Mono a message; its turn runs until `endTurn(CHAT)`. */
async function monoTurn(page: Page, text: string) {
  const before = (await prompts(page, CHAT)).length;
  await page.getByPlaceholder(`Message ${MONO}`).fill(text);
  await page.getByPlaceholder(`Message ${MONO}`).press("Enter");
  await expect
    .poll(async () => (await prompts(page, CHAT)).length)
    .toBe(before + 1);
}

/** Omp ends `sessionId`'s running turn with `text`. */
async function endTurn(app: AppShell, sessionId: string, text: string) {
  for (const line of [
    {
      type: "message_update",
      assistantMessageEvent: { type: "text_delta", delta: text },
    },
    { type: "agent_end", willRetry: false },
  ])
    await app.emit("harness-stdout", { sessionId, line: JSON.stringify(line) });
}

test("the ship 5 restart: each monitored session the quit cut reports once when its continued turn ends", async ({
  app,
  page,
}) => {
  await mountOrchestrator(app, page);
  for (const request of db.requests) {
    // sessions.start names the session it makes app-<chat>-<request id>.
    const started = request.sessionId === `app-${CHAT}-${request.requestId}`;
    await call(
      app,
      page,
      request.requestId,
      started ? "sessions.start" : "sessions.send",
      started
        ? {
            // The fake model list has only omp's default; the real call
            // named omp:anthropic/claude-opus-5-5.
            harness: "omp",
            model: "omp:default",
            prompt: request.prompt,
            project: PROJECT,
            notifyOnComplete: request.notifyOnComplete,
          }
        : {
            sessionId: request.sessionId,
            prompt: request.prompt,
            project: PROJECT,
            whenBusy: "steer",
            notifyOnComplete: request.notifyOnComplete,
          },
    );
  }
  await expect.poll(() => prompts(page, STARTED)).toHaveLength(1);
  await expect.poll(() => prompts(page, SENT)).toHaveLength(1);

  await app.restart();
  await expect
    .poll(() => prompts(page, STARTED))
    .toEqual(["Continue from where you left off."]);
  await expect
    .poll(() => prompts(page, SENT))
    .toEqual(["Continue from where you left off."]);

  await endTurn(app, STARTED, "M-10 #117 wording fix done.");
  await expect.poll(() => notices(page)).toEqual([[at(STARTED)]]);
  await endTurn(app, CHAT, "Noted.");
  await endTurn(app, SENT, "ADE-58 repro done.");
  await expect.poll(() => notices(page)).toEqual([[at(STARTED)], [at(SENT)]]);
});

// The Mono was told of S's end through its first request; the steer it sent
// S from a later turn ended with that same turn and waits, unreported, in the
// later group, which also waits on S2. The quit must not keep that steer.
test("a result the Mono was told of before the quit is not reported again", async ({
  app,
  page,
}) => {
  const [S2, S] = db.sessions;
  await mountOrchestrator(app, page, [S2]);
  await monoTurn(page, "Start the ADE-58 repro.");
  await call(app, page, "first", "sessions.send", {
    sessionId: S.id,
    prompt: "Reproduce ADE-58.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await endTurn(app, CHAT, "Started.");
  await monoTurn(page, "Ship 5 is close; warn the workers.");
  await call(app, page, "steer", "sessions.send", {
    sessionId: S.id,
    prompt: "Stop at a safe point.",
    project: PROJECT,
    whenBusy: "steer",
    notifyOnComplete: true,
  });
  await call(app, page, "second", "sessions.send", {
    sessionId: S2.id,
    prompt: "Fix the M-10 wording.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await endTurn(app, S.id, "Paused at a safe point.");
  await endTurn(app, CHAT, "Warned.");
  await expect.poll(() => notices(page)).toEqual([[at(S.id)]]);
  // A quit while the Mono's turn still runs pauses its queue, which would
  // hold the later notice; the case is about the groups, so quit idle.
  const busy = page.getByRole("button", {
    name: new RegExp(`^${MONO}, (Thinking|Working),`),
  });
  await expect(busy).toBeVisible();
  await endTurn(app, CHAT, "Noted.");
  await expect(busy).toHaveCount(0);

  await app.restart();
  await expect
    .poll(() => prompts(page, S2.id))
    .toEqual(["Continue from where you left off."]);
  await endTurn(app, S2.id, "M-10 wording fixed.");
  await expect.poll(() => notices(page)).toEqual([[at(S2.id)]]);
});

// A session whose turn the quit cut, and which the relaunch cannot continue
// (no provider session to resume), ended while the app was down.
test("a monitored session that ended while the app was down is reported once after the relaunch", async ({
  app,
  page,
}) => {
  const offline = {
    ...db.sessions[0],
    id: `${db.sessions[0].id}-offline`,
    providerSessionId: undefined,
  };
  await mountOrchestrator(app, page, [offline]);
  await call(app, page, "offline", "sessions.send", {
    sessionId: offline.id,
    prompt: "Fix the M-10 wording.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await expect.poll(() => prompts(page, offline.id)).toHaveLength(1);

  await app.restart();
  await expect.poll(() => notices(page)).toEqual([[at(offline.id)]]);
  expect(
    (await prompts(page, CHAT)).find((text) => text.includes(NOTICE)),
  ).toContain("Turn interrupted when MonoCode quit.");
  await endTurn(app, CHAT, "Noted.");
  await openMono(page);
  await monoTurn(page, "Anything else?");
  expect(await notices(page)).toEqual([[at(offline.id)]]);
  expect(await prompts(page, offline.id)).toEqual([]);
});
