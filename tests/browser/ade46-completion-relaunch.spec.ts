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
const CONTINUE = "Continue from where you left off.";

type OmpLine = {
  sessionId: string;
  request: { id?: string; type?: string; message?: string };
};
type ControlResponse = { ok: boolean; error?: string };
declare global {
  interface Window {
    ompLines?: OmpLine[];
    controlReplies?: { id: string; response: ControlResponse }[];
    projectLocation?: Promise<void>;
    releaseProjectLocation?: () => void;
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
  // session_store.rs replace_in_flight: what a relaunch resumes. A reload
  // writes it last; `ade46.keepAtInFlight` keeps the store as that write
  // leaves it, as the Rust database outlives the page.
  session_set_in_flight: (store, { sessions }) => {
    store.inFlight = sessions as { sessionId: string; cwd: string }[];
    if (sessionStorage.getItem("ade46.keepAtInFlight")) {
      sessionStorage.removeItem("ade46.keepAtInFlight");
      window.appBackend!.keepStore();
    }
    return null;
  },
  // The user answers OK to each dialog: the warning the app shows at boot
  // because the real project folder is named monocode, and the busy-window
  // close, whose `ask` compares the answer with its OK label.
  "plugin:dialog|message": (_store, { message }) =>
    String(message).startsWith("Close this window") ? "Close window" : "Ok",
  // fs.rs resolve_project_location. Once `holdLocation` is set, the check
  // waits for `releaseProjectLocation`, as the real one takes about a second
  // after a relaunch (ADE-60). sessionStorage outlives the reload.
  resolve_project_location: async (_store, { path }) => {
    if (sessionStorage.getItem("ade46.holdLocation")) {
      if (!window.projectLocation) {
        const { promise, resolve } = Promise.withResolvers<void>();
        window.projectLocation = promise;
        window.releaseProjectLocation = resolve;
      }
      await window.projectLocation;
    }
    return { path, identity: path };
  },
  mono_chat_keep_alive: () => false,
  hide_window: () => null,
  destroy_window: () => null,
};

const keepAtInFlight = (page: Page) =>
  page.evaluate(() => sessionStorage.setItem("ade46.keepAtInFlight", "1"));

const holdLocation = (page: Page) =>
  page.evaluate(() => sessionStorage.setItem("ade46.holdLocation", "1"));

/** Until React has committed, run its effects and their zero-delay timers. */
const rendered = (page: Page) =>
  page.evaluate(() => {
    const { promise, resolve } = Promise.withResolvers<void>();
    requestAnimationFrame(() =>
      requestAnimationFrame(() => setTimeout(resolve)),
    );
    return promise;
  });

/**
 * The window closed (its close sent `closed`); the app opens again on the
 * store it left, with the location check released.
 */
async function reopenAfter(page: Page, closed: string) {
  await expect
    .poll(() =>
      page.evaluate(
        (closed) =>
          window.appBackend!.calls.some(({ command }) => command === closed),
        closed,
      ),
    )
    .toBe(true);
  await page.evaluate(() => {
    sessionStorage.removeItem("ade46.holdLocation");
    window.appBackend!.keepStore();
  });
  await reopen(page);
}

async function reopen(page: Page) {
  await page.reload();
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible({
    timeout: 30_000,
  });
}

/** Session ids as the lane B output names them: the last 8 characters. */
const at = (id: string) => id.slice(-8);

async function mountOrchestrator(
  app: AppShell,
  page: Page,
  extra: SessionSummary[] = [],
  platform: MountOptions["platform"] = "macos",
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
    { handlers, platform },
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

  // As at ship 5 (ADE-60): SENT works in a worktree, so its Continue opens at
  // once and the app renders again while STARTED's Continue waits on its
  // project's location check. STARTED's Continue is asked for twice; the
  // second copy is refused on the turn the first opens.
  await holdLocation(page);
  await app.restart();
  await expect.poll(() => prompts(page, SENT)).toEqual([CONTINUE]);
  await expect
    .poll(() => page.evaluate(() => !!window.releaseProjectLocation))
    .toBe(true);
  await rendered(page);
  await page.evaluate(() => window.releaseProjectLocation!());
  await expect.poll(() => prompts(page, STARTED)).toEqual([CONTINUE]);
  await rendered(page);
  expect(await notices(page)).toEqual([]);

  await endTurn(app, STARTED, "M-10 #117 wording fix done.");
  await expect.poll(() => notices(page)).toEqual([[at(STARTED)]]);
  expect(
    (await prompts(page, CHAT)).find((text) => text.includes(NOTICE)),
  ).toContain("M-10 #117 wording fix done.");
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
  await expect.poll(() => prompts(page, S2.id)).toEqual([CONTINUE]);
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

// Each save path that ends this window keeps the waiting group: a reload, a
// close with nothing running, and closing a busy window on Linux.
test("a reload keeps the waiting group", async ({ app, page }) => {
  await mountOrchestrator(app, page);
  await call(app, page, "reload", "sessions.send", {
    sessionId: SENT,
    prompt: "Reproduce ADE-58.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await expect.poll(() => prompts(page, SENT)).toHaveLength(1);

  await keepAtInFlight(page);
  await reopen(page);
  await expect.poll(() => prompts(page, SENT)).toEqual([CONTINUE]);
  await endTurn(app, SENT, "ADE-58 repro done.");
  await expect.poll(() => notices(page)).toEqual([[at(SENT)]]);
});

test("closing the window with nothing running keeps the waiting group", async ({
  app,
  page,
}) => {
  await mountOrchestrator(app, page, [db.sessions[0]]);
  await call(app, page, "idle-close", "sessions.send", {
    sessionId: STARTED,
    prompt: "Fix the M-10 wording.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await expect.poll(() => prompts(page, STARTED)).toHaveLength(1);
  // After a relaunch the Continue waits on the location check, so the
  // window runs nothing while the Mono still waits on STARTED.
  await holdLocation(page);
  await app.restart();
  await expect
    .poll(() => page.evaluate(() => !!window.releaseProjectLocation))
    .toBe(true);

  await app.emit("tauri://close-requested", {});
  await reopenAfter(page, "destroy_window");
  await expect.poll(() => prompts(page, STARTED)).toEqual([CONTINUE]);
  await endTurn(app, STARTED, "M-10 wording fixed.");
  await expect.poll(() => notices(page)).toEqual([[at(STARTED)]]);
});

test("closing a busy window on Linux keeps the waiting group", async ({
  app,
  page,
}) => {
  await mountOrchestrator(app, page, [], "linux");
  await call(app, page, "busy-close", "sessions.send", {
    sessionId: SENT,
    prompt: "Reproduce ADE-58.",
    project: PROJECT,
    notifyOnComplete: true,
  });
  await expect.poll(() => prompts(page, SENT)).toHaveLength(1);

  // The close stops SENT's turn after it saves, and this page lives on;
  // keep the store as the save left it, as the Rust database does.
  await keepAtInFlight(page);
  await app.emit("tauri://close-requested", {});
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("ade46.keepAtInFlight")),
    )
    .toBeNull();
  await reopen(page);
  await expect.poll(() => prompts(page, SENT)).toEqual([CONTINUE]);
  await endTurn(app, SENT, "ADE-58 repro done.");
  await expect.poll(() => notices(page)).toEqual([[at(SENT)]]);
});
