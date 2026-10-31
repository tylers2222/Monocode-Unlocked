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
const RUN = "Feature check-in, 8:15 pm run";

// The agent's model is a local server: its first answer calls `ask_user`, a
// real omp tool that asks through omp's extension UI (`ui.select`), the way
// omp asks "Spend a saved Claude rate-limit reset?". Once the tool has a
// result, the model ends the turn with "Done.". No real model is ever called.
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
      appendFileSync(process.env.OMP_ANSWERS, JSON.stringify({ choice: choice ?? null }) + "\\n");
      return { content: [{ type: "text", text: "answer: " + (choice ?? "skipped") }], details: {} };
    },
  });
}
`;

type Answer = { choice: string | null };
type OmpBridge = { answers: () => Answer[] };

/** Real omp with the `ask_user` extension; it records each answer it gets. */
async function startOmp(page: Page, app: AppShell): Promise<OmpBridge> {
  const dir = mkdtempSync(join(tmpdir(), "ade71-"));
  const extension = join(dir, "ask.ts");
  const answers = join(dir, "answers.jsonl");
  writeFileSync(extension, EXTENSION);
  await bridgeOmp(page, app, {
    dir,
    args: ["--no-extensions", "-e", extension],
    env: { OMP_ANSWERS: answers },
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
const handlers = {
  ...ompHandlers,
  // The restart reads omp's own session file; the bridge runs omp --no-session.
  omp_session_interjections: () => [],
  mono_read: (
    _store: unknown,
    { mono, path }: { mono: string; path: string },
  ) =>
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
  mono_save: (
    _store: unknown,
    { path, text }: { path: string; text: string },
  ) => {
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

/** The habit's card in Atlas's chat, in whichever state it is. */
const monoCard = (page: Page) => page.locator("[data-habit-question]");
/** The question card in the run's own session. */
const runCard = (page: Page) =>
  page.locator("[data-question-form]:not([data-habit-question] *)");
const runRow = (page: Page) =>
  page.locator('[data-mono-session^="app-giga-chat-habit-"]');

async function shot(page: Page, name: string) {
  // A Mono turn's actions show on hover; the frames draw the chat at rest.
  await page.mouse.move(0, 0);
  await page.screenshot({ path: test.info().outputPath(`${name}.png`) });
}

/**
 * Starts Atlas's habit run with Run now, at 8:15 pm Sydney time, and lets
 * the run check on itself every 2 s until its question is in Atlas's chat.
 */
async function startHabitRun(page: Page, app: AppShell) {
  await page.clock.install({ time: new Date("2026-10-22T09:15:00Z") });
  await app.mount(ATLAS_SEED, { handlers });
  await page.getByRole("button", { name: /^Atlas, / }).click();
  await page.getByRole("tab", { name: "Habits", exact: true }).click();
  const habit = page.locator('[data-habit="habit-1"]');
  await habit.hover();
  await habit.getByRole("button", { name: "Run now" }).click();
  await expect
    .poll(
      async () => {
        await page.clock.fastForward(2_000);
        return monoCard(page).count();
      },
      { message: "the run's question shows in the Mono chat", timeout: 30_000 },
    )
    .toBe(1);
}

test.use({ locale: "en-AU", timezoneId: "Australia/Sydney" });
needsOmp();

test("a habit run's question is answered in the Mono chat and the run goes on", async ({
  app,
  page,
}) => {
  const omp = await startOmp(page, app);
  await startHabitRun(page, app);
  const card = monoCard(page);
  await expect(card).toHaveAttribute("data-habit-question", "open");
  await expect(card, "the top line names the habit and the run").toContainText(
    "Feature check-in · run from 8:15 pm",
  );
  await expect(card).toContainText(QUESTION);
  for (const name of ["Skip", "Open run", "Continue"])
    await expect(card.getByRole("button", { name })).toBeVisible();
  expect(omp.answers(), "the question waits for the user").toEqual([]);
  await card.getByText("Yes", { exact: true }).click();
  await shot(page, "A1-open");

  await card.getByRole("button", { name: "Continue" }).click();
  await expect
    .poll(omp.answers, "omp gets the answer from the Mono chat, once")
    .toEqual([{ choice: "Yes" }]);
  await expect(card, "the card folds to the answer").toHaveAttribute(
    "data-habit-question",
    "answered",
  );
  await expect(card).toContainText(QUESTION);
  await expect(card).toContainText("Yes");
  await expect(card).toContainText(RUN);
  await expect(card.getByRole("button")).toHaveCount(0);
  await shot(page, "A2-answered");

  await page.getByRole("tab", { name: "Sessions", exact: true }).click();
  await runRow(page).click();
  await expect(runCard(page), "the run's own card settled too").toHaveCount(0);
  await expect(page.getByText(/Waiting for answers/)).toHaveCount(0);
  await expect(
    page.getByText("Done.", { exact: true }).first(),
    "the run goes on past the question",
  ).toBeVisible();

  await app.restart();
  await page.getByRole("button", { name: /^Atlas, / }).click();
  await expect(card, "the answered line stays after a restart").toHaveAttribute(
    "data-habit-question",
    "answered",
  );
  await expect(card).toContainText(RUN);
});

test("Skip in the run's own card settles the Mono chat's card with it", async ({
  app,
  page,
}) => {
  const omp = await startOmp(page, app);
  await startHabitRun(page, app);
  await monoCard(page).getByRole("button", { name: "Open run" }).click();
  await expect(
    runCard(page),
    "Open run shows the run at its question",
  ).toContainText(QUESTION);
  await runCard(page).getByRole("button", { name: "Skip" }).click();
  await expect
    .poll(omp.answers, "omp gets the skip once")
    .toEqual([{ choice: null }]);
  await expect(monoCard(page), "the Mono chat's card settles").toHaveAttribute(
    "data-habit-question",
    "skipped",
  );
  await expect(monoCard(page)).toContainText(`${QUESTION}Skipped`);
  await expect(monoCard(page)).toContainText(RUN);
  await expect(runCard(page)).toHaveCount(0);
  await shot(page, "A2-skipped");
});

test("the card stays, dimmed, when you stop the run", async ({ app, page }) => {
  const omp = await startOmp(page, app);
  await startHabitRun(page, app);
  await monoCard(page).getByRole("button", { name: "Open run" }).click();
  await expect(runCard(page)).toContainText(QUESTION);
  await page
    .getByRole("complementary", { name: "Atlas overview" })
    .getByRole("button", { name: "Stop", exact: true })
    .click();
  await expect
    .poll(omp.answers, "the stop skips omp's question")
    .toEqual([{ choice: null }]);
  await expect(monoCard(page)).toHaveAttribute(
    "data-habit-question",
    "stopped",
  );
  await expect(monoCard(page)).toContainText(
    "You stopped the run. Nothing to answer.",
  );
  await expect(monoCard(page)).toContainText(QUESTION);
  for (const name of ["Skip", "Continue"])
    await expect(
      monoCard(page).getByRole("button", { name }),
      `${name} does nothing`,
    ).toBeDisabled();
  await expect(
    monoCard(page).getByRole("button", { name: "Open run" }),
  ).toHaveCount(0);
  await shot(page, "A3-stopped");
});

test("the card stays, dimmed, when nobody answers in 12 h", async ({
  app,
  page,
}) => {
  const omp = await startOmp(page, app);
  await startHabitRun(page, app);
  // The wait is wall time, so the run's next check, 12 h on, gives up.
  await page.clock.fastForward(12 * 60 * MINUTE);
  await expect
    .poll(omp.answers, "the run gives up and skips omp's question")
    .toEqual([{ choice: null }]);
  await expect(monoCard(page)).toHaveAttribute(
    "data-habit-question",
    "timed-out",
  );
  await expect(monoCard(page)).toContainText(
    "No answer in 12 h. The run was stopped.",
  );
  await expect(
    monoCard(page).getByRole("button", { name: "Continue" }),
  ).toBeDisabled();
  await shot(page, "A3-timed-out");
});
