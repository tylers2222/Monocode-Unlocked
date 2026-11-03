import type { Page } from "@playwright/test";
import { expect, type Handler, test } from "../app/fixture";

// ADE-176, the front-end half of a usage-limit routing grant ending. The
// session runs on the granted login "work"; its 1-minute grant from the
// default login has ended, so Rust (spaces/grants.rs `check_turn`) refuses
// the next turn with `route_grant_expired` and removes the grant. The window
// must show that as the turn's error (no hang, no child on the granted
// login) and move the session back to the grant's first login, so the turn
// after it starts there, never on "work" again.

const PROJECT = "/Users/test/acme";
// grants.rs `expired`: granted at 1000, a 1-minute TTL.
const EXPIRED =
  "route_grant_expired: the grant from default to work granted at 1000 ended at 61000";

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
  // The first turn finds the ended grant; Rust has removed it after that.
  control_authorize_turn: (store, { cwd }) => {
    const refused = store as typeof store & { grantChecked?: boolean };
    if (!refused.grantChecked) {
      refused.grantChecked = true;
      throw "route_grant_expired: the grant from default to work granted at 1000 ended at 61000";
    }
    return cwd;
  },
  control_authorize_harness_turn: () => null,
  control_turn_finished: () => null,
  control_owns_session: () => true,
  // mono_messages.rs `mono_messages_unqueued`: no message waits for a chat.
  mono_messages_unqueued: () => [],
  // The desktop app's Mono polls (ask_scheduler.rs, mono_messages.rs,
  // mono_chat.rs): nothing pending, no chat kept alive.
  mono_outbox_pending: () => [],
  mono_messages_pending_notices: () => [],
  mono_chat_keep_alive: () => false,
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

const savedAccount = async (page: Page) =>
  (
    (await callsFor(page, "session_upsert")).at(-1)?.session as
      | { providerAccountId?: string }
      | undefined
  )?.providerAccountId;

test("a turn refused because its grant ended shows the error and returns the session to its first login", async ({
  app,
  page,
}) => {
  await page.addInitScript(() => {
    Object.assign(globalThis, { isTauri: true });
    // The second Claude login, picked for this project, as the grant left it.
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        claude: [{ id: "work", provider: "claude", label: "Work" }],
      }),
    );
    localStorage.setItem(
      "monocode.providerAccountSelections.v1",
      JSON.stringify({ "/Users/test/acme": { claude: "work" } }),
    );
  });
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

  await expect(
    page.getByText(EXPIRED).first(),
    "the refusal is the turn's error",
  ).toBeVisible();
  await expect
    .poll(() => savedAccount(page), "the session is back on its first login")
    .toBe("default");
  expect(
    await callsFor(page, "harness_spawn"),
    "no child started on the granted login",
  ).toEqual([]);

  await composer.fill("Try again");
  await composer.press("Enter");
  await expect
    .poll(
      async () => (await callsFor(page, "harness_spawn")).map((a) => a.account),
      "the next turn starts on the first login",
    )
    .toEqual([{ provider: "claude", id: "default" }]);
});
