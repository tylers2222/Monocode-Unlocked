import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { WorkspaceSnapshot } from "../../src/features/workspace/model/workspaceSnapshot";
import { type AppShell, expect, type Handler, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

const session = (id: string, title: string): SessionSummary => ({
  id,
  cwd: PROJECT,
  harness: "claude",
  model: "sonnet",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

/**
 * Claude Code on PATH. It answers `initialize` and nothing else, so a turn
 * runs until the test ends it.
 */
const claude: Record<string, Handler> = {
  harness_resolve_claude: () => ({ path: "/usr/local/bin/claude" }),
  harness_spawn: () => 4242,
  harness_write: (_, { sessionId, line }) => {
    const input = JSON.parse(line as string) as {
      request_id?: string;
      request?: { subtype: string };
    };
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

const tab = (page: Page, title: string) =>
  page
    .getByRole("banner")
    .getByRole("button", { name: `acme · ${title}`, exact: true });

/** Each session has a transcript, so opening one never replaces another. */
async function mount(app: AppShell, page: Page) {
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        session("login", "Fix the login redirect"),
        session("docs", "Update the docs"),
      ],
    },
    { handlers: claude },
  );
  await page.evaluate(() => {
    for (const id of ["login", "docs"])
      window.appBackend!.store.blocks[id] = [
        { id: `${id}-1`, role: "user", text: "Start here" },
      ];
  });
}

async function openSession(page: Page, title: string) {
  await page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) })
    .click();
  await expect(tab(page, title)).toBeVisible();
}

/**
 * Ends the page with no quit, as a crash or force-quit does, and opens the
 * app again on the store as it stood at that moment. A reload alone would
 * also run the app's unload save, which a crash never reaches, so the store
 * is copied first and that copy is what the next load finds.
 */
async function crashAndReopen(page: Page) {
  const store = await page.evaluate(() =>
    JSON.stringify(window.appBackend!.store),
  );
  await page.addInitScript((store) => {
    sessionStorage.setItem("appBackendStore", store);
  }, store);
  await page.reload();
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible({
    timeout: 30_000,
  });
}

test("a tab opened while a turn streams is saved before the turn ends", async ({
  app,
  page,
}) => {
  await mount(app, page);
  await openSession(page, "Fix the login redirect");
  const composer = page.getByPlaceholder("Ask, build, / for commands");
  await composer.fill("Build the login page");
  await composer.press("Enter");
  await expect(composer).toHaveValue("");
  const callsBefore = await page.evaluate(
    () => window.appBackend!.calls.length,
  );
  await openSession(page, "Update the docs");

  // A text chunk every 80 ms re-renders the app with the same snapshot, so
  // none of these gaps is long enough for a save that each re-render restarts.
  const saves = await page.evaluate(async (callsBefore) => {
    for (let chunk = 0; chunk < 15; chunk++) {
      await window.appBackend!.emit("harness-stdout", {
        sessionId: "login",
        lines: [
          JSON.stringify({
            type: "assistant",
            message: {
              id: "reply",
              role: "assistant",
              content: [{ type: "text", text: `Step ${chunk}` }],
            },
          }),
        ],
      });
      const { promise, resolve } = Promise.withResolvers();
      setTimeout(resolve, 80);
      await promise;
    }
    return window
      .appBackend!.calls.slice(callsBefore)
      .flatMap(({ command, args }) =>
        command === "workspace_set_snapshot"
          ? [
              (args as { snapshot: WorkspaceSnapshot }).snapshot.tabs.map(
                (t) => t.focusedId,
              ),
            ]
          : [],
      );
  }, callsBefore);

  expect(saves, "workspace_set_snapshot calls during the turn").toEqual([
    ["login", "docs"],
  ]);
  await tab(page, "Fix the login redirect").click();
  await expect(page.getByText("Step 14")).toBeVisible();
});

test("tabs opened before a crash come back on the next launch", async ({
  app,
  page,
}) => {
  await mount(app, page);
  await openSession(page, "Fix the login redirect");
  await openSession(page, "Update the docs");
  await page.waitForTimeout(1_000);

  await crashAndReopen(page);

  await expect(tab(page, "Fix the login redirect")).toBeVisible();
  await expect(tab(page, "Update the docs")).toBeVisible();
});
