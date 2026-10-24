import { spawnSync, type ChildProcess } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { join } from "node:path";
import { createInterface } from "node:readline";
import type { Page } from "@playwright/test";
import { type AppShell, expect, test } from "./fixture";
import { spawnRealHarness } from "./realHarness";

/** The omp on PATH; empty when there is none. */
export const OMP = spawnSync("which", ["omp"], { encoding: "utf8" }).stdout.trim();

/**
 * Call at the top of a real-omp spec. Locally the spec skips without omp. CI
 * installs omp in the browser job, so there a missing omp fails each test
 * instead of hiding the spec among the skips.
 */
export function needsOmp(): void {
  test.skip(!OMP && !process.env.CI, "needs the omp binary on PATH");
  test.beforeEach(() => {
    expect(OMP, "omp is on PATH (CI installs it in the browser job)").not.toBe("");
  });
}

/** What omp sends the local model: the conversation so far and its tools. */
export type ModelRequest = {
  messages: { role: string; content?: unknown }[];
  tools: unknown[];
};

/** The model's answer: text that ends the turn, or one tool call. */
export type ModelReply =
  | { text: string }
  | { tool: string; arguments: Record<string, unknown> };

declare global {
  interface Window {
    ompSpawn: (sessionId: string, args: string[]) => Promise<number>;
    ompWrite: (sessionId: string, line: string) => Promise<void>;
    ompKill: (sessionId: string) => Promise<void>;
  }
}

/** The app's harness commands for omp, answered by the bridge. */
export const ompHandlers = {
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

/** One OpenAI-completions stream that carries `reply` and ends it. */
function sse(reply: ModelReply): string {
  const delta =
    "text" in reply
      ? { role: "assistant", content: reply.text }
      : {
          role: "assistant",
          tool_calls: [
            {
              index: 0,
              id: "call_1",
              type: "function",
              function: { name: reply.tool, arguments: JSON.stringify(reply.arguments) },
            },
          ],
        };
  const chunk = (choice: object) =>
    `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "offline-model", choices: [{ index: 0, ...choice }] })}\n\n`;
  return (
    chunk({ delta, finish_reason: null }) +
    chunk({ delta: {}, finish_reason: "text" in reply ? "stop" : "tool_calls" }) +
    "data: [DONE]\n\n"
  );
}

/**
 * Pipes the app's harness IPC (harness_spawn / harness_write / harness_kill
 * and the harness-stdout / harness-exit events) to a real omp whose only
 * model is a local server answering with `model`. omp runs in `dir` with its
 * login there (spawnRealHarness), with `args` added after the app's own. The
 * processes and the server close with the page. `ready(sessionId)` resolves
 * on the first `ready` frame of the omp the app starts for that session.
 */
export async function bridgeOmp(
  page: Page,
  app: AppShell,
  {
    dir,
    model,
    args = [],
    env = {},
  }: {
    dir: string;
    model: (request: ModelRequest) => ModelReply | Promise<ModelReply>;
    args?: string[];
    env?: NodeJS.ProcessEnv;
  },
): Promise<{ ready: (sessionId: string) => Promise<void> }> {
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", async () => {
      const { messages = [], tools = [] } = JSON.parse(body || "{}") as Partial<ModelRequest>;
      const reply = await model({ messages, tools });
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(sse(reply));
    });
  });
  const listening = Promise.withResolvers<void>();
  server.listen(0, "127.0.0.1", listening.resolve);
  await listening.promise;
  const { port } = server.address() as AddressInfo;
  const agentDir = join(dir, ".omp", "agent");
  mkdirSync(agentDir, { recursive: true });
  writeFileSync(
    join(agentDir, "models.yml"),
    `providers:\n  offline:\n    baseUrl: http://127.0.0.1:${port}/v1\n    api: openai-completions\n    apiKey: offline\n    models:\n      - id: offline-model\n`,
  );
  // omp also finds model servers running on this machine (Ollama, llama.cpp,
  // LM Studio, Apple) and caches them in the agent dir every omp here shares.
  // An omp that starts after the cache has one could pick it as its default.
  writeFileSync(
    join(agentDir, "config.yml"),
    "modelRoles:\n  default: offline/offline-model\ndisabledProviders: [ollama, llama.cpp, lm-studio, apple]\n",
  );
  console.log(
    `[omp rig] ${OMP}: HOME=${dir}, agent dir ${agentDir}, only model http://127.0.0.1:${port}/v1 (local fake)`,
  );
  const children = new Map<string, ChildProcess>();
  const readyBySession = new Map<string, PromiseWithResolvers<void>>();
  const readyFor = (sessionId: string) => {
    let ready = readyBySession.get(sessionId);
    if (!ready) readyBySession.set(sessionId, (ready = Promise.withResolvers()));
    return ready;
  };
  const spawned: ChildProcess[] = [];
  page.once("close", () => {
    for (const child of spawned) child.kill("SIGKILL");
    server.close();
  });
  const emit = (event: string, payload: unknown) => {
    if (!page.isClosed()) void app.emit(event, payload).catch(() => undefined);
  };
  await page.exposeFunction("ompSpawn", (sessionId: string, appArgs: string[]) => {
    const child = spawnRealHarness(OMP, [...appArgs, "--no-session", ...args], {
      cwd: dir,
      ownDir: dir,
      env,
    });
    children.set(sessionId, child);
    spawned.push(child);
    createInterface({ input: child.stdout! }).on("line", (line) => {
      if (line.startsWith('{"type":"ready"')) readyFor(sessionId).resolve();
      emit("harness-stdout", { sessionId, line });
    });
    child.on("exit", (code) =>
      emit("harness-exit", { sessionId, code, pid: child.pid }),
    );
    return child.pid!;
  });
  await page.exposeFunction("ompWrite", (sessionId: string, line: string) => {
    children.get(sessionId)?.stdin?.write(`${line}\n`);
  });
  await page.exposeFunction("ompKill", (sessionId: string) => {
    children.get(sessionId)?.kill();
  });
  return { ready: (sessionId) => readyFor(sessionId).promise };
}
