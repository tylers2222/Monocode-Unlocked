import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { HostChildBackend } from "../../host/child-backend";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { expect, type Handler, test } from "./app/fixture";

const OPENCODE = spawnSync("which", ["opencode"], {
  encoding: "utf8",
}).stdout.trim();
const PROMPT = "Plan the login page";
const STEER = "Add a logout step too";
const ANSWER = "Planned with the logout step";
const RECORDING = new URL(
  "./fixtures/opencode-todo-approval.json",
  import.meta.url,
);

/**
 * OpenCode 1.19.3's event stream for one supervised turn: what follows the
 * prompt, the steer sent while OpenCode waited for the `todowrite`
 * permission, and the permission reply. The real-OpenCode test below writes
 * a fresh copy to its test output folder.
 */
type Recording = {
  sessionId: string;
  afterPrompt: string[];
  afterSteer: string[];
  afterReply: string[];
};

const session = (cwd: string): SessionSummary => ({
  id: "s-1",
  cwd,
  harness: "opencode",
  model: "opencode:fake/fake-model",
  runtimeMode: "supervised",
  title: "Login work",
  createdAt: 1,
  updatedAt: 1,
});

/**
 * The turn Tyler hit in ADE-75: OpenCode asks before `todowrite`, a steer
 * arrives while it waits, and the turn ends only once the user allows it.
 */
async function steerWhileTodoApprovalWaits(page: Page) {
  await page.getByText("Login work").click();
  const field = page.getByPlaceholder("Ask, build, / for commands");
  await field.fill(PROMPT);
  await field.press("Enter");

  const allow = page.getByRole("button", { name: "Allow", exact: true });
  await expect(
    allow,
    "the todo call OpenCode waits on can be allowed",
  ).toBeVisible({
    timeout: 30_000,
  });
  await expect(
    page.getByRole("button", { name: "Deny", exact: true }),
  ).toBeVisible();

  await field.fill(STEER);
  await field.press("Enter");
  await expect(
    page.getByText(STEER),
    "the steer joins the waiting turn",
  ).toBeVisible();
  await expect(field, "the composer takes the steer").toHaveValue("");

  // Sending pins the steer to the top of the transcript; the approval row
  // stays above it, where the user scrolls back to answer.
  await page.getByRole("main").hover();
  await page.mouse.wheel(0, -600);
  await allow.click();
  await expect(
    page.getByText(ANSWER),
    "OpenCode runs the todo call and reads the steer in the same turn",
  ).toBeVisible({ timeout: 30_000 });
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
    "the composer is idle once the turn ends",
  ).toHaveCount(0);
}

declare global {
  interface Window {
    openCodeRecording: Recording;
    openCodeStream?: string;
    openCodePrompts?: number;
    openCodeInvoke: (command: string, args: unknown) => Promise<unknown>;
  }
}

/**
 * `opencode serve` behind the app's harness commands. Each request answers as
 * OpenCode did in the recording, then streams the events that followed it.
 */
const recordedOpenCode: Record<string, Handler> = {
  harness_resolve_opencode: () => ({ path: "/usr/local/bin/opencode" }),
  harness_exec: (_store, { args }) =>
    (args as string[])[0] === "--version" ? "1.19.3\n" : "",
  harness_free_port: () => 4096,
  harness_spawn: (_store, { sessionId }) => {
    setTimeout(() =>
      window.appBackend!.emit("harness-stdout", {
        sessionId,
        lines: ["opencode server listening on http://127.0.0.1:4096"],
      }),
    );
    return 4242;
  },
  harness_sse_open: (_store, { sessionId }) => {
    window.openCodeStream = sessionId as string;
    return null;
  },
  harness_sse_close: () => null,
  harness_http: (_store, { method, url }) => {
    const recording = window.openCodeRecording;
    const path = new URL(url as string).pathname;
    const stream = (events: string[]) =>
      setTimeout(() =>
        window.appBackend!.emit("harness-sse", {
          sessionId: window.openCodeStream,
          events,
        }),
      );
    if (method === "POST" && path === "/session")
      return {
        status: 200,
        body: JSON.stringify({
          id: recording.sessionId,
          directory: "/Users/test/acme",
        }),
      };
    if (path.endsWith("/prompt_async")) {
      window.openCodePrompts = (window.openCodePrompts ?? 0) + 1;
      stream(
        window.openCodePrompts === 1
          ? recording.afterPrompt
          : recording.afterSteer,
      );
      return { status: 204, body: "" };
    }
    if (path.startsWith("/permission/")) {
      stream(recording.afterReply);
      return { status: 200, body: "true" };
    }
    throw `OpenCode has no route ${method} ${path}`;
  },
};

test("a recorded OpenCode turn shows the todo approval, takes a steer, and ends when allowed", async ({
  app,
  page,
}) => {
  const recording = JSON.parse(readFileSync(RECORDING, "utf8")) as Recording;
  await page.addInitScript((recording) => {
    window.openCodeRecording = recording;
  }, recording);
  await app.mount(
    { projects: ["/Users/test/acme"], sessions: [session("/Users/test/acme")] },
    { handlers: recordedOpenCode },
  );

  await steerWhileTodoApprovalWaits(page);

  const replies = await page.evaluate(() =>
    window.appBackend!.calls.flatMap(({ command, args }) => {
      const call = args as { url?: string; body?: string };
      return command === "harness_http" && call.url?.includes("/permission/")
        ? [JSON.parse(call.body!) as unknown]
        : [];
    }),
  );
  expect(replies, "OpenCode gets one allow-once reply").toEqual([
    { reply: "once" },
  ]);
});

type ModelRequest = {
  messages: { role: string; content: unknown }[];
  tools?: unknown[];
};

/**
 * An OpenAI-compatible model that calls `todowrite` first and, once the tool
 * ran, answers ANSWER when OpenCode passed it the steer. No real model runs.
 */
function fakeModel(): Server {
  const chunks = (request: ModelRequest) => {
    if (!request.tools?.length)
      return [
        {
          delta: { role: "assistant", content: "Login plan" },
          finish_reason: "stop",
        },
      ];
    if (request.messages.some((message) => message.role === "tool")) {
      const steered = JSON.stringify(request.messages).includes(STEER);
      return [
        {
          delta: { role: "assistant", content: steered ? ANSWER : "Planned" },
          finish_reason: "stop",
        },
      ];
    }
    const todos = [
      {
        id: "1",
        content: "Plan the login page",
        status: "in_progress",
        priority: "high",
      },
    ];
    return [
      {
        delta: {
          role: "assistant",
          tool_calls: [
            {
              index: 0,
              id: "call_todo",
              type: "function",
              function: {
                name: "todowrite",
                arguments: JSON.stringify({ todos }),
              },
            },
          ],
        },
        finish_reason: "tool_calls",
      },
    ];
  };
  const reply = (response: ServerResponse, request: ModelRequest) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    for (const choice of chunks(request))
      response.write(
        `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "fake-model", choices: [{ index: 0, ...choice }] })}\n\n`,
      );
    response.end("data: [DONE]\n\n");
  };
  return createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => reply(response, JSON.parse(body) as ModelRequest));
  });
}

/** The same harness commands, answered by HostChildBackend in the test process. */
const realOpenCode: Record<string, Handler> = {
  harness_resolve_opencode: (_store, args) =>
    window.openCodeInvoke("harness_resolve_opencode", args),
  harness_exec: (_store, args) => window.openCodeInvoke("harness_exec", args),
  harness_free_port: (_store, args) =>
    window.openCodeInvoke("harness_free_port", args),
  harness_spawn: (_store, args) => window.openCodeInvoke("harness_spawn", args),
  harness_sse_open: (_store, args) =>
    window.openCodeInvoke("harness_sse_open", args),
  harness_sse_close: (_store, args) =>
    window.openCodeInvoke("harness_sse_close", args),
  harness_http: (_store, args) => window.openCodeInvoke("harness_http", args),
};

test.describe("with OpenCode installed", () => {
  test.skip(!OPENCODE, "needs the opencode binary on PATH");

  let model: Server;
  let backend: HostChildBackend;
  let project: string;

  test.beforeEach(async () => {
    model = fakeModel();
    await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
    const { port } = model.address() as AddressInfo;
    project = mkdtempSync(join(tmpdir(), "opencode-todo-approval-"));
    const config = join(project, "opencode.json");
    writeFileSync(
      config,
      JSON.stringify({
        provider: {
          fake: {
            npm: "@ai-sdk/openai-compatible",
            options: { baseURL: `http://127.0.0.1:${port}/v1`, apiKey: "fake" },
            models: { "fake-model": { tool_call: true } },
          },
        },
      }),
    );
    process.env.OPENCODE_CONFIG = config;
    process.env.XDG_DATA_HOME = join(project, "data");
    process.env.XDG_STATE_HOME = join(project, "state");
    backend = new HostChildBackend({ opencode: OPENCODE });
  });

  test.afterEach(async () => {
    await backend.close();
    model.close();
    for (const name of ["OPENCODE_CONFIG", "XDG_DATA_HOME", "XDG_STATE_HOME"])
      delete process.env[name];
  });

  test("a real OpenCode server shows the todo approval, takes a steer, and ends when allowed", async ({
    app,
    page,
  }) => {
    const recording: Recording = {
      sessionId: "",
      afterPrompt: [],
      afterSteer: [],
      afterReply: [],
    };
    let phase: Exclude<keyof Recording, "sessionId"> = "afterPrompt";
    await page.exposeFunction(
      "openCodeInvoke",
      async (command: string, args: Record<string, unknown>) => {
        const path =
          command === "harness_http" ? new URL(String(args.url)).pathname : "";
        if (path.endsWith("/prompt_async") && recording.afterPrompt.length)
          phase = "afterSteer";
        if (path.startsWith("/permission/")) phase = "afterReply";
        const result = await backend
          .invoke(command, args)
          .catch((error: unknown) => {
            throw String(error);
          });
        if (path === "/session")
          recording.sessionId = (
            JSON.parse((result as { body: string }).body) as { id: string }
          ).id;
        return result ?? null;
      },
    );
    for (const event of [
      "harness-stdout",
      "harness-stderr",
      "harness-exit",
      "harness-sse",
      "harness-sse-end",
    ])
      await backend.listen(event, ({ payload }) => {
        if (event === "harness-sse")
          recording[phase].push((payload as { data: string }).data);
        if (!page.isClosed())
          void app.emit(event, payload).catch(() => undefined);
      });
    await app.mount(
      { projects: [project], sessions: [session(project)] },
      { handlers: realOpenCode },
    );

    await steerWhileTodoApprovalWaits(page);

    writeFileSync(
      test.info().outputPath("opencode-todo-approval.json"),
      JSON.stringify(recording, null, 2),
    );
  });
});
