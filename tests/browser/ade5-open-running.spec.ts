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
  // control.rs: a control request comes from the session's live turn.
  control_owns_session: () => true,
  // mono_messages.rs: no message waits for any session.
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
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
  // worktrees.rs list: each project's checkout and the worktrees its saved
  // sessions work in, as git lists them for the real repositories.
  git_worktrees: (store, { cwd }) => {
    const tree = (path: string, isMain: boolean) => ({
      path,
      branch: isMain ? "main" : path.split("/").pop(),
      head: "0000000000000000000000000000000000000000",
      isMain,
      locked: false,
      prunable: false,
      missing: false,
      dirty: false,
      unpushed: 0,
      sessionIds: store.sessions
        .filter((session) => (session.worktreeCwd || session.cwd) === path)
        .map((session) => session.id),
    });
    const worktrees = [
      ...new Set(
        store.sessions
          .filter((session) => session.cwd === cwd && session.worktreeCwd)
          .map((session) => session.worktreeCwd as string),
      ),
    ];
    return {
      worktrees: [tree(cwd as string, true), ...worktrees.sort().map((path) => tree(path, false))],
      defaultRoot: `${cwd}-worktrees`,
    };
  },
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

/** An idle acme session in a worktree no running acme session uses. */
const OTHER_WORKTREE_RECORD = records.find(
  (record) =>
    record.cwd === ACME &&
    record.worktreeCwd &&
    !realDb.inFlight.some(
      (ref) => realDb.records[ref.sessionId].worktreeCwd === record.worktreeCwd,
    ),
)!;
const OTHER_WORKTREE_SESSION = OTHER_WORKTREE_RECORD.title;
const OTHER_WORKTREE_TREE = OTHER_WORKTREE_RECORD.worktreeCwd!.split("/").pop()!;

/**
 * session_store.rs list_by_project leaves orchestration workers out
 * (`id NOT IN orchestration_workers`); they show in their lead's panel.
 */
const workerHandlers: MountOptions["handlers"] = {
  session_list_by_project: (store, { cwd }) =>
    store.sessions
      .filter((session) => session.cwd === cwd && !session.id.includes("-worker-"))
      .sort((a, b) => b.updatedAt - a.updatedAt || (a.id < b.id ? -1 : 1))
      .map((session) => ({ ...session, branch: session.branch || "main" })),
};

/** Pip's chat, which the live-agents rail and the project rule leave out. */
const PIP_CHAT = "mono-chat-0001";

/**
 * Pip (a Mono of acme) has one habit, and Pip's chat comes back from the
 * restart with a turn of its own in acme. mono.rs serves habits.json and
 * keeps what the app saves to it, so the panel's Run now reaches the app's own
 * habit scheduler.
 */
const excludedRunningHandlers: MountOptions["handlers"] = {
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

/** An idle acme lead in a worktree, and the worker it runs through. */
const LEAD = realDb.snapshot.tabs
  .map((tab) => realDb.records[tab.focusedId])
  .find(
    (record) =>
      record.cwd === ACME &&
      record.worktreeCwd &&
      !running.includes(record.id) &&
      record.title !== OTHER_WORKTREE_SESSION,
  )!;
const WORKER_ID = `${LEAD.id}-worker-1`;
const WORKER_TREE = `${ACME}-worktrees/orchestration-task-1`;
/**
 * The worker as the app saves it when an orchestration run attaches it
 * (App.tsx `synced`): the lead's project as `cwd`, its own worktree and
 * branch, the lead's runtime mode, and `orchestrationLeadId`, which the
 * session store keeps on the first user block (`sanitizeSessionForPersist`).
 */
const workerRecord: SessionSummary = {
  id: WORKER_ID,
  cwd: ACME,
  harness: LEAD.harness,
  model: LEAD.model,
  runtimeMode: LEAD.runtimeMode,
  title: "Write the migration",
  worktreeCwd: WORKER_TREE,
  branch: "orchestration/task-1",
  providerSessionId: "provider-worker-1",
  createdAt: 1,
  updatedAt: 1,
};
const workerBlocks: Block[] = [
  {
    id: "worker-u",
    role: "user",
    text: "Write the migration",
    startedAt: 1,
    turnId: "worker-turn",
    orchestrationLeadId: LEAD.id,
  },
  { id: "worker-a", role: "assistant", text: "Working on it" },
];

type RealDbOptions = {
  /** Pip's chat and Pip's habit run also in acme (both excluded). */
  excludedRunning?: boolean;
  /** Only an orchestration worker of LEAD runs in acme. */
  runningWorker?: boolean;
};

async function mountRealDb(
  app: AppShell,
  { excludedRunning = false, runningWorker = false }: RealDbOptions = {},
) {
  const pipInAcme = (session: SessionSummary) =>
    excludedRunning && session.id === PIP_CHAT ? { ...session, cwd: ACME } : session;
  await app.mount(
    {
      projects: [ACME, NOTES],
      sessions: [
        ...records.map(summary).map(pipInAcme),
        ...(runningWorker ? [workerRecord] : []),
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
        ...(runningWorker ? { [WORKER_ID]: workerBlocks } : {}),
        ...(excludedRunning
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
      inFlight: runningWorker
        ? [
            { sessionId: WORKER_ID, cwd: ACME },
            ...realDb.inFlight.filter((ref) => ref.cwd !== ACME),
          ]
        : excludedRunning
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
    {
      handlers: {
        ...handlers,
        ...(excludedRunning ? excludedRunningHandlers : {}),
        ...(runningWorker ? workerHandlers : {}),
      },
    },
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
  {
    excludedRunning = false,
    acmeAfterMonoCode = false,
  }: { excludedRunning?: boolean; acmeAfterMonoCode?: boolean } = {},
) {
  await mountRealDb(app, { excludedRunning });
  await expect(page.getByText(`${running.length} working agents`)).toBeVisible();
  if (acmeAfterMonoCode) {
    // The Giga's resumed turn adds its MonoCode workspace to the projects;
    // choosing acme while its sessions still have tabs puts acme after it.
    await expect(railProject(page, "MonoCode")).toBeVisible();
    await railProject(page, "acme").click();
  }
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

const sidebarRow = (page: Page, title: string) =>
  page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) });

/**
 * On a running acme session, and the project is narrowed to that session's
 * worktree (ADE-12): the sidebar lists the session and no session of another
 * worktree.
 */
async function expectOnRunningAcmeSession(page: Page) {
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toBeVisible();
  const close = banner(page).getByRole("button", { name: RUNNING_ACME });
  await expect(close).toHaveCount(1);
  const title = (await close.getAttribute("aria-label"))!.slice("Close ".length);
  await expect(sidebarRow(page, title)).toBeVisible();
  await expect(sidebarRow(page, OTHER_WORKTREE_SESSION)).toHaveCount(0);
  return records.find((record) => record.title === title)!.worktreeCwd!;
}

test("the Projects rail row opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await railProject(page, "acme").click();
  const worktree = await expectOnRunningAcmeSession(page);
  // The working copy shown is that worktree, not the project folder.
  await expect(
    page.getByRole("complementary").getByRole("button", { name: "Switch working copy" }),
  ).toHaveAttribute("title", new RegExp(`\\n~${worktree.slice(HOME.length)}$`));
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
  await runningWithNoTab(app, page, { excludedRunning: true });
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

test("a sidebar session row opens a running session that has no tab in its worktree", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await sidebarRow(page, runningIn(ACME)[0]).click();
  await expectOnRunningAcmeSession(page);
});

test("the rail row from a new session in another project opens the running session in its worktree", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  // A blank session's tab takes the running session in place of its pane.
  await page.getByRole("button", { name: "New session (⌘T)" }).click();
  await expect(page.getByRole("heading", { name: "What should we work on in notes?" })).toBeVisible();
  await railProject(page, "acme").click();
  await expectOnRunningAcmeSession(page);
});

test("a sidebar session row opens an idle session that has no tab in the workspace on screen", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await closeTab(page, OTHER_WORKTREE_SESSION).click();
  await expect(closeTab(page, OTHER_WORKTREE_SESSION)).toHaveCount(0);
  await sidebarRow(page, OTHER_WORKTREE_SESSION).click();
  const other = records.find((record) => record.title === OTHER_WORKTREE_SESSION)!;
  const opener = other.blocks.find((block) => block.role === "user")!.text;
  await expect(page.getByRole("main").getByText(opener, { exact: true })).toBeVisible();
  // It joins the project folder's workspace: every worktree stays listed.
  await expect(sidebarRow(page, runningIn(ACME)[0])).toBeVisible();
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

// Archiving notes moves to the most recent project left, acme, whose
// Mono-started sessions run in worktrees with no tab.
test("archiving the current project moves to the next project's running session", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page, { acmeAfterMonoCode: true });
  await archiveFromRail(page, "notes");
  await expectOnRunningAcmeSession(page);
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


const switcher = (page: Page) =>
  page.getByRole("complementary").getByRole("button", { name: "Switch working copy" });

/** The worker of LEAD runs; LEAD and the worker have no tab; notes is shown. */
async function leadOfRunningWorkerWithNoTab(app: AppShell, page: Page) {
  await mountRealDb(app, { runningWorker: true });
  await expect.poll(() => inFlightIds(page)).toContain(WORKER_ID);
  // A worker shows in its lead's panel; the restart gave it a tab to resume in.
  await closeTab(page, workerRecord.title).click();
  await closeTab(page, LEAD.title).click();
  await railProject(page, "notes").click();
  await expect(closeTab(page, LEAD.title)).toHaveCount(0);
  await expect(closeTab(page, workerRecord.title)).toHaveCount(0);
  await expect.poll(() => inFlightIds(page)).toContain(WORKER_ID);
}

test("the rail row opens the lead of a running worker in the lead's worktree", async ({
  app,
  page,
}) => {
  await leadOfRunningWorkerWithNoTab(app, page);
  await railProject(page, "acme").click();
  await expect(closeTab(page, LEAD.title)).toHaveCount(1);
  await expect(switcher(page)).toHaveAttribute(
    "title",
    new RegExp(`\\n~${LEAD.worktreeCwd!.slice(HOME.length)}$`),
  );
  await expect(sidebarRow(page, LEAD.title)).toBeVisible();
  await expect(sidebarRow(page, OTHER_WORKTREE_SESSION)).toHaveCount(0);
});

const STARTED_TITLE = "Tidy the changelog";

/** `monocode app sessions start` from Pip's chat: no tab, the acme folder. */
async function startInAcmeFolder(app: AppShell, page: Page) {
  await page.getByRole("button", { name: /^Pip, / }).click();
  await expect(page.getByPlaceholder("Message Pip")).toBeVisible();
  await app.emit("monocode-control-request", {
    id: "start",
    namespace: "app",
    sessionId: PIP_CHAT,
    requestId: "r1",
    action: "sessions.start",
    input: { harness: LEAD.harness, model: LEAD.model, prompt: STARTED_TITLE, project: "acme" },
    turnId: "turn-1",
  });
  await expect
    .poll(() => inFlightIds(page))
    .toContain(`app-${PIP_CHAT}-r1`);
}

test("the rail row moves a worktree-focused project to its running session in the project folder", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  // Focus acme on an idle worktree, then let Pip start a session that runs
  // in the acme folder with no tab; its turn is the newest in acme.
  await railProject(page, "acme").click();
  await switcher(page).click();
  await page
    .getByRole("listbox", { name: "Working copies" })
    .getByRole("option", { name: new RegExp(`^${OTHER_WORKTREE_TREE} `) })
    .click();
  await expect(switcher(page)).toHaveAttribute("title", new RegExp(`${OTHER_WORKTREE_TREE}$`));
  await startInAcmeFolder(app, page);
  await railProject(page, "notes").click();
  await railProject(page, "acme").click();
  await expect(closeTab(page, STARTED_TITLE)).toHaveCount(1);
  await expect(switcher(page)).toHaveAttribute("title", "main");
});

test("archiving the current project moves to the next project's running session in its folder", async ({
  app,
  page,
}) => {
  await onIdleAcmeTab(app, page);
  await archiveFromRail(page, "acme");
  await expect(page.getByRole("main").getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(closeTab(page, runningIn(NOTES)[0])).toBeVisible();
  await expect(switcher(page)).toHaveAttribute("title", "main");
});
