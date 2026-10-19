import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-5. Each case opens the app on the reduced real database
// (`untabbedRunning.realdb.json`: what session_store.rs returned for a copy of
// the shipped app's database, ids and text made up). Its four running sessions
// were started by a Mono or a Giga. The app resumes them after the restart and
// the user closes their tabs, which leaves them running with no tab, as in the
// shipped app. Then one entry point opens the project.

const HOME = "/Users/test";
const ACME = `${HOME}/acme`;
const NOTES = `${HOME}/notes`;

type Fixture = {
  snapshot: { sessions: { id: string }[] };
  inFlight: { sessionId: string; cwd: string }[];
  records: Record<
    string,
    SessionSummary & { blocks: Block[]; workFolder?: string; title: string }
  >;
};
const realDb = JSON.parse(
  readFileSync(
    new URL(
      "../../src/features/projects/model/fixtures/untabbedRunning.realdb.json",
      import.meta.url,
    ),
    "utf8",
  ),
) as Fixture;
const records = Object.values(realDb.records);
const titleOf = (id: string) => realDb.records[id].title;
const running = realDb.inFlight.map((ref) => ref.sessionId);
const runningIn = (project: string) =>
  realDb.inFlight
    .filter((ref) => ref.cwd === project)
    .map((ref) => titleOf(ref.sessionId));
/** Any running acme session: the newest turn wins, and resuming restarted them all. */
const RUNNING_ACME = new RegExp(`^Close (${runningIn(ACME).join("|")})$`);

/** Omp in RPC mode accepts each request; no turn ends on its own. */
const handlers: MountOptions["handlers"] = {
  harness_resolve_omp: () => ({ path: "/usr/local/bin/omp" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id?: string; type?: string };
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
  // fs.ts: no omp transcript on disk to backfill from.
  omp_session_interjections: () => [],
  omp_active_assistant_texts: () => [],
  session_checkpoint_keep: () => null,
  control_reply: () => null,
  "plugin:dialog|open": () => "/Users/test/acme",
  // session_store.rs search_sessions, as far as the one saved chat at home.
  session_search: (_store, { options }) => ({
    hits: String((options as { query: string }).query)
      .toLowerCase()
      .includes("home")
      ? [
          {
            kind: "conversation",
            sessionId: "home",
            cwd: "/Users/test",
            harness: "pi",
            title: "Home chat",
            updatedAt: 1,
            preview: "Home chat",
          },
        ]
      : [],
    truncated: false,
  }),
  cancel_session_search: () => null,
  // search.rs: no file in these projects matches.
  search_project: () => ({ matches: [], truncated: false }),
  cancel_project_search: () => null,
};

function summary(record: Fixture["records"][string]): SessionSummary {
  const {
    blocks: _blocks,
    modelSettings: _modelSettings,
    queuedMessages: _queued,
    contextUsed: _used,
    contextWindow: _window,
    workFolder: _workFolder,
    ...rest
  } = record as Fixture["records"][string] & Record<string, unknown>;
  return rest;
}

const rail = (page: Page) => page.getByRole("navigation", { name: "Projects" });
const railProject = (page: Page, name: string) =>
  rail(page).getByRole("button", { name: new RegExp(`^${name}(,|$)`) });
const banner = (page: Page) => page.getByRole("banner");
const closeTab = (page: Page, title: string) =>
  banner(page).getByRole("button", { name: `Close ${title}`, exact: true });

/** The options menu of one rail project, opened from that project's own row. */
async function archiveFromRail(page: Page, name: string) {
  const row = railProject(page, name).locator("..");
  await row.hover();
  await row.getByRole("button", { name: "Project options" }).click();
  await page.getByRole("menuitem", { name: "Archive" }).click();
}

/** Pip's chat, which the live-agents rail and the project rule leave out. */
const PIP_CHAT = "mono-chat-0001";

/**
 * Pip (a Mono of acme) has one habit, and Pip's chat comes back from the
 * restart with a turn of its own in acme. mono.rs serves habits.json and
 * keeps what the app saves to it, so the panel's Run now reaches the app's own
 * habit scheduler.
 */
const excludedRunning: MountOptions["handlers"] = {
  mono_read: (_store, { mono, path }) =>
    mono === "mono-1" && path === "habits.json"
      ? {
          text:
            (window as { habitsText?: string }).habitsText ??
            JSON.stringify([
              {
                id: "habit-1",
                name: "Tidy the docs",
                instructions: "Tidy the docs folder.",
                schedule: { scheduleKind: "daily", time: "09:00" },
                enabled: true,
                createdAt: 1,
                nextRunAt: 4_102_444_800_000,
              },
            ]),
          hash: "habits",
        }
      : {
          text: null,
          hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        },
  mono_save: (_store, { path, text }) => {
    if (path === "habits.json")
      (window as { habitsText?: string }).habitsText = text as string;
    return "saved";
  },
};

async function mountRealDb(app: AppShell, withExcludedRunning = false) {
  const pipInAcme = (session: SessionSummary) =>
    withExcludedRunning && session.id === PIP_CHAT ? { ...session, cwd: ACME } : session;
  await app.mount(
    {
      projects: [ACME, NOTES],
      sessions: [
        ...records.map(summary).map(pipInAcme),
        {
          id: "home",
          cwd: HOME,
          harness: "pi",
          model: "pi:default",
          runtimeMode: "supervised",
          title: "Home chat",
          createdAt: 1,
          updatedAt: 1,
        },
      ],
      blocks: {
        ...Object.fromEntries(records.map((record) => [record.id, record.blocks])),
        home: [{ id: "home-u", role: "user", text: "Plan the week", startedAt: 1, durationMs: 1 }],
        ...(withExcludedRunning
          ? {
              [PIP_CHAT]: [
                { id: "pip-u", role: "user", text: "Check acme", startedAt: Date.now() },
              ],
            }
          : {}),
      },
      workFolders: Object.fromEntries(
        records.flatMap((record) =>
          record.workFolder ? [[record.id, record.workFolder]] : [],
        ),
      ),
      workspace: realDb.snapshot,
      inFlight: withExcludedRunning
        ? [...realDb.inFlight, { sessionId: PIP_CHAT, cwd: ACME }]
        : realDb.inFlight,
      monos: [
        {
          id: "mono-1",
          role: "mono",
          data: {
            name: "Pip",
            mascot: "mushroom",
            sessionId: "mono-chat-0001",
            projects: [ACME],
            showStartedSessionsInSidebar: true,
          },
        },
        {
          id: "giga-1",
          role: "giga",
          data: {
            name: "Atlas",
            mascot: "ghost",
            gigaSlug: "giga-one",
            sessionId: "giga-chat-0001",
            projects: [NOTES],
            showStartedSessionsInSidebar: true,
          },
        },
      ],
    },
    { handlers: withExcludedRunning ? { ...handlers, ...excludedRunning } : handlers },
  );
}

/** The saved workspace: which sessions the window holds and which have a tab. */
function savedWorkspace(page: Page) {
  return page.evaluate(() => {
    const saved = window.appBackend!.store.workspace as {
      sessions: { id: string }[];
      tabs: { layout: unknown }[];
    } | null;
    return {
      held: saved?.sessions.map((stub) => stub.id) ?? [],
      tabbed: JSON.stringify(saved?.tabs ?? []),
    };
  });
}

/**
 * Every running session runs with no tab, and an idle notes tab is shown.
 * The resumed sessions come back in tabs; closing a tab keeps its turn
 * running, which is one way the shipped app reached this state.
 */
async function runningWithNoTab(
  app: AppShell,
  page: Page,
  withExcludedRunning = false,
) {
  await mountRealDb(app, withExcludedRunning);
  await expect(page.getByText(`${running.length} working agents`)).toBeVisible();
  for (const title of runningIn(ACME)) await closeTab(page, title).click();
  await railProject(page, "notes").click();
  for (const title of runningIn(NOTES)) await closeTab(page, title).click();
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toHaveCount(0);
  await expect
    .poll(async () => {
      const saved = await savedWorkspace(page);
      return running.every(
        (id) => saved.held.includes(id) && !saved.tabbed.includes(id),
      );
    })
    .toBe(true);
  await expect(page.getByText(`${running.length} working agents`)).toBeVisible();
}

/** As `runningWithNoTab`, then back on the idle acme tab that was shown last. */
async function onIdleAcmeTab(app: AppShell, page: Page) {
  await runningWithNoTab(app, page);
  await page.getByRole("button", { name: "Back (⌘[)" }).click();
  await expect(banner(page).getByRole("button", { name: /^acme · / }).first()).toBeVisible();
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toHaveCount(0);
}

async function expectOnRunningAcmeSession(page: Page) {
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(banner(page).getByRole("button", { name: RUNNING_ACME })).toHaveCount(1);
}

test("the Projects rail row opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await railProject(page, "acme").click();
  await expectOnRunningAcmeSession(page);
});

test("the rail row on an idle tab of the project opens its running session", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await railProject(page, "acme").click();
  await expectOnRunningAcmeSession(page);
});

/** The sessions this window last wrote to session_set_in_flight as running. */
function inFlightIds(page: Page) {
  return page.evaluate(() => {
    const writes = window.appBackend!.calls.filter(
      ({ command }) => command === "session_set_in_flight",
    );
    const last = writes[writes.length - 1]?.args as
      | { sessions: { sessionId: string }[] }
      | undefined;
    return last?.sessions.map((ref) => ref.sessionId) ?? [];
  });
}

test("the rail row does not open a running habit run or Mono chat of the project", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page, true);
  // Started after the resumed turns, so the habit run's turn is the newest
  // in acme and only its exclusion keeps the rule off it.
  await page.getByRole("button", { name: /^Pip, / }).click();
  await page.getByRole("tab", { name: "Habits", exact: true }).click();
  const habit = page.locator('[data-habit="habit-1"]');
  await habit.hover();
  await habit.getByRole("button", { name: "Run now" }).click();
  await expect
    .poll(() => inFlightIds(page))
    .toEqual(
      expect.arrayContaining([PIP_CHAT, expect.stringMatching(/^app-mono-chat-0001-habit-/)]),
    );
  await railProject(page, "acme").click();
  await expectOnRunningAcmeSession(page);
});

test("the compact rail's project picker opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await page.getByRole("button", { name: "Toggle Projects" }).click();
  await page
    .getByRole("button", { name: "Switch project, current project notes" })
    .click();
  await page
    .getByRole("dialog", { name: "Project picker" })
    .getByRole("button", { name: /^acme / })
    .click();
  await expectOnRunningAcmeSession(page);
});

test("the compact rail's picker on an idle tab of the project opens its running session", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await page.getByRole("button", { name: "Toggle Projects" }).click();
  await page
    .getByRole("button", { name: "Switch project, current project acme" })
    .click();
  await page
    .getByRole("dialog", { name: "Project picker" })
    .getByRole("button", { name: /^acme / })
    .click();
  await expectOnRunningAcmeSession(page);
});

test("the sidebar's project picker opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem("monocode.collapsedProjectRailMode", "hidden"),
  );
  await runningWithNoTab(app, page);
  await page.getByRole("button", { name: "Toggle Projects" }).click();
  await page
    .getByRole("complementary")
    .getByRole("button", { name: /notes/ })
    .first()
    .click();
  await page
    .getByRole("dialog", { name: "Project picker" })
    .getByRole("button", { name: /^acme / })
    .click();
  await expectOnRunningAcmeSession(page);
});

// The title bar offers projects only while no project is open, as on a chat
// saved at home, which Search opens.
test("the title bar's project picker opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await rail(page).getByRole("button", { name: /^Search/ }).click();
  // Search clears its query when it opens, then focuses the box.
  await expect(page.getByRole("textbox", { name: "Search" })).toBeFocused();
  await page.getByRole("textbox", { name: "Search" }).pressSequentially("Home");
  await page
    .getByRole("listbox", { name: "Search results" })
    .getByRole("option", { name: /^Home chat/ })
    .click();
  await expect(page.getByRole("main").getByText("Plan the week", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Toggle Projects" }).click();
  await banner(page).getByRole("button", { name: /^Project / }).click();
  await page
    .getByRole("menu", { name: "Project picker" })
    .getByRole("menuitem", { name: /acme/ })
    .click();
  await expectOnRunningAcmeSession(page);
});

test("a project hit in Search opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await rail(page).getByRole("button", { name: /^Search/ }).click();
  await expect(page.getByRole("textbox", { name: "Search" })).toBeFocused();
  await page.getByRole("textbox", { name: "Search" }).pressSequentially("acme");
  await page
    .getByRole("listbox", { name: "Search results" })
    .getByRole("option", { name: /^acme/ })
    .click();
  await expectOnRunningAcmeSession(page);
});

test("the previous-project shortcut opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await page.keyboard.press("Meta+Shift+ArrowLeft");
  await expectOnRunningAcmeSession(page);
});

// Archiving acme moves to the most recent project left, notes, whose
// Giga-started session runs with no tab.
test("archiving the current project moves to the next project's running session", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await archiveFromRail(page, "acme");
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(
    closeTab(page, runningIn(NOTES)[0]),
  ).toBeVisible();
});

test("restoring an archived project in Settings opens its running session", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await archiveFromRail(page, "acme");
  await expect(railProject(page, "acme")).toHaveCount(0);
  await rail(page).getByRole("button", { name: /^Settings/ }).click();
  await rail(page).getByRole("button", { name: "Archive", exact: true }).click();
  await page
    .getByRole("region", { name: "Settings" })
    .getByRole("button", { name: /Restore/ })
    .first()
    .click();
  await rail(page).getByRole("button", { name: "Back", exact: true }).click();
  await expectOnRunningAcmeSession(page);
});

test("opening the project's folder opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await rail(page).getByRole("button", { name: "Open project" }).click();
  await page.getByRole("menuitem", { name: "Open folder…" }).click();
  await expectOnRunningAcmeSession(page);
});

test("Back and Forward return to the running session the project opened", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await railProject(page, "acme").click();
  await expectOnRunningAcmeSession(page);
  await page.getByRole("button", { name: "Back (⌘[)" }).click();
  await expect(banner(page).getByRole("button", { name: /^notes · / }).first()).toBeVisible();
  await page.getByRole("button", { name: "Forward (⌘])" }).click();
  await expectOnRunningAcmeSession(page);
});
