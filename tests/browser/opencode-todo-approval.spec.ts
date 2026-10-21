import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
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
const PROJECT = "/Users/test/acme";
const STEER = "Add a logout step too";
const TODO_PROMPT = "Plan the login page";
const TODO_ANSWER = "Planned with the logout step";
const COUNT_PROMPT = "Count the login steps";
const COUNT_START = "Counting the login steps";
const COUNT_ANSWER = "Counted with the logout step";

/**
 * OpenCode 1.19.3's event stream for one supervised turn: what followed the
 * prompt, the steer, and the permission reply. The real-OpenCode tests below
 * write a fresh copy to their test output folder.
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

async function send(page: Page, text: string) {
  const field = page.getByPlaceholder("Ask, build, / for commands");
  await field.fill(text);
  await field.press("Enter");
  await expect(field, "the composer takes the message").toHaveValue("");
}

async function expectTurnEnds(page: Page, answer: string, because: string) {
  await expect(page.getByText(answer), because).toBeVisible({
    timeout: 30_000,
  });
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
    "the composer is idle once the turn ends",
  ).toHaveCount(0);
}

/**
 * The turn Tyler hit in ADE-75: OpenCode asks before `todowrite`, a steer
 * arrives while it waits, and the turn ends only once the user allows it.
 */
async function steerWhileTodoApprovalWaits(page: Page) {
  await page.getByText("Login work").click();
  await send(page, TODO_PROMPT);

  const allow = page.getByRole("button", { name: "Allow", exact: true });
  await expect(
    allow,
    "the todo call OpenCode waits on can be allowed",
  ).toBeVisible({ timeout: 30_000 });
  await expect(
    page.getByRole("button", { name: "Deny", exact: true }),
  ).toBeVisible();

  await send(page, STEER);
  await expect(
    page.getByText(STEER),
    "the steer joins the waiting turn",
  ).toBeVisible();

  // Sending pins the steer to the top of the transcript; the approval row
  // stays above it, where the user scrolls back to answer.
  await page.getByRole("main").hover();
  await page.mouse.wheel(0, -600);
  await allow.click();
  await expectTurnEnds(
    page,
    TODO_ANSWER,
    "OpenCode runs the todo call and reads the steer in the same turn",
  );
}

/** A steer while the reply streams, with no approval pending. */
async function steerWhileReplyStreams(page: Page) {
  await page.getByText("Login work").click();
  await send(page, COUNT_PROMPT);
  await expect(page.getByText(COUNT_START), "the reply streams").toBeVisible({
    timeout: 30_000,
  });

  await send(page, STEER);
  await expect(
    page.getByText(STEER),
    "the steer joins the running turn",
  ).toBeVisible();
  await expectTurnEnds(
    page,
    COUNT_ANSWER,
    "OpenCode reads the steer in the same turn",
  );
}

const SCENARIOS = [
  {
    name: "shows the todo approval, takes a steer, and ends when allowed",
    fixture: "opencode-todo-approval.json",
    run: steerWhileTodoApprovalWaits,
    replies: [{ reply: "once" }],
  },
  {
    name: "takes a steer while the reply streams and ends idle",
    fixture: "opencode-steer.json",
    run: steerWhileReplyStreams,
    replies: [],
  },
];

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

for (const scenario of SCENARIOS) {
  test(`a recorded OpenCode turn ${scenario.name}`, async ({ app, page }) => {
    const recording = JSON.parse(
      readFileSync(
        new URL(`./fixtures/${scenario.fixture}`, import.meta.url),
        "utf8",
      ),
    ) as Recording;
    await page.addInitScript((recording) => {
      window.openCodeRecording = recording;
    }, recording);
    await app.mount(
      { projects: [PROJECT], sessions: [session(PROJECT)] },
      { handlers: recordedOpenCode },
    );

    await scenario.run(page);

    const calls = await page.evaluate(() =>
      window.appBackend!.calls.flatMap(({ command, args }) => {
        const call = args as { url?: string; body?: string };
        return command === "harness_http" ? [call] : [];
      }),
    );
    expect(
      calls.filter((call) => call.url!.includes("/prompt_async")),
      "the prompt and the steer each reach OpenCode",
    ).toHaveLength(2);
    expect(
      calls
        .filter((call) => call.url!.includes("/permission/"))
        .map((call) => JSON.parse(call.body!) as unknown),
      "OpenCode gets a reply for each approval",
    ).toEqual(scenario.replies);
  });
}

type ModelRequest = {
  messages: { role: string; content: unknown }[];
  tools?: unknown[];
};

const chunk = (choice: object) =>
  `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "fake-model", choices: [{ index: 0, ...choice }] })}\n\n`;
const say = (content: string) =>
  chunk({ delta: { role: "assistant", content }, finish_reason: "stop" });

/**
 * An OpenAI-compatible model. For TODO_PROMPT it calls `todowrite`, then
 * answers TODO_ANSWER once the tool ran and OpenCode passed it the steer. For
 * COUNT_PROMPT it streams COUNT_START and holds the reply open until
 * `held` settles, then answers COUNT_ANSWER once it sees the steer. No real
 * model runs.
 */
function fakeModel(held: Promise<void>): Server {
  const reply = async (response: ServerResponse, request: ModelRequest) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    const transcript = JSON.stringify(request.messages);
    if (!request.tools?.length) response.write(say("Login work"));
    else if (transcript.includes(STEER))
      response.write(
        say(transcript.includes(TODO_PROMPT) ? TODO_ANSWER : COUNT_ANSWER),
      );
    else if (request.messages.some((message) => message.role === "tool"))
      response.write(say("Planned"));
    else if (transcript.includes(COUNT_PROMPT)) {
      response.write(
        chunk({
          delta: { role: "assistant", content: COUNT_START },
          finish_reason: null,
        }),
      );
      await held;
      response.write(chunk({ delta: { content: "." }, finish_reason: "stop" }));
    } else {
      const todos = [
        {
          id: "1",
          content: TODO_PROMPT,
          status: "in_progress",
          priority: "high",
        },
      ];
      response.write(
        chunk({
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
        }),
      );
    }
    response.end("data: [DONE]\n\n");
  };
  return createServer((request, response) => {
    let body = "";
    request.on("data", (data) => (body += data));
    request.on(
      "end",
      () => void reply(response, JSON.parse(body) as ModelRequest),
    );
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

const OPENCODE_ENV = [
  "OPENCODE_CONFIG",
  "XDG_CONFIG_HOME",
  "XDG_DATA_HOME",
  "XDG_STATE_HOME",
] as const;

test.describe("with OpenCode installed", () => {
  test.skip(!OPENCODE, "needs the opencode binary on PATH");

  let model: Server;
  let release: () => void;
  let backend: HostChildBackend;
  let project: string;
  let savedEnv: Partial<Record<(typeof OPENCODE_ENV)[number], string>>;

  test.beforeEach(async () => {
    const held = Promise.withResolvers<void>();
    release = held.resolve;
    model = fakeModel(held.promise);
    await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
    const { port } = model.address() as AddressInfo;
    project = realpathSync(
      mkdtempSync(join(tmpdir(), "opencode-todo-approval-")),
    );
    const config = join(project, "opencode.json");
    writeFileSync(
      config,
      JSON.stringify({
        model: "fake/fake-model",
        small_model: "fake/fake-model",
        provider: {
          fake: {
            npm: "@ai-sdk/openai-compatible",
            options: { baseURL: `http://127.0.0.1:${port}/v1`, apiKey: "fake" },
            models: { "fake-model": { tool_call: true } },
          },
        },
      }),
    );
    savedEnv = Object.fromEntries(
      OPENCODE_ENV.map((name) => [name, process.env[name]]),
    );
    process.env.OPENCODE_CONFIG = config;
    process.env.XDG_CONFIG_HOME = join(project, "config");
    process.env.XDG_DATA_HOME = join(project, "data");
    process.env.XDG_STATE_HOME = join(project, "state");
    backend = new HostChildBackend({ opencode: OPENCODE });
  });

  test.afterEach(async () => {
    release();
    await backend.close();
    model.close();
    for (const name of OPENCODE_ENV) {
      const value = savedEnv[name];
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    rmSync(project, { recursive: true, force: true });
  });

  for (const scenario of SCENARIOS)
    test(`a real OpenCode server ${scenario.name}`, async ({
      app,
      page,
      browserName,
    }) => {
      test.skip(
        browserName === "webkit" &&
          scenario.run === steerWhileTodoApprovalWaits,
        "Playwright's macOS WebKit aborts after the todo turn (NSTextInputContext textInputClientDidUpdateSelection); the recorded test covers WebKit",
      );
      const recording: Recording = {
        sessionId: "",
        afterPrompt: [],
        afterSteer: [],
        afterReply: [],
      };
      let phase: Exclude<keyof Recording, "sessionId"> = "afterPrompt";
      let prompts = 0;
      await page.exposeFunction(
        "openCodeInvoke",
        async (command: string, args: Record<string, unknown>) => {
          const path =
            command === "harness_http"
              ? new URL(String(args.url)).pathname
              : "";
          const steer = path.endsWith("/prompt_async") && ++prompts === 2;
          if (steer) phase = "afterSteer";
          if (path.startsWith("/permission/")) phase = "afterReply";
          const result = await backend
            .invoke(command, args)
            .catch((error: unknown) => {
              throw String(error);
            });
          if (steer) release();
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
            recording[phase].push(
              (payload as { data: string }).data
                .replaceAll(project, PROJECT)
                .replaceAll(project.slice(1), PROJECT.slice(1)),
            );
          if (!page.isClosed())
            void app.emit(event, payload).catch(() => undefined);
        });
      await app.mount(
        { projects: [project], sessions: [session(project)] },
        { handlers: realOpenCode },
      );

      await scenario.run(page);

      writeFileSync(
        test.info().outputPath(scenario.fixture),
        JSON.stringify(recording, null, 2),
      );
    });
});
