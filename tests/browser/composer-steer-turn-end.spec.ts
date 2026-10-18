import type { Locator, Page } from "@playwright/test";
import { expect, test, type AppShell, type Handler } from "./app/fixture";

const PROJECT = "/Users/test/acme";
const OPENER = "Build the login page";
const TYPED = "stop, wrong branch";

/** A line the app writes to Claude Code's stdin (stream-json input). */
type ClaudeInput = {
  type: string;
  request_id?: string;
  request?: { subtype: string };
  message?: { content: { text: string }[] };
};

/**
 * Claude Code on PATH. It answers `initialize` as the CLI does and nothing
 * else, so a turn runs until the test prints its result.
 */
const claude: Record<string, Handler> = {
  harness_resolve_claude: () => ({ path: "/usr/local/bin/claude" }),
  harness_spawn: () => 4242,
  harness_write: (_, { sessionId, line }) => {
    const input: ClaudeInput = JSON.parse(line as string);
    if (input.request?.subtype === "initialize")
      setTimeout(() =>
        window.appBackend!.emit("harness-stdout", {
          sessionId,
          lines: [
            JSON.stringify({
              type: "control_response",
              response: {
                subtype: "success",
                request_id: input.request_id,
                response: {},
              },
            }),
          ],
        }),
      );
    return null;
  },
};

/**
 * What the app sent chat `sessionId`, in order: "new turn" for each turn it
 * opened (session_checkpoint_begin_turn), and the text of each user message
 * it wrote to Claude Code (harness_write), without the context a Mono chat
 * puts ahead of it. A steer opens no turn, and Claude reads it after the line
 * "[steer → whole turn]".
 */
function sent(page: Page, sessionId: string): Promise<string[]> {
  return page.evaluate(
    (sessionId) =>
      window.appBackend!.calls.flatMap(({ command, args }) => {
        const call = args as { sessionId?: string; line?: string };
        if (call.sessionId !== sessionId) return [];
        if (command === "session_checkpoint_begin_turn") return ["new turn"];
        if (command !== "harness_write") return [];
        const input: ClaudeInput = JSON.parse(call.line!);
        if (input.type !== "user") return [];
        return input.message!.content.map((part) =>
          part.text.replace(
            /^<monocode_context>[\s\S]*?<\/monocode_context>\n\n/,
            "",
          ),
        );
      }),
    sessionId,
  );
}

/** Claude Code prints the turn's result; Rust forwards stdout in `harness-stdout`. */
function finishTurn(app: AppShell, sessionId: string) {
  return app.emit("harness-stdout", {
    sessionId,
    lines: [
      JSON.stringify({
        type: "result",
        subtype: "success",
        is_error: false,
        num_turns: 1,
        result: "Started on the login page.",
      }),
    ],
  });
}

/** Text typed during a turn outlives it: the ADE-6 eval gate. */
async function typeThroughTurnEnd(
  app: AppShell,
  page: Page,
  sessionId: string,
  field: Locator,
) {
  await field.fill(OPENER);
  await field.press("Enter");
  await expect
    .poll(() => sent(page, sessionId), "the first message opens a turn")
    .toEqual(["new turn", OPENER]);

  await field.fill(TYPED);
  const steering = page.locator('[data-composer-intent="turn"]');
  await expect(steering, "text typed during the turn steers it").toBeVisible();
  await expect(steering).toContainText("Steering");

  await finishTurn(app, sessionId);
  await expect(
    page.locator("[data-composer-intent]"),
    "the bar goes when the turn ends",
  ).toHaveCount(0);
  await expect(field, "the typed text stays").toHaveValue(TYPED);

  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect
    .poll(
      () => sent(page, sessionId),
      "Send opens a new turn with the text instead of steering",
    )
    .toEqual(["new turn", OPENER, "new turn", TYPED]);
  await expect(field, "Send empties the field").toHaveValue("");
}

test("a project session drops Steering when its turn ends and sends the typed text as a new turn", async ({
  app,
  page,
}) => {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        {
          id: "s-1",
          cwd: PROJECT,
          harness: "claude",
          model: "sonnet",
          runtimeMode: "supervised",
          title: "Fix the login redirect",
          createdAt: 1,
          updatedAt: 1,
        },
      ],
    },
    { handlers: claude },
  );
  await page.getByText("Fix the login redirect").click();

  await typeThroughTurnEnd(
    app,
    page,
    "s-1",
    page.getByPlaceholder("Ask, build, / for commands"),
  );
});

test("a Mono chat drops Steering when its turn ends and sends the typed text as a new turn", async ({
  app,
  page,
}) => {
  await app.mount(
    {
      monos: [
        {
          id: "mono-1",
          role: "mono",
          data: { name: "Pip", mascot: "ghost", sessionId: "chat-1" },
        },
      ],
      sessions: [
        {
          id: "chat-1",
          cwd: "/Users/test",
          harness: "claude",
          model: "sonnet",
          runtimeMode: "auto",
          title: "Pip",
          createdAt: 1,
          updatedAt: 1,
        },
      ],
    },
    { handlers: claude },
  );
  await page.getByText("Pip").click();

  await typeThroughTurnEnd(app, page, "chat-1", page.getByLabel("Message Pip"));
});
