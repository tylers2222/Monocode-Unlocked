import type { Locator, Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-253. Only the shown tab mounts a transcript and a Composer; a hidden
// tab gets both back from the transcript pool and the draft cache. Before,
// every open tab kept its whole pane mounted inside `display: none`.

const PROJECT = "/Users/test/acme";
const TITLES = ["Login", "Docs", "Billing", "Search"];
const LONG = "Login";
const RUNNING = "Docs";
const PLACEHOLDER = "Ask, build, / for commands";

const chat = (title: string): SessionSummary => ({
  id: title.toLowerCase(),
  cwd: PROJECT,
  harness: "pi",
  model: "pi:anthropic/claude-sonnet-4-5",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

/** Pi in RPC mode; every request but a prompt is answered at once. */
const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id: string; type: string };
    if (request.type === "prompt") return null;
    void window.appBackend!.emit("harness-stdout", {
      sessionId,
      line: JSON.stringify({
        type: "response",
        id: request.id,
        command: request.type,
        success: true,
        data: {},
      }),
    });
    return null;
  },
};

const titleTab = (page: Page, title: string) =>
  page
    .getByRole("banner")
    .getByRole("button", { name: new RegExp(`^acme · ${title}( · |$)`) });

const tabStatus = (page: Page, title: string) =>
  page
    .locator("[data-title-tab-id]", { has: titleTab(page, title) })
    .locator("[data-harness-status]");

const composer = (page: Page) =>
  page.getByRole("textbox", { name: PLACEHOLDER });

async function showTab(page: Page, title: string) {
  await titleTab(page, title).click();
  await expect(composer(page)).toBeVisible();
}

/** Each session has a transcript, so opening one never replaces another tab. */
async function openSession(page: Page, title: string) {
  await page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) })
    .click();
  await expect(titleTab(page, title)).toBeVisible();
}

async function prompts(page: Page, sessionId: string) {
  return page.evaluate(
    (sessionId) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_write" &&
            (args as { sessionId: string }).sessionId === sessionId,
        )
        .map(({ args }) => JSON.parse((args as { line: string }).line))
        .filter((request: { type: string }) => request.type === "prompt") as {
        id: string;
      }[],
    sessionId,
  );
}

async function startTurn(page: Page, app: AppShell, sessionId: string) {
  await composer(page).fill("Write the setup guide");
  await composer(page).press("Enter");
  await expect
    .poll(async () => (await prompts(page, sessionId)).length)
    .toBe(1);
  const [prompt] = await prompts(page, sessionId);
  await app.emit("harness-stdout", {
    sessionId,
    line: JSON.stringify({
      type: "response",
      id: prompt.id,
      command: "prompt",
      success: true,
    }),
  });
}

/** How far the shown transcript sits above its end, in px. */
const distanceFromBottom = (scroller: Locator) =>
  scroller.evaluate((el) => el.scrollHeight - el.clientHeight - el.scrollTop);

test("hidden tabs mount no transcript or Composer and keep their place", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [PROJECT], sessions: TITLES.map(chat) },
    { handlers },
  );
  await page.evaluate(
    ({ ids, long }) => {
      const paragraph =
        "This step explains the release checklist in detail, with the commands to run and the checks to make before moving on. ";
      for (const id of ids)
        window.appBackend!.store.blocks[id] =
          id === long
            ? Array.from({ length: 20 }, (_, turn) => [
                {
                  id: `${id}-u${turn}`,
                  role: "user" as const,
                  text: `Step ${turn}?`,
                },
                {
                  id: `${id}-a${turn}`,
                  role: "assistant" as const,
                  text: paragraph.repeat(6),
                },
              ]).flat()
            : [{ id: `${id}-1`, role: "user" as const, text: "Start here" }];
    },
    { ids: TITLES.map((title) => chat(title).id), long: chat(LONG).id },
  );
  for (const title of TITLES) await openSession(page, title);

  // One tab shows; the other three mount neither part of their pane.
  await expect(page.locator("textarea")).toHaveCount(1);
  await expect(page.locator(".agent-transcript")).toHaveCount(1);

  // A draft and a scroll offset in one tab survive a visit to another.
  await showTab(page, LONG);
  await composer(page).fill("Half-written release note");
  const scroller = page.locator(".agent-transcript");
  await expect
    .poll(() => scroller.evaluate((el) => el.scrollHeight - el.clientHeight))
    .toBeGreaterThan(1_000);
  await scroller.hover();
  await page.mouse.wheel(0, -700);
  await expect.poll(() => distanceFromBottom(scroller)).toBeGreaterThan(300);
  const offset = await distanceFromBottom(scroller);

  await showTab(page, "Billing");
  await expect(composer(page)).toHaveValue("");
  await expect(page.locator("textarea")).toHaveCount(1);
  await showTab(page, LONG);
  await expect(composer(page)).toHaveValue("Half-written release note");
  await expect
    .poll(async () => Math.abs((await distanceFromBottom(scroller)) - offset))
    .toBeLessThanOrEqual(2);

  // A turn in a hidden tab streams and ends; its tab shows both, and the
  // reply is there on return.
  const running = chat(RUNNING).id;
  await showTab(page, RUNNING);
  await startTurn(page, app, running);
  await showTab(page, LONG);
  const frame = (event: object) =>
    app.emit("harness-stdout", {
      sessionId: running,
      line: JSON.stringify(event),
    });
  await frame({
    type: "message_update",
    assistantMessageEvent: {
      type: "text_delta",
      delta: "The guide streamed in while its tab was hidden.",
    },
  });
  await expect(tabStatus(page, RUNNING)).toHaveAttribute(
    "data-harness-status",
    "busy",
  );
  await frame({ type: "agent_end", willRetry: false });
  await expect(tabStatus(page, RUNNING)).toHaveAttribute(
    "data-harness-status",
    "done",
  );
  await expect(titleTab(page, RUNNING)).toHaveAccessibleName(
    /Response complete$/,
  );
  await expect(page.locator(".agent-transcript")).toHaveCount(1);

  await showTab(page, RUNNING);
  await expect(
    page
      .locator(".agent-transcript")
      .getByText("The guide streamed in while its tab was hidden."),
  ).toBeVisible();
  await expect(page.locator("textarea")).toHaveCount(1);
});
