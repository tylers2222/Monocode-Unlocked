import type { Page } from "@playwright/test";
import { type AppShell, expect, type Handler, test } from "../app/fixture";

// ADE-173, same project in two folders: Space 1 holds the project's main
// checkout, so Rust authorizes this Space 2 session's turn in the Space's own
// copy (`<project>-worktrees/space-2/<session>`, branch `space-2/<session>`).
// The window must run the turn there, keep the copy as the session's working
// copy, and ask for the copy from then on, never the main checkout again.

const PROJECT = "/Users/test/acme";
const COPY = "/Users/test/acme-worktrees/space-2/s-1";

/** Claude Code's stdin lines carry no id; only `initialize` needs an answer. */
type ClaudeInput = { request_id?: string; request?: { subtype: string } };

const handlers: Record<string, Handler> = {
  harness_resolve_claude: () => ({ path: "/usr/local/bin/claude" }),
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
  // control.rs + spaces/leases.rs: another Space holds the main checkout,
  // so a turn asked there runs in this Space's copy; any other folder as asked.
  control_authorize_turn: (_, { cwd }) =>
    cwd === "/Users/test/acme" ? "/Users/test/acme-worktrees/space-2/s-1" : cwd,
  control_authorize_harness_turn: () => null,
  control_turn_finished: () => null,
  control_owns_session: () => true,
  mono_message_confirm_openers: () => null,
  mono_menu_bar_set_visible: () => null,
  mono_chat_sync: () => null,
  mono_chat_take: () => [],
  // useFloatingMono.ts and trackpadZoom.ts, which run only in the desktop app.
  mono_chat_publish: () => null,
  mono_chat_accept: () => false,
  mono_chat_reply: () => null,
  mono_chat_switch: () => null,
  set_trackpad_zoom_enabled: () => null,
};

/** Each `command` call's arguments for session s-1, in order. */
function callsFor(page: Page, command: string) {
  return page.evaluate(
    (command) =>
      window
        .appBackend!.calls.filter(
          ({ command: name, args }) =>
            name === command &&
            ((args as { sessionId?: string }).sessionId ??
              (args as { session?: { id: string } }).session?.id) === "s-1",
        )
        .map(({ args }) => args as Record<string, unknown>),
    command,
  );
}

function finishTurn(app: AppShell) {
  return app.emit("harness-stdout", {
    sessionId: "s-1",
    lines: [
      JSON.stringify({
        type: "result",
        subtype: "success",
        is_error: false,
        num_turns: 1,
        result: "Done.",
      }),
    ],
  });
}

test("a Space 2 session of a project another Space holds runs in its own copy and keeps it", async ({
  app,
  page,
}) => {
  // The desktop app authorizes every turn with Rust first.
  await page.addInitScript(() => Object.assign(globalThis, { isTauri: true }));
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
    { handlers },
  );
  await page.getByText("Fix the login redirect").click();
  const composer = page.getByPlaceholder("Ask, build, / for commands");
  await composer.fill("Build the login page");
  await composer.press("Enter");

  await expect
    .poll(
      async () => (await callsFor(page, "harness_spawn")).map((a) => a.cwd),
      "the agent starts in the Space's copy, not the main checkout",
    )
    .toEqual([COPY]);
  await expect
    .poll(
      async () =>
        (
          (await callsFor(page, "session_upsert")).at(-1)?.session as
            | { worktreeCwd?: string }
            | undefined
        )?.worktreeCwd,
      "the copy is saved as the session's working copy",
    )
    .toBe(COPY);

  await finishTurn(app);
  await expect
    .poll(async () => (await callsFor(page, "control_turn_finished")).length)
    .toBe(1);
  await composer.fill("Now the tests");
  await composer.press("Enter");
  await expect
    .poll(
      async () =>
        (await callsFor(page, "control_authorize_turn")).map((a) => a.cwd),
      "the next turn asks for the copy itself",
    )
    .toEqual([PROJECT, COPY]);
});
