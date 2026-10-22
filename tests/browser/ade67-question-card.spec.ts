import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, test } from "./app/fixture";
import { bridgeOmp, needsOmp, ompHandlers } from "./app/ompBridge";

const HOME = "/Users/test";
const GIGA_CHAT = "giga-chat";
const MINUTE = 60_000;
const QUESTION = "Spend a saved Claude rate-limit reset?";
/**
 * The card's first appearance waits on a real omp starting: on a CI runner
 * its first prompt can take several seconds ("working for 4s", no card yet).
 */
const OMP_START = { timeout: 30_000 };

// The agent's model is a local server: its first answer calls `ask_user`, a
// real omp tool that asks through omp's extension UI (`ui.select`), the way
// omp asks "Spend a saved Claude rate-limit reset?". Once the tool has a
// result, the model ends the turn. No real model is ever called.
const EXTENSION = `import { appendFileSync } from "node:fs";
import { Type } from "@sinclair/typebox";
export default function (pi) {
  pi.registerTool({
    name: "ask_user",
    label: "Ask",
    description: "Ask the user",
    parameters: Type.Object({}),
    async execute(_id, _params, _signal, _onUpdate, ctx) {
      const choice = await ctx.ui.select(${JSON.stringify(QUESTION)}, ["Yes", "No"]);
      appendFileSync(process.env.ADE67_ANSWERS, JSON.stringify({ choice: choice ?? null }) + "\\n");
      return { content: [{ type: "text", text: "answer: " + (choice ?? "skipped") }], details: {} };
    },
  });
}
`;

type Answer = { choice: string | null };
type OmpBridge = { answers: () => Answer[] };

/** Real omp with the `ask_user` extension; it records each answer it gets. */
async function startOmp(page: Page, app: AppShell): Promise<OmpBridge> {
  const dir = mkdtempSync(join(tmpdir(), "ade67-"));
  const extension = join(dir, "ask.ts");
  const answers = join(dir, "answers.jsonl");
  writeFileSync(extension, EXTENSION);
  await bridgeOmp(page, app, {
    dir,
    args: ["--no-extensions", "-e", extension],
    env: { ADE67_ANSWERS: answers },
    model: ({ messages }) =>
      messages.some((message) => message.role === "tool")
        ? { text: "Done." }
        : { tool: "ask_user", arguments: {} },
  });
  return {
    answers: () =>
      existsSync(answers)
        ? readFileSync(answers, "utf8")
            .trim()
            .split("\n")
            .map((line) => JSON.parse(line) as Answer)
        : [],
  };
}

declare global {
  interface Window {
    habitsText?: string;
  }
}

/** Atlas has one habit; mono.rs keeps what the app saves to habits.json. */
const habitHandlers = {
  ...ompHandlers,
  mono_read: (_store: unknown, { mono, path }: { mono: string; path: string }) =>
    mono === "giga-1" && path === "habits.json"
      ? {
          text:
            window.habitsText ??
            JSON.stringify([
              {
                id: "habit-1",
                name: "Feature check-in",
                instructions: "Check in on the features.",
                schedule: { scheduleKind: "daily", time: "09:00" },
                enabled: true,
                createdAt: 1,
                nextRunAt: 4_102_444_800_000,
              },
            ]),
          hash: "habits",
        }
      : {
          text: null,
          hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        },
  mono_save: (_store: unknown, { path, text }: { path: string; text: string }) => {
    if (path === "habits.json") window.habitsText = text;
    return "saved";
  },
};

const chat = (id: string, cwd: string, title: string): SessionSummary => ({
  id,
  cwd,
  harness: "omp",
  model: "omp:default",
  runtimeMode: "full-access",
  title,
  createdAt: 1,
  updatedAt: 1,
});

const ATLAS_SEED = {
  sessions: [chat(GIGA_CHAT, `${HOME}/MonoCode/gigas/atlas`, "Atlas")],
  monos: [
    {
      id: "giga-1",
      role: "giga" as const,
      data: {
        name: "Atlas",
        mascot: "ghost",
        gigaSlug: "atlas",
        sessionId: GIGA_CHAT,
        projects: [],
      },
    },
  ],
};

const card = (page: Page) => page.locator("[data-question-form]");

async function answer(page: Page, choice: "No" | "Skip") {
  await expect(card(page)).toContainText(QUESTION, OMP_START);
  if (choice === "Skip") {
    await card(page).getByRole("button", { name: "Skip" }).click();
    return;
  }
  await card(page).getByText(choice, { exact: true }).click();
  await card(page).getByRole("button", { name: "Continue" }).click();
}

async function expectAnswered(page: Page, omp: OmpBridge, choice: "No" | "Skip") {
  await expect
    .poll(omp.answers, "omp gets exactly the answer from the card")
    .toEqual([{ choice: choice === "No" ? "No" : null }]);
  await expect(card(page), "the card goes").toHaveCount(0);
  await expect(
    page.getByText(/Waiting for answers/),
    "the session leaves Waiting for answers",
  ).toHaveCount(0);
}

/** Starts Atlas's habit run with Run now and opens the run's tab at its question. */
async function startHabitRun(page: Page, app: AppShell) {
  await page.clock.install({ time: new Date("2026-10-20T09:15:00Z") });
  await app.mount(ATLAS_SEED, { handlers: habitHandlers });
  await page.getByRole("button", { name: /^Atlas, / }).click();
  await page.getByRole("tab", { name: "Habits", exact: true }).click();
  const habit = page.locator('[data-habit="habit-1"]');
  await habit.hover();
  await habit.getByRole("button", { name: "Run now" }).click();
  await page.getByRole("tab", { name: "Sessions", exact: true }).click();
  await page.locator('[data-mono-session^="app-giga-chat-habit-"]').click();
  await expect(card(page)).toContainText(QUESTION, OMP_START);
}

/**
 * The run checks on itself every 2 s; each step fires that check once,
 * without replaying every animation frame in between.
 */
async function runChecks(page: Page, ms: number, until = () => false) {
  for (let at = 0; at < ms && !until(); at += 2_000)
    await page.clock.fastForward(2_000);
}

needsOmp();

for (const choice of ["No", "Skip"] as const) {
  test(`a habit run gets ${choice} from the card after its hour of work`, async ({
    app,
    page,
  }) => {
    // 1,830 fake-clock steps: about 15 s locally, over 30 s in CI WebKit.
    test.setTimeout(2 * MINUTE);
    const omp = await startOmp(page, app);
    await startHabitRun(page, app);
    // Tyler answered about 95 minutes after the run asked. 61 minutes is
    // enough: it is past the run's 60-minute work limit (HABIT_RUN_TIMEOUT_MS),
    // where tyler/main stopped the run and skipped the question, and the next
    // limit is the 12-hour wait on the user.
    await runChecks(page, 61 * MINUTE);
    await expect(
      card(page),
      "the run still waits on the question after its hour of work",
    ).toContainText(QUESTION);
    expect(omp.answers(), "omp has no answer yet").toEqual([]);
    await answer(page, choice);
    await expectAnswered(page, omp, choice);
  });

  test(`a Giga chat gets ${choice} from the card`, async ({ app, page }) => {
    const omp = await startOmp(page, app);
    await app.mount(ATLAS_SEED, { handlers: ompHandlers });
    await page.getByRole("button", { name: /^Atlas, / }).click();
    const composer = page.getByPlaceholder("Message Atlas");
    await composer.fill("Check in");
    await composer.press("Enter");
    await answer(page, choice);
    await expectAnswered(page, omp, choice);
  });

  test(`a project session gets ${choice} from the card`, async ({ app, page }) => {
    const omp = await startOmp(page, app);
    const project = `${HOME}/acme`;
    await app.mount(
      { projects: [project], sessions: [chat("s-1", project, "Fix the login redirect")] },
      { handlers: ompHandlers },
    );
    await page.getByText("Fix the login redirect").click();
    const composer = page.getByPlaceholder("Ask, build, / for commands");
    await composer.fill("Check in");
    await composer.press("Enter");
    await answer(page, choice);
    await expectAnswered(page, omp, choice);
  });
}

test("a habit run stopped while it waits leaves no question card", async ({
  app,
  page,
  browserName,
}) => {
  test.skip(
    browserName === "webkit",
    "21,600 fake-clock steps take over 5 min in WebKit; the stop logic does not depend on the browser, and chromium runs it",
  );
  // 21,600 checks of fake time; on the base the stop comes at the hour.
  test.setTimeout(5 * MINUTE);
  const omp = await startOmp(page, app);
  await startHabitRun(page, app);
  // The run gives up on the user after 12 hours and stops; omp gets a skip.
  await runChecks(page, 12 * 60 * MINUTE + MINUTE, () => omp.answers().length > 0);
  expect(omp.answers(), "the stop skips omp's question").toEqual([{ choice: null }]);
  await expect(card(page), "the card goes with the question").toHaveCount(0);
  await expect(page.getByText(/Waiting for answers/)).toHaveCount(0);
  await expect(
    page.getByRole("main").getByRole("button", { name: "Stop" }),
    "the stopped run is no longer running",
  ).toHaveCount(0);
  await expect
    .poll(
      () => page.evaluate(() => JSON.parse(window.habitsText ?? "[]")[0]?.lastError),
      "the habit says why its run stopped (ADE-70)",
    )
    .toBe("Nobody answered it.");
});
