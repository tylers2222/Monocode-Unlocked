import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { FakeStore } from "./app/backend";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

const HOME = "/Users/test";
const MONOCODE = `${HOME}/MonoCode`;
const ATLAS_FOLDER = `${MONOCODE}/gigas/atlas`;
const GIGA_CHAT = "giga-chat";
const MONO_CHAT = "mono-chat";
const PI = { harness: "pi", model: "pi:default" };

const session = (id: string, cwd: string, title: string): SessionSummary => ({
  id,
  cwd,
  harness: "pi",
  model: PI.model,
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

/** checkpoint/turn.rs: each session's last turn, kept at the cwd it began at. */
type Checkpointed = FakeStore & {
  checkpoints?: Record<string, { cwd: string; files: string[] }>;
};

declare global {
  interface Window {
    controlReplies?: { id: string; response: ControlResponse }[];
  }
}

type ControlResponse =
  { ok: true; result: { id: string } } | { ok: false; error: string };

const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_resolve_claude: () => ({ path: "/usr/local/bin/claude" }),
  harness_spawn: () => 4242,
  // Pi in RPC mode (piFamily.ts) accepts every request at once; a turn's
  // events come from `endTurnWriting`. Claude's stdin lines carry no id.
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as {
      id?: string;
      type?: string;
    };
    if (request.id)
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
  // control.rs hands the CLI caller the app's answer.
  control_reply: (_store, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  // checkpoint/turn.rs: a turn is recorded at the cwd it began at. An edit
  // path is resolved against that cwd, and one outside it is not recorded.
  // `status` finds the turn only at that same cwd (`matching_turn_review`).
  session_checkpoint_begin_turn: (store, { sessionId, cwd }) => {
    ((store as Checkpointed).checkpoints ??= {})[sessionId as string] = {
      cwd: cwd as string,
      files: [],
    };
    return null;
  },
  session_checkpoint_prepare: () => null,
  session_checkpoint_capture: (store, { sessionId, cwd, paths }) => {
    const turn = (store as Checkpointed).checkpoints?.[sessionId as string];
    if (!turn || turn.cwd !== cwd) return null;
    for (const path of paths as string[]) {
      const relative = path.startsWith("/")
        ? path.startsWith(`${cwd}/`)
          ? path.slice((cwd as string).length + 1)
          : null
        : path;
      if (relative && !turn.files.includes(relative)) turn.files.push(relative);
    }
    return null;
  },
  session_checkpoint_status: (store, { sessionId, cwd }) => {
    const turn = (store as Checkpointed).checkpoints?.[sessionId as string];
    return {
      files:
        turn && turn.cwd === cwd
          ? turn.files.map((relative) => ({
              path: `${cwd}/${relative}`,
              relative,
              status: "added",
              additions: 1,
              deletions: 0,
              exact: true,
              undoable: true,
            }))
          : [],
    };
  },
};

async function mountMonoCode(app: AppShell) {
  await app.mount(
    {
      projects: [MONOCODE],
      sessions: [
        session(GIGA_CHAT, HOME, "Atlas"),
        session(MONO_CHAT, HOME, "Pip"),
        session("docs", MONOCODE, "Fix the docs"),
      ],
      monos: [
        {
          id: "giga-1",
          role: "giga",
          data: {
            name: "Atlas",
            mascot: "ghost",
            gigaSlug: "atlas",
            sessionId: GIGA_CHAT,
            projects: [MONOCODE],
            showStartedSessionsInSidebar: true,
          },
        },
        {
          // A former Giga keeps its slug in `data` (mono.ts `setMonoGiga`).
          id: "mono-1",
          role: "mono",
          data: {
            name: "Pip",
            mascot: "mushroom",
            gigaSlug: "pip",
            sessionId: MONO_CHAT,
            projects: [MONOCODE],
            showStartedSessionsInSidebar: true,
          },
        },
      ],
    },
    { handlers },
  );
}

/**
 * `monocode app sessions start` from a Mono's chat, as control.rs delivers
 * it to the window. Returns the started session's id.
 */
async function startFromChat(
  page: Page,
  app: AppShell,
  mono: "Atlas" | "Pip",
  prompt: string,
) {
  await page.getByRole("button", { name: new RegExp(`^${mono}, `) }).click();
  await expect(page.getByPlaceholder(`Message ${mono}`)).toBeVisible();
  const id = `start-${mono}`;
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId: mono === "Atlas" ? GIGA_CHAT : MONO_CHAT,
    requestId: "r1",
    action: "sessions.start",
    input: { ...PI, prompt, project: "MonoCode" },
    turnId: null,
  });
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.controlReplies?.find((reply) => reply.id === id),
        id,
      ),
    )
    .toMatchObject({ response: { ok: true } });
  const reply = await page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!,
    id,
  );
  return (reply.response as { result: { id: string } }).result.id;
}

/** The cwd of each agent process the app started for this session. */
function spawnCwds(page: Page, sessionId: string) {
  return page.evaluate(
    (sessionId) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_spawn" &&
            (args as { sessionId: string }).sessionId === sessionId,
        )
        .map(({ args }) => (args as { cwd: string }).cwd),
    sessionId,
  );
}

/** Each Claude Code session the app started, other than its own probes. */
function claudeSpawns(page: Page) {
  return page.evaluate(() =>
    window
      .appBackend!.calls.filter(({ command, args }) => {
        const spawn = args as { sessionId: string; command: string };
        return (
          command === "harness_spawn" &&
          spawn.command.endsWith("/claude") &&
          !spawn.sessionId.startsWith("monocode-")
        );
      })
      .map(({ args }) => (args as { cwd: string }).cwd),
  );
}

/** Pi's events for the session's running turn: it writes `path`, then ends. */
async function endTurnWriting(
  page: Page,
  app: AppShell,
  sessionId: string,
  path: string,
) {
  await expect.poll(() => spawnCwds(page, sessionId)).not.toEqual([]);
  for (const line of [
    { type: "agent_start" },
    {
      type: "tool_execution_start",
      toolCallId: "write-1",
      toolName: "write",
      args: { path, content: "# Plan" },
    },
    {
      type: "tool_execution_end",
      toolCallId: "write-1",
      toolName: "write",
      result: { content: [{ type: "text", text: `Wrote ${path}` }] },
      isError: false,
    },
    { type: "agent_end", willRetry: false },
  ])
    await app.emit("harness-stdout", {
      sessionId,
      line: JSON.stringify(line),
    });
}

/** Picks Claude Sonnet 5 from the turn's "Second opinion" or "Handoff" menu. */
async function sendTurnToClaude(
  page: Page,
  button: "Second opinion" | "Handoff",
) {
  await page.getByRole("button", { name: button, exact: true }).click();
  await page
    .getByRole("menuitem", { name: "Claude Code", exact: true })
    .hover();
  await page
    .getByRole("menu", { name: "Claude Code models" })
    .getByRole("menuitem", { name: "Claude Sonnet 5", exact: true })
    .click();
}

/** Opens the MonoCode project, then the session with this title. */
async function openInMonoCode(page: Page, title: string) {
  await page
    .getByRole("navigation", { name: "Projects" })
    .getByRole("button", { name: /^MonoCode/ })
    .click();
  await page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) })
    .click();
  await expect(tab(page, title)).toBeVisible();
}

const tab = (page: Page, title: string) =>
  page
    .getByRole("banner")
    .getByRole("button", { name: `MonoCode · ${title}`, exact: true });

/** The composer's working-copy label: the main checkout, not a worktree. */
const currentCheckout = (page: Page) =>
  page
    .getByRole("main")
    .getByText("Current checkout", { exact: true })
    .filter({ visible: true });

test("a session a Giga starts runs in the Giga's folder and its tab stays in the MonoCode deck", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Atlas", "Write the plan");

  await expect.poll(() => spawnCwds(page, id)).toEqual([ATLAS_FOLDER]);

  await openInMonoCode(page, "Write the plan");
  await expect(currentCheckout(page)).toBeVisible();
  await openInMonoCode(page, "Fix the docs");
  await expect(tab(page, "Write the plan")).toBeVisible();
  await tab(page, "Write the plan").click();
  await expect(currentCheckout(page)).toBeVisible();
});

test("a session a plain Mono starts runs at the project root", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Pip", "Write the plan");

  await expect.poll(() => spawnCwds(page, id)).toEqual([MONOCODE]);
  expect(
    await page.evaluate((id) => window.appBackend!.store.workFolders[id], id),
  ).toBeUndefined();
});

test("a Giga session's checkpoint and changed-files card stay at the MonoCode checkout", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Atlas", "Write the plan");
  await endTurnWriting(page, app, id, "plan.md");

  await openInMonoCode(page, "Write the plan");
  await expect(page.getByText("Changed 1 file")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "gigas/atlas/plan.md +1 -0" }),
  ).toBeVisible();
});

test("a second opinion on a Giga session runs in the Giga's folder", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Atlas", "Write the plan");
  await endTurnWriting(page, app, id, "plan.md");
  await openInMonoCode(page, "Write the plan");

  await sendTurnToClaude(page, "Second opinion");

  await expect.poll(() => claudeSpawns(page)).toEqual([ATLAS_FOLDER]);
});

test("a handoff from a Giga session runs in the Giga's folder", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Atlas", "Write the plan");
  await endTurnWriting(page, app, id, "plan.md");
  await openInMonoCode(page, "Write the plan");

  await sendTurnToClaude(page, "Handoff");
  await page
    .getByRole("button", { name: "Send", exact: true, disabled: false })
    .click();

  await expect.poll(() => claudeSpawns(page)).toEqual([ATLAS_FOLDER]);
});

test("after a restart a Giga session keeps its folder and its tab in the MonoCode deck", async ({
  app,
  page,
}) => {
  await mountMonoCode(app);
  const id = await startFromChat(page, app, "Atlas", "Write the plan");
  await endTurnWriting(page, app, id, "plan.md");
  await openInMonoCode(page, "Write the plan");

  await app.restart();

  await expect(tab(page, "Write the plan")).toBeVisible();
  await tab(page, "Write the plan").click();
  await expect(currentCheckout(page)).toBeVisible();
  const composer = page.getByRole("textbox", { name: /^Ask, build/ });
  await composer.fill("Now the timeline");
  await composer.press("Enter");
  await expect.poll(() => spawnCwds(page, id)).toEqual([ATLAS_FOLDER]);
});
