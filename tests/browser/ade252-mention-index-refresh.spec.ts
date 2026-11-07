import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";
const TITLES = ["Login", "Docs", "Billing", "Search", "Onboarding", "Exports"];
const UNCHANGED_TOOL_CALLS = 3;

declare global {
  interface Window {
    ade252Listing?: string[];
  }
}

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
  // `git ls-files` answers with a fresh array on every call, as Rust does.
  list_project_files: () =>
    (window.ade252Listing ?? []).map((relative) => ({
      name: relative.split("/").pop()!,
      path: `/Users/test/acme/${relative}`,
      relative,
      isDir: false,
    })),
};

/** Mention indexes built so far; the module is the one the app loaded. */
const builds = (page: Page) =>
  page.evaluate(
    "import('/src/features/files/model/fileMentions.ts').then((m) => m.mentionIndexBuildCount())",
  ) as Promise<number>;

const listings = (page: Page) =>
  page.evaluate(
    () =>
      window.appBackend!.calls.filter(
        ({ command }) => command === "list_project_files",
      ).length,
  );

async function piRequests(page: Page, sessionId: string, type: string) {
  const lines = await page.evaluate(
    (sessionId) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_write" &&
            (args as { sessionId: string }).sessionId === sessionId,
        )
        .map(({ args }) => (args as { line: string }).line),
    sessionId,
  );
  return lines
    .map((line) => JSON.parse(line) as { id: string; type: string })
    .filter((request) => request.type === type);
}

/** Each session has a transcript, so opening one never replaces another tab. */
async function openSession(page: Page, title: string) {
  await page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) })
    .click();
  await expect(
    page
      .getByRole("banner")
      .getByRole("button", { name: `acme · ${title}`, exact: true }),
  ).toBeVisible();
}

async function startTurn(page: Page, app: AppShell, sessionId: string) {
  const composer = page.getByRole("textbox", {
    name: "Ask, build, / for commands",
  });
  await composer.fill("Tidy the login page");
  await composer.press("Enter");
  await expect
    .poll(async () => (await piRequests(page, sessionId, "prompt")).length)
    .toBeGreaterThan(0);
  const prompt = (await piRequests(page, sessionId, "prompt")).at(-1)!;
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

/** One completed shell call; waits until the app has rescanned the files. */
async function completeShellCall(
  page: Page,
  app: AppShell,
  sessionId: string,
  callId: string,
) {
  const before = await listings(page);
  const frame = (event: object) =>
    app.emit("harness-stdout", { sessionId, line: JSON.stringify(event) });
  await frame({
    type: "tool_execution_start",
    toolCallId: callId,
    toolName: "bash",
    args: { command: "git status" },
  });
  await frame({
    type: "tool_execution_end",
    toolCallId: callId,
    toolName: "bash",
    result: { content: [{ type: "text", text: "nothing to commit" }] },
    isError: false,
  });
  await expect
    .poll(() => listings(page), "a completed shell call rescans the files")
    .toBeGreaterThan(before);
}

test("file rescans that find the same files build no mention index", async ({
  app,
  page,
}) => {
  await page.addInitScript(() => {
    window.ade252Listing = Array.from(
      { length: 400 },
      (_, i) => `src/module-${i}/index.ts`,
    );
  });
  await app.mount(
    { projects: [PROJECT], sessions: TITLES.map(chat) },
    { handlers },
  );
  await page.evaluate(
    (ids) => {
      for (const id of ids)
        window.appBackend!.store.blocks[id] = [
          { id: `${id}-1`, role: "user", text: "Start here" },
        ];
    },
    TITLES.map((title) => chat(title).id),
  );
  for (const title of TITLES) await openSession(page, title);
  const active = chat(TITLES.at(-1)!).id;
  await startTurn(page, app, active);

  // Warm-up: the first rescan settles any load still in flight from mount.
  await completeShellCall(page, app, active, "call-warm");
  const base = await builds(page);
  expect(base, "the dev counter sees the app's own builds").toBeGreaterThan(0);

  for (let i = 0; i < UNCHANGED_TOOL_CALLS; i++) {
    await completeShellCall(page, app, active, `call-same-${i}`);
  }
  expect(
    (await builds(page)) - base,
    "rescans that find the same files build no index",
  ).toBe(0);

  await page.evaluate(() => window.ade252Listing!.push("src/login.ts"));
  await completeShellCall(page, app, active, "call-added");
  await expect
    .poll(
      async () => (await builds(page)) - base,
      "a changed file list is indexed once for all Composers, not per Composer",
    )
    .toBe(1);
});
