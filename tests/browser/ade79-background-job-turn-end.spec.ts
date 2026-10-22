import { spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, test } from "./app/fixture";
import { spawnRealHarness } from "./app/realHarness";

const OMP = spawnSync("which", ["omp"], { encoding: "utf8" }).stdout.trim();
const PROJECT = "/Users/test/acme";
const SESSION = "s-1";
const HOLD_MS = 20_000;

type Reply = "watching" | "finished" | "waiting" | "noted" | "hi" | "welcome";

/**
 * The model's answer to the conversation so far. "START" makes it run a
 * watcher as an omp background job (bash `async: true`) that ends once the
 * marker file exists. omp hands the job's result back as a user message that
 * starts "Background job".
 */
function reply(last: { role: string; content?: unknown }): Reply | "start" {
  const text = JSON.stringify(last.content ?? "");
  if (last.role === "tool") return "watching";
  if (text.includes("Background job")) return "finished";
  if (text.includes("START")) return "start";
  if (text.includes("one more thing")) return "noted";
  if (text.includes("status?")) return "waiting";
  if (text.includes("hello")) return "hi";
  return "welcome";
}

const TEXT: Record<Reply, string> = {
  watching: "Watching the build.",
  finished: "The watcher finished.",
  waiting: "Still waiting on the build.",
  noted: "Noted the extra thing.",
  hi: "Hi there.",
  welcome: "You're welcome.",
};

type Hold = { reached: Promise<void>; release: () => void };

type OmpRig = {
  /** Holds the model's next `reply` until `release`; `reached` resolves when omp asks for it. */
  hold: (reply: Reply) => Hold;
  /** Ends the watcher: its background job finishes. */
  finishJob: () => void;
  /** Lets omp's session_stop hook return (only with `gateStop`). */
  openStopGate: () => void;
  /** Resolves once omp's session_stop hook is waiting on the gate. */
  stopHeld: () => Promise<void>;
};

const running = new Set<ChildProcess>();
const servers = new Set<Server>();
test.afterEach(() => {
  for (const child of running) child.kill("SIGKILL");
  running.clear();
  for (const server of servers) server.close();
  servers.clear();
});

/**
 * The app's harness IPC piped to a real omp, whose model is a local server.
 * With `gateStop`, an extension's session_stop hook holds omp's settle until
 * the test opens the gate: a steer sent then reaches omp after its run's last
 * poll, so omp ends that run with `agent_end` isTerminal:false and runs again.
 */
async function bridgeOmp(
  page: Page,
  app: AppShell,
  { gateStop = false } = {},
): Promise<OmpRig> {
  const dir = mkdtempSync(join(tmpdir(), "ade79-"));
  const marker = join(dir, "build.done");
  const gate = join(dir, "stop.gate");
  const held = join(dir, "stop.held");
  const extension = join(dir, "stop-gate.ts");
  writeFileSync(
    extension,
    `import { existsSync, writeFileSync } from "node:fs";
export default function (pi) {
  pi.on("session_stop", async () => {
    writeFileSync(${JSON.stringify(held)}, "");
    while (!existsSync(${JSON.stringify(gate)})) await new Promise((r) => setTimeout(r, 20));
    return undefined;
  });
}
`,
  );
  const holds = new Map<Reply, { reached: () => void; released: Promise<void> }>();
  const model = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", async () => {
      const { messages = [], tools = [] } = JSON.parse(body || "{}") as {
        messages?: { role: string; content?: unknown }[];
        tools?: unknown[];
      };
      // A title job runs without tools; it gets plain text.
      const next = tools.length ? reply(messages.at(-1)!) : "welcome";
      const hold = next === "start" ? undefined : holds.get(next);
      if (hold) {
        holds.delete(next as Reply);
        hold.reached();
        await hold.released;
      }
      const delta =
        next === "start"
          ? {
              role: "assistant",
              tool_calls: [
                {
                  index: 0,
                  id: "call_watch",
                  type: "function",
                  function: {
                    name: "bash",
                    arguments: JSON.stringify({
                      command: `while [ ! -f ${marker} ]; do sleep 0.1; done; echo built`,
                      async: true,
                    }),
                  },
                },
              ],
            }
          : { role: "assistant", content: TEXT[next] };
      const chunk = (choice: object) =>
        `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "offline-model", choices: [{ index: 0, ...choice }] })}\n\n`;
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(
        chunk({ delta, finish_reason: null }) +
          chunk({ delta: {}, finish_reason: next === "start" ? "tool_calls" : "stop" }) +
          "data: [DONE]\n\n",
      );
    });
  });
  servers.add(model);
  const listening = Promise.withResolvers<void>();
  model.listen(0, "127.0.0.1", listening.resolve);
  await listening.promise;
  const { port } = model.address() as AddressInfo;
  mkdirSync(join(dir, ".omp", "agent"), { recursive: true });
  writeFileSync(
    join(dir, ".omp", "agent", "models.yml"),
    `providers:\n  offline:\n    baseUrl: http://127.0.0.1:${port}/v1\n    api: openai-completions\n    apiKey: offline\n    models:\n      - id: offline-model\n`,
  );
  const children = new Map<string, ChildProcess>();
  const emit = (event: string, payload: unknown) => {
    if (!page.isClosed()) void app.emit(event, payload).catch(() => undefined);
  };
  await page.exposeFunction("ompSpawn", (sessionId: string, args: string[]) => {
    const extra = gateStop ? ["--no-extensions", "-e", extension] : [];
    const child = spawnRealHarness(OMP, [...args, "--no-session", ...extra], {
      cwd: dir,
      ownDir: dir,
    });
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
  });
  await page.exposeFunction("ompWrite", (sessionId: string, line: string) => {
    children.get(sessionId)?.stdin?.write(`${line}\n`);
  });
  await page.exposeFunction("ompKill", (sessionId: string) => {
    children.get(sessionId)?.kill();
    children.delete(sessionId);
  });
  return {
    hold: (next) => {
      const reached = Promise.withResolvers<void>();
      const released = Promise.withResolvers<void>();
      holds.set(next, { reached: reached.resolve, released: released.promise });
      return { reached: reached.promise, release: released.resolve };
    },
    finishJob: () => writeFileSync(marker, ""),
    openStopGate: () => writeFileSync(gate, ""),
    stopHeld: async () => {
      while (!existsSync(held)) await sleep(25);
    },
  };
}

declare global {
  interface Window {
    ompSpawn: (sessionId: string, args: string[]) => Promise<number>;
    ompWrite: (sessionId: string, line: string) => Promise<void>;
    ompKill: (sessionId: string) => Promise<void>;
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

const chat: SessionSummary = {
  id: SESSION,
  cwd: PROJECT,
  harness: "omp",
  model: "omp:default",
  runtimeMode: "full-access",
  title: "Watch the build",
  createdAt: 1,
  updatedAt: 1,
};

async function openSession(page: Page, app: AppShell) {
  await page.clock.install({ time: new Date("2026-10-22T09:00:00Z") });
  await app.mount({ projects: [PROJECT], sessions: [chat] }, { handlers });
  await page.getByText("Watch the build").click();
}

const composer = (page: Page) => page.getByPlaceholder("Ask, build, / for commands");
const stopButton = (page: Page) =>
  page.getByRole("main").getByRole("button", { name: "Stop" });

async function send(page: Page, text: string) {
  await composer(page).fill(text);
  await composer(page).press("Enter");
}

const saved = (page: Page): Promise<Block[]> =>
  page.evaluate((id) => window.appBackend!.store.blocks[id] ?? [], SESSION);

/** The saved user block that opened the turn with `text`. */
async function opener(page: Page, text: string) {
  return (await saved(page)).find(
    (block) => block.role === "user" && block.text.startsWith(text),
  );
}

async function savedReply(page: Page, reply: Reply) {
  return (await saved(page)).some(
    (block) => block.role === "assistant" && block.text.includes(TEXT[reply]),
  );
}

/** omp asks the model for `reply`; the test holds it for HOLD_MS of app time. */
async function holdFor(page: Page, hold: Hold) {
  await hold.reached;
  await page.clock.fastForward(HOLD_MS);
  hold.release();
}

/**
 * Soft, so a run on a build with the bug reports every checkpoint the turn
 * misses: still busy, reply not saved, no duration.
 */
async function expectTurnEnded(page: Page, text: string, reply: Reply) {
  const soft = expect.configure({ soft: true });
  await soft(stopButton(page), `the turn "${text}" ends`).toHaveCount(0);
  await soft
    .poll(() => savedReply(page, reply), `"${TEXT[reply]}" is saved at once`)
    .toBe(true);
  const block = await opener(page, text);
  soft(block?.durationMs, `the turn "${text}" has its duration`).toBeDefined();
}

test.skip(!OMP, "needs the omp binary on PATH");

test("an omp turn that leaves a background job running ends when omp's turn ends", async ({
  app,
  page,
}) => {
  const omp = await bridgeOmp(page, app);
  await openSession(page, app);

  const watching = omp.hold("watching");
  await send(page, "START the watcher");
  await holdFor(page, watching);
  await expectTurnEnded(page, "START the watcher", "watching");
  const startTurn = (await opener(page, "START the watcher"))?.durationMs ?? NaN;
  expect
    .soft(startTurn - HOLD_MS, "the duration ends at omp's last stop, not later")
    .toBeLessThan(5_000);
  expect.soft(startTurn).toBeGreaterThanOrEqual(HOLD_MS);

  // The job still runs. The next message is a turn of its own, and its reply
  // is saved when omp answers it.
  await send(page, "status?");
  await expectTurnEnded(page, "status?", "waiting");

  // The job ends while the session is idle: omp answers the result on its own.
  omp.finishJob();
  await expect
    .poll(() => savedReply(page, "finished"), "omp's answer to the job result is saved")
    .toBe(true);
  await expect(stopButton(page), "the job's result opens no turn").toHaveCount(0);
  expect(
    (await saved(page)).filter((block) => block.streaming),
    "nothing is left streaming after omp's own run",
  ).toEqual([]);
  expect(
    (await opener(page, "START the watcher"))!.durationMs,
    "the job's later end does not change the turn that started it",
  ).toBe(startTurn);

  const thanks = omp.hold("welcome");
  await send(page, "Thanks");
  await holdFor(page, thanks);
  await expectTurnEnded(page, "Thanks", "welcome");
  expect(
    (await opener(page, "Thanks"))!.durationMs,
    "the next turn runs to its own end",
  ).toBeGreaterThanOrEqual(HOLD_MS);
});

test("a message sent while omp answers a finished background job runs as the next turn", async ({
  app,
  page,
}) => {
  const omp = await bridgeOmp(page, app);
  await openSession(page, app);
  await send(page, "START the watcher");
  await expectTurnEnded(page, "START the watcher", "watching");

  // omp starts its own run for the job's result and waits on the model.
  const finished = omp.hold("finished");
  omp.finishJob();
  await finished.reached;
  await send(page, "status?");
  await page.clock.fastForward(HOLD_MS);
  finished.release();

  await expectTurnEnded(page, "status?", "waiting");
  expect(await savedReply(page, "finished"), "omp's answer to the job result is kept").toBe(
    true,
  );
  expect(
    (await saved(page)).filter((block) => block.role === "system" && !block.interjection),
    "omp takes the message: no error such as Agent is busy",
  ).toEqual([]);
  expect(
    (await opener(page, "status?"))!.durationMs,
    "the turn ends after omp answered it, not when omp's own run ended",
  ).toBeGreaterThanOrEqual(HOLD_MS);
});

test("a steer that omp takes after its run's last poll stays in the turn", async ({
  app,
  page,
}) => {
  const omp = await bridgeOmp(page, app, { gateStop: true });
  await openSession(page, app);
  await send(page, "hello");
  await omp.stopHeld();
  const noted = omp.hold("noted");
  await composer(page).fill("one more thing");
  await expect(page.locator('[data-composer-intent="turn"]')).toContainText("Steering");
  await composer(page).press("Enter");
  // The steer is queued in omp; omp then ends its run with isTerminal:false
  // and runs again for the steer.
  omp.openStopGate();
  await holdFor(page, noted);

  await expectTurnEnded(page, "hello", "noted");
  expect(
    (await opener(page, "hello"))!.durationMs,
    "the turn ends after omp answered the steer",
  ).toBeGreaterThanOrEqual(HOLD_MS);
  expect(await savedReply(page, "hi")).toBe(true);
});
