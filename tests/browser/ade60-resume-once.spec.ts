import type { Page } from "@playwright/test";
import { CONTINUE_PROMPT } from "../../src/features/sessions/model/inFlight";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

const HOME = "/Users/test";
const PROJECT = `${HOME}/acme`;
// The relaunch of 2026-10-20 17:19 (ship 5): 7a1eab69 ran in a worktree and
// 872c4aa7 in the project folder itself.
const IN_WORKTREE = "in-worktree";
const IN_PROJECT = "in-project";

const session = (
  id: string,
  title: string,
  worktreeCwd?: string,
): SessionSummary => ({
  id,
  cwd: PROJECT,
  ...(worktreeCwd ? { worktreeCwd } : {}),
  harness: "pi",
  model: "pi:anthropic/claude-sonnet-4-5",
  runtimeMode: "supervised",
  title,
  providerSessionId: `pi-thread-${id}`,
  createdAt: 1,
  updatedAt: 1,
});

/** A turn that was still running when MonoCode quit. */
const running = (id: string, text: string): Block[] => [
  {
    id: `${id}-opener`,
    role: "user",
    text,
    startedAt: 1,
    turnId: `${id}-turn`,
  },
  { id: `${id}-work`, role: "assistant", text: "Checking CI now." },
];

/**
 * Pi in RPC mode (piFamily.ts). Every request is answered at once except a
 * prompt, which the test accepts with `acceptPrompt`. The project folder's
 * location check waits for `window.releaseProjectLocation`, as the real
 * check takes about a second after a relaunch.
 */
const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id: string; type: string };
    if (request.type === "prompt") return null;
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
  resolve_project_location: async (_store, { path }) => {
    if (!window.projectLocation) {
      const { promise, resolve } = Promise.withResolvers<void>();
      window.projectLocation = promise;
      window.releaseProjectLocation = resolve;
    }
    await window.projectLocation;
    return { path, identity: path };
  },
};

declare global {
  interface Window {
    projectLocation?: Promise<void>;
    releaseProjectLocation?: () => void;
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

async function emitPi(app: AppShell, sessionId: string, line: object) {
  await app.emit("harness-stdout", { sessionId, line: JSON.stringify(line) });
}

const savedBlocks = (page: Page, sessionId: string): Promise<Block[]> =>
  page.evaluate(
    (sessionId) => window.appBackend!.store.blocks[sessionId] ?? [],
    sessionId,
  );

const continues = async (page: Page, sessionId: string) =>
  (await savedBlocks(page, sessionId)).filter(
    (block) => block.role === "user" && block.text === CONTINUE_PROMPT,
  );

test("a relaunch resumes each interrupted session once and times its turn", async ({
  app,
  page,
}) => {
  await page.clock.install({ time: new Date("2026-10-20T06:19:30Z") });
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        session(IN_WORKTREE, "Rust action gate", `${PROJECT}-worktrees/gate`),
        session(IN_PROJECT, "Milestone settings"),
      ],
      blocks: {
        [IN_WORKTREE]: running(IN_WORKTREE, "Fix the gate"),
        [IN_PROJECT]: running(IN_PROJECT, "Build M-10"),
      },
      inFlight: [
        { sessionId: IN_WORKTREE, cwd: PROJECT },
        { sessionId: IN_PROJECT, cwd: PROJECT },
      ],
    },
    { handlers },
  );

  // The worktree session needs no location check, so its Continue goes
  // first and the app renders again while the project session still waits.
  await expect
    .poll(async () => (await piRequests(page, IN_WORKTREE, "prompt")).length)
    .toBe(1);
  await page.clock.runFor(1_300);
  await page.evaluate(() => window.releaseProjectLocation?.());

  // The project session opens one turn with one Continue.
  await expect
    .poll(async () => (await piRequests(page, IN_PROJECT, "prompt")).length)
    .toBe(1);
  await page.clock.runFor(1_000);
  expect(await piRequests(page, IN_WORKTREE, "prompt")).toHaveLength(1);

  // The turn runs 38.8 s, as 872c4aa7's did, and its opener records that.
  const prompt = (await piRequests(page, IN_PROJECT, "prompt"))[0];
  await emitPi(app, IN_PROJECT, {
    type: "response",
    id: prompt.id,
    command: "prompt",
    success: true,
  });
  await emitPi(app, IN_PROJECT, { type: "agent_start" });
  await page.clock.runFor(38_800);
  await emitPi(app, IN_PROJECT, { type: "agent_end", willRetry: false });
  await expect
    .poll(async () => (await continues(page, IN_PROJECT))[0]?.durationMs)
    .toBeDefined();
  const blocks = await savedBlocks(page, IN_PROJECT);
  expect(
    blocks.slice(2).map(({ role, text, durationMs }) => ({
      role,
      text: text.slice(0, 40),
      long: durationMs == null ? undefined : durationMs >= 38_800,
    })),
  ).toEqual([
    {
      role: "system",
      text: "Turn interrupted when MonoCode quit.",
      long: undefined,
    },
    { role: "user", text: CONTINUE_PROMPT, long: true },
  ]);
  expect(await continues(page, IN_WORKTREE)).toHaveLength(1);
});
