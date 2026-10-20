import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, test } from "./app/fixture";

const HOME = "/Users/test";
const GIGA_CHAT = "giga-chat";
const MINUTE = 60_000;
const QUESTION = "Spend a saved Claude rate-limit reset?";

const OMP = spawnSync("which", ["omp"], { encoding: "utf8" }).stdout.trim();

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

function fakeModel(): Server {
  return createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      const { messages = [] } = JSON.parse(body || "{}") as {
        messages?: { role: string }[];
      };
      const delta = messages.some((message) => message.role === "tool")
        ? { role: "assistant", content: "Done." }
        : {
            role: "assistant",
            tool_calls: [
              {
                index: 0,
                id: "call_1",
                type: "function",
                function: { name: "ask_user", arguments: "{}" },
              },
            ],
          };
      const chunk = (choice: object) =>
        `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "offline-model", choices: [{ index: 0, ...choice }] })}\n\n`;
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(
        chunk({ delta, finish_reason: null }) +
          chunk({ delta: {}, finish_reason: "content" in delta ? "stop" : "tool_calls" }) +
          "data: [DONE]\n\n",
      );
    });
  });
}

type Answer = { choice: string | null };
type OmpBridge = { answers: () => Answer[] };

const running = new Set<ChildProcess>();
const servers = new Set<Server>();
test.afterEach(() => {
  for (const child of running) child.kill("SIGKILL");
  running.clear();
  for (const server of servers) server.close();
  servers.clear();
});

/**
 * The app's harness IPC (harness_spawn / harness_write / harness_kill and the
 * harness-stdout / harness-exit events) piped to a real omp process.
 */
async function bridgeOmp(page: Page, app: AppShell): Promise<OmpBridge> {
  const dir = mkdtempSync(join(tmpdir(), "ade67-"));
  const extension = join(dir, "ask.ts");
  const answers = join(dir, "answers.jsonl");
  writeFileSync(extension, EXTENSION);
  const model = fakeModel();
  servers.add(model);
  await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
  const { port } = model.address() as AddressInfo;
  mkdirSync(join(dir, "agent"));
  writeFileSync(
    join(dir, "agent", "models.yml"),
    `providers:\n  offline:\n    baseUrl: http://127.0.0.1:${port}/v1\n    api: openai-completions\n    apiKey: offline\n    models:\n      - id: offline-model\n`,
  );
  const env: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    HOME: dir,
    PI_CODING_AGENT_DIR: join(dir, "agent"),
    ADE67_ANSWERS: answers,
  };
  const children = new Map<string, ChildProcess>();
  const emit = (event: string, payload: unknown) => {
    if (!page.isClosed()) void app.emit(event, payload).catch(() => undefined);
  };
  await page.exposeFunction(
    "ompSpawn",
    (sessionId: string, args: string[]): number => {
      const child = spawn(
        OMP,
        [...args, "--no-session", "--no-extensions", "-e", extension],
        { cwd: dir, env },
      );
      children.set(sessionId, child);
      running.add(child);
      createInterface({ input: child.stdout! }).on("line", (line) =>
        emit("harness-stdout", { sessionId, line }),
      );
      child.on("exit", (code) => {
        running.delete(child);
        emit("harness-exit", { sessionId, code, pid: child.pid });
      });
      return child.pid!;
    },
  );
  await page.exposeFunction("ompWrite", (sessionId: string, line: string) => {
    children.get(sessionId)?.stdin?.write(`${line}\n`);
  });
  await page.exposeFunction("ompKill", (sessionId: string) => {
    children.get(sessionId)?.kill();
    children.delete(sessionId);
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
    ompSpawn: (sessionId: string, args: string[]) => Promise<number>;
    ompWrite: (sessionId: string, line: string) => Promise<void>;
    ompKill: (sessionId: string) => Promise<void>;
    habitsText?: string;
  }
}

const handlers = {
  harness_resolve_omp: () => ({ path: "omp" }),
  harness_spawn: (
    _store: unknown,
    { sessionId, args }: { sessionId: string; args: string[] },
  ) => window.ompSpawn(sessionId, args),
  harness_write: (
    _store: unknown,
    { sessionId, line }: { sessionId: string; line: string },
  ) => window.ompWrite(sessionId, line).then(() => null),
  harness_kill: (_store: unknown, { sessionId }: { sessionId: string }) =>
    window.ompKill(sessionId).then(() => null),
};

/** Atlas has one habit; mono.rs keeps what the app saves to habits.json. */
const habitHandlers = {
  ...handlers,
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
  await expect(card(page)).toContainText(QUESTION);
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
  await expect(card(page)).toContainText(QUESTION);
}

/**
 * The run checks on itself every 2 s; each step fires that check once,
 * without replaying every animation frame in between.
 */
async function runChecks(page: Page, ms: number, until = () => false) {
  for (let at = 0; at < ms && !until(); at += 2_000)
    await page.clock.fastForward(2_000);
}

test.skip(!OMP, "needs the omp binary on PATH");

for (const choice of ["No", "Skip"] as const) {
  test(`a habit run gets ${choice} from the card after its hour of work`, async ({
    app,
    page,
  }) => {
    const omp = await bridgeOmp(page, app);
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
    const omp = await bridgeOmp(page, app);
    await app.mount(ATLAS_SEED, { handlers });
    await page.getByRole("button", { name: /^Atlas, / }).click();
    const composer = page.getByPlaceholder("Message Atlas");
    await composer.fill("Check in");
    await composer.press("Enter");
    await answer(page, choice);
    await expectAnswered(page, omp, choice);
  });

  test(`a project session gets ${choice} from the card`, async ({ app, page }) => {
    const omp = await bridgeOmp(page, app);
    const project = `${HOME}/acme`;
    await app.mount(
      { projects: [project], sessions: [chat("s-1", project, "Fix the login redirect")] },
      { handlers },
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
}) => {
  // 21,600 checks of fake time; on the base the stop comes at the hour.
  test.setTimeout(5 * MINUTE);
  const omp = await bridgeOmp(page, app);
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
});
