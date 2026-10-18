import type { Page } from "@playwright/test";
import { TURN_GUARD_STEER } from "../../src/features/monos/model/gigaWorkspace";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

const HOME = "/Users/test";
const PROJECT = `${HOME}/acme`;
const GIGA_CHAT = "giga-chat";
const MONO_CHAT = "mono-chat";
const PROJECT_SESSION = "project-session";
const MINUTE = 60_000;

const chat = (id: string, cwd: string, title: string): SessionSummary => ({
  id,
  cwd,
  harness: "pi",
  model: "pi:anthropic/claude-sonnet-4-5",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

/**
 * Pi in RPC mode (piFamily.ts). Every request is answered at once except a
 * prompt, which the test accepts with `acceptPrompt`, and a steer while
 * `window.piRejectsSteers` is set, whose write fails.
 */
const fakePi: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id: string; type: string };
    if (request.type === "prompt") return null;
    if (request.type === "steer" && window.piRejectsSteers)
      throw "pi stdin is closed";
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
};

declare global {
  interface Window {
    piRejectsSteers?: boolean;
  }
}

type PiRequest = { id: string; type: string; message?: string };

async function piRequests(page: Page, sessionId: string, type: string) {
  const lines = await page.evaluate(
    (sessionId) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_write" &&
            (args as { sessionId: string }).sessionId === sessionId,
        )
        .map(({ args }) => (args as { line: string }).line),
    sessionId,
  );
  return lines
    .map((line) => JSON.parse(line) as PiRequest)
    .filter((request) => request.type === type);
}

const steers = (page: Page, sessionId: string) =>
  piRequests(page, sessionId, "steer");

/** Pi's answer to the last prompt: the turn is accepted and takes steers. */
async function acceptPrompt(page: Page, app: AppShell, sessionId: string) {
  await expect
    .poll(async () => (await piRequests(page, sessionId, "prompt")).length)
    .toBeGreaterThan(0);
  const prompts = await piRequests(page, sessionId, "prompt");
  await app.emit("harness-stdout", {
    sessionId,
    line: JSON.stringify({
      type: "response",
      id: prompts.at(-1)!.id,
      command: "prompt",
      success: true,
    }),
  });
}

async function send(page: Page, chat: string, text: string) {
  const composer = page.getByPlaceholder(`Message ${chat}`);
  await composer.fill(text);
  await composer.press("Enter");
}

/** Time since the first prompt was sent, with the page clock paused. */
function turnClock(page: Page) {
  let now = 0;
  return async (ms: number) => {
    await page.clock.runFor(ms - now);
    now = ms;
  };
}

async function mountGiga(
  page: Page,
  app: AppShell,
  options: Pick<MountOptions, "cannotSteer"> = {},
) {
  await page.clock.install({ time: new Date("2026-10-18T09:00:00Z") });
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        chat(GIGA_CHAT, `${HOME}/MonoCode/gigas/atlas`, "Atlas"),
        chat(MONO_CHAT, HOME, "Pip"),
        chat(PROJECT_SESSION, PROJECT, "Fix the login redirect"),
      ],
      monos: [
        {
          id: "giga-1",
          role: "giga",
          data: {
            name: "Atlas",
            mascot: "ghost",
            gigaSlug: "atlas",
            sessionId: GIGA_CHAT,
            projects: [],
          },
        },
        {
          id: "mono-1",
          role: "mono",
          data: {
            name: "Pip",
            mascot: "mushroom",
            sessionId: MONO_CHAT,
            projects: [PROJECT],
          },
        },
      ],
    },
    { handlers: fakePi, ...options },
  );
}

const ATLAS = { name: "Atlas", sessionId: GIGA_CHAT };
const PIP = { name: "Pip", sessionId: MONO_CHAT };

/** Opens a Mono's chat and sends `text` with the clock paused at 0:00. */
async function startTurn(
  page: Page,
  app: AppShell,
  text: string,
  { name, sessionId } = ATLAS,
) {
  await page.getByRole("button", { name: new RegExp(`^${name}, `) }).click();
  await expect(page.getByPlaceholder(`Message ${name}`)).toBeVisible();
  await page.clock.pauseAt((await page.evaluate(() => Date.now())) + 1_000);
  await send(page, name, text);
  await acceptPrompt(page, app, sessionId);
  return turnClock(page);
}

/** Each saved guard steer: its role and the opener of the turn it joined. */
async function savedSteers(page: Page, sessionId: string) {
  const blocks: Block[] = await page.evaluate(
    (sessionId) => window.appBackend!.store.blocks[sessionId] ?? [],
    sessionId,
  );
  const openers = new Map(
    blocks
      .filter((block) => block.role === "user" && block.turnId)
      .filter((block) => block.text !== TURN_GUARD_STEER)
      .map((block) => [block.turnId, block.text]),
  );
  return blocks
    .filter((block) => block.text === TURN_GUARD_STEER)
    .map((block) => ({ role: block.role, joins: openers.get(block.turnId) }));
}

const shownSteers = (page: Page) =>
  page.getByText(TURN_GUARD_STEER, { exact: true });

test("a Giga turn still running at 3:00 is steered once into that turn", async ({
  app,
  page,
}) => {
  await mountGiga(page, app);
  const at = await startTurn(page, app, "Plan the launch");

  await at(3 * MINUTE - 1_000);
  expect(await steers(page, GIGA_CHAT)).toEqual([]);

  await at(3 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toEqual([
    expect.objectContaining({ message: TURN_GUARD_STEER }),
  ]);
  await expect(shownSteers(page)).toHaveCount(1);
  await expect
    .poll(() => savedSteers(page, GIGA_CHAT))
    .toEqual([{ role: "user", joins: "Plan the launch" }]);

  await at(13 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(1);
});

test("a Giga turn on a harness that cannot steer is not steered", async ({
  app,
  page,
}) => {
  await mountGiga(page, app, { cannotSteer: ["pi"] });
  const at = await startTurn(page, app, "Plan the launch");

  await at(10 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toEqual([]);
  await expect(shownSteers(page)).toHaveCount(0);
});

test("a rejected steer is retried once at 3:30 and its block is removed", async ({
  app,
  page,
}) => {
  await mountGiga(page, app);
  await page.evaluate(() => (window.piRejectsSteers = true));
  const at = await startTurn(page, app, "Plan the launch");

  await at(3 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(1);
  await expect(shownSteers(page)).toHaveCount(0);

  await at(3 * MINUTE + 29_000);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(1);

  await at(3 * MINUTE + 30_000);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(2);
  await expect(shownSteers(page)).toHaveCount(0);

  await at(10 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(2);
  await expect(shownSteers(page)).toHaveCount(0);
  expect(await savedSteers(page, GIGA_CHAT)).toEqual([]);
});

test("a turn that ends early leaves the next turn its own 3 minutes", async ({
  app,
  page,
}) => {
  await mountGiga(page, app);
  const at = await startTurn(page, app, "Plan the launch");

  await at(1 * MINUTE);
  await app.emit("harness-stdout", {
    sessionId: GIGA_CHAT,
    line: JSON.stringify({ type: "agent_end", willRetry: false }),
  });
  await expect(
    page.getByRole("button", { name: /^Atlas, idle/ }),
  ).toBeVisible();

  await at(1.5 * MINUTE);
  await send(page, "Atlas", "Now draft the announcement");
  await acceptPrompt(page, app, GIGA_CHAT);

  await at(4.5 * MINUTE - 1_000);
  expect(await steers(page, GIGA_CHAT)).toEqual([]);

  await at(4.5 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toHaveLength(1);
  await expect
    .poll(() => savedSteers(page, GIGA_CHAT))
    .toEqual([{ role: "user", joins: "Now draft the announcement" }]);
});

test("a Giga turn stopped at 2:00 is not steered", async ({ app, page }) => {
  await mountGiga(page, app);
  const at = await startTurn(page, app, "Plan the launch");

  await at(2 * MINUTE);
  await page.getByPlaceholder("Message Atlas").press("Escape");
  // The app stops on Escape a tick later, once no other surface claimed it.
  await at(2 * MINUTE + 100);
  await expect(
    page.getByRole("button", { name: /^Atlas, idle/ }),
  ).toBeVisible();

  await at(10 * MINUTE);
  expect(await steers(page, GIGA_CHAT)).toEqual([]);
  await expect(shownSteers(page)).toHaveCount(0);
});

test("a plain Mono's chat and a project session are never steered", async ({
  app,
  page,
}) => {
  await mountGiga(page, app);
  await startTurn(page, app, "Review the login flow", PIP);

  await page.getByRole("button", { name: "acme", exact: true }).click();
  await page.getByText("Fix the login redirect").click();
  const composer = page.getByRole("textbox").last();
  await composer.fill("Add a test for the redirect");
  await composer.press("Enter");
  await acceptPrompt(page, app, PROJECT_SESSION);

  // A project session redraws on every animation frame, which runFor fires
  // one by one. One 10-minute jump missed a guard armed for a project
  // session; 30-second jumps fire it at 3:00.
  for (let jump = 0; jump < 20; jump++) await page.clock.fastForward(30_000);
  expect(await steers(page, MONO_CHAT)).toEqual([]);
  expect(await steers(page, PROJECT_SESSION)).toEqual([]);
  await expect(
    page.getByRole("button", { name: /^Pip, Thinking/ }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
});
