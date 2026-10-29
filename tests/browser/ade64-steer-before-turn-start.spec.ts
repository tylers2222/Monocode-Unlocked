import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { sessionDisplayTitle, type Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, test } from "./app/fixture";
import { OMP, bridgeOmp, ompHandlers } from "./app/ompBridge";

const realDb = JSON.parse(
  readFileSync(
    new URL("./fixtures/ade64-steer-miss.realdb.json", import.meta.url),
    "utf8",
  ),
) as {
  record: Pick<SessionSummary, "id" | "harness" | "model" | "runtimeMode" | "title"> & {
    blocks: Block[];
  };
  miss: { opener: Block; steer: Block; error: Block };
};

const PROJECT = "/Users/test/acme";
const HOLD_MS = 20_000;
const REPLY = "Picked up where I left off.";

declare global {
  interface Window {
    spawnHeld?: boolean;
    releaseSpawn?: () => void;
  }
}

const heldSpawn = {
  ...ompHandlers,
  harness_spawn: async (
    _store: unknown,
    { sessionId, args }: { sessionId: string; args: string[] },
  ) => {
    await new Promise<void>((resolve) => {
      window.releaseSpawn = resolve;
      window.spawnHeld = true;
    });
    return window.ompSpawn(sessionId, args);
  },
};

type Rig = { reached: Promise<void>; release: () => void };

async function startOmpHoldingReply(page: Page, app: AppShell): Promise<Rig> {
  const reached = Promise.withResolvers<void>();
  const released = Promise.withResolvers<void>();
  await bridgeOmp(page, app, {
    dir: mkdtempSync(join(tmpdir(), "ade64-")),
    model: async ({ tools }) => {
      // A title job runs without tools.
      if (!tools.length) return { text: "Continue" };
      reached.resolve();
      await released.promise;
      return { text: REPLY };
    },
  });
  return { reached: reached.promise, release: released.resolve };
}

const saved = (page: Page): Promise<Block[]> =>
  page.evaluate((id) => window.appBackend!.store.blocks[id] ?? [], realDb.record.id);

test.skip(!OMP, "needs the omp binary on PATH");

test("a steer that reaches omp before its turn starts leaves the turn running to its real end", async ({
  app,
  page,
}) => {
  const { opener, steer, error } = realDb.miss;
  const composer = page.getByPlaceholder("Ask, build, / for commands");
  const main = page.getByRole("main");
  const stop = main.getByRole("button", { name: "Stop" });
  const omp = await startOmpHoldingReply(page, app);
  await page.clock.install({ time: new Date("2026-10-22T09:00:00Z") });
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [{ ...realDb.record, cwd: PROJECT, createdAt: 1, updatedAt: 1 }],
      blocks: { [realDb.record.id]: realDb.record.blocks },
    },
    { handlers: heldSpawn },
  );
  await expect(async () => {
    await page
      .getByText(sessionDisplayTitle(realDb.record.title, realDb.record.harness), { exact: true })
      .click();
    await expect(main.getByText(realDb.record.blocks[0].text)).toBeVisible({ timeout: 1_000 });
  }).toPass();

  await composer.fill(opener.text);
  await composer.press("Enter");
  await expect.poll(() => page.evaluate(() => window.spawnHeld)).toBe(true);
  await composer.fill(steer.text);
  await expect(page.locator('[data-composer-intent="turn"]')).toContainText("Steering");
  await composer.press("Enter");

  await expect(main.getByText(error.text, { exact: true })).toBeVisible();
  const soft = expect.configure({ soft: true });
  await soft(stop, "the refused steer leaves the turn running").toBeVisible();

  await page.evaluate(() => window.releaseSpawn!());
  await omp.reached;
  await page.clock.fastForward(HOLD_MS);
  omp.release();

  await expect(stop, "the turn ends when omp ends it").toHaveCount(0);
  const turnOf = (blocks: Block[]) =>
    blocks.find(
      (block) => block.role === "user" && block.startedAt != null && block.text === opener.text,
    );
  // The reply's first block is saved while the turn runs (ADE-89); the save
  // at the turn's end stamps its duration.
  await expect
    .poll(async () => {
      const blocks = await saved(page);
      return (
        blocks.some((block) => block.role === "assistant" && block.text.includes(REPLY)) &&
        turnOf(blocks)?.durationMs !== undefined
      );
    })
    .toBe(true);
  const blocks = await saved(page);
  const turn = turnOf(blocks);
  soft(turn?.durationMs, "the turn's duration runs to omp's real end").toBeGreaterThanOrEqual(
    HOLD_MS,
  );
  soft(
    blocks.filter((block) => block.notice === "error").map((block) => block.text),
    "the user still reads that the steer did not reach omp",
  ).toEqual([error.text]);
});
