import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-5. A Mono starts its sessions without a tab, so opening their project
// must find a running session that is in no tab. Each test reaches the
// project through one entry point that runs the project-open rule, from
// another project, with an earlier finished session of the project open in a
// tab and remembered for it.

const HOME = "/Users/test";
const ACME = `${HOME}/acme`;
const NOTES = `${HOME}/notes`;
const MONO_CHAT = "mono-chat";
const STARTED = `app-${MONO_CHAT}-r1`;
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

/** Pi in RPC mode accepts each request and starts no turn end on its own. */
const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
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

const rail = (page: Page) => page.getByRole("navigation", { name: "Projects" });
const railProject = (page: Page, name: string) =>
  rail(page).getByRole("button", { name: new RegExp(`^${name}(,|$)`) });
/** The options menu of one rail project, opened from that project's own row. */
async function archiveFromRail(page: Page, name: string) {
  const row = railProject(page, name).locator("..");
  await row.hover();
  await row.getByRole("button", { name: "Project options" }).click();
  await page.getByRole("menuitem", { name: "Archive" }).click();
}

const sessionRow = (page: Page, title: string) =>
  page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^(?!Archive ).* ${title}$`) });

async function mountProjects(app: AppShell, page: Page) {
  await app.mount(
    {
      projects: [ACME, NOTES],
      sessions: [
        session(MONO_CHAT, HOME, "Pip"),
        session("done", ACME, "Done earlier"),
        session("notes", NOTES, "Notes work"),
        session("home", HOME, "Home chat"),
      ],
      monos: [
        {
          id: "mono-1",
          role: "mono",
          data: {
            name: "Pip",
            mascot: "mushroom",
            sessionId: MONO_CHAT,
            projects: [ACME],
            showStartedSessionsInSidebar: true,
          },
        },
      ],
    },
    { handlers },
  );
  await page.evaluate(() => {
    for (const [id, text] of [
      ["done", "Fix the docs"],
      ["notes", "Sort the notes"],
      ["home", "Plan the week"],
    ])
      window.appBackend!.store.blocks[id] = [
        { id: `${id}-u`, role: "user", text, startedAt: 1, durationMs: 1 },
      ];
  });
}

/** `monocode app sessions start` from Pip's chat, as control.rs delivers it. */
async function startFromPip(app: AppShell, page: Page) {
  await page.getByRole("button", { name: /^Pip, / }).click();
  await expect(page.getByPlaceholder("Message Pip")).toBeVisible();
  await app.emit("monocode-control-request", {
    id: "start",
    namespace: "app",
    sessionId: MONO_CHAT,
    requestId: "r1",
    action: "sessions.start",
    input: { ...PI, prompt: "Write the plan", project: "acme" },
    turnId: null,
  });
  await expect(railProject(page, "acme")).toHaveAccessibleName(/working/);
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
 * Done earlier is open in acme and remembered; Notes work is open and shown;
 * Pip's session runs in acme with no tab.
 */
async function runningWithNoTab(app: AppShell, page: Page) {
  await mountProjects(app, page);
  await railProject(page, "acme").click();
  await sessionRow(page, "Done earlier").click();
  await railProject(page, "notes").click();
  await sessionRow(page, "Notes work").click();
  await startFromPip(app, page);
  await railProject(page, "notes").click();
  await expect(page.getByRole("main").getByText("Sort the notes", { exact: true })).toBeVisible();
  await expect
    .poll(async () => {
      const saved = await savedWorkspace(page);
      return saved.held.includes(STARTED) && !saved.tabbed.includes(STARTED);
    })
    .toBe(true);
}

async function expectOnRunningSession(page: Page) {
  const main = page.getByRole("main");
  await expect(main.getByText("Write the plan", { exact: true })).toBeVisible();
  await expect(main.getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(
    page
      .getByRole("banner")
      .getByRole("button", { name: "acme · Write the plan", exact: true }),
  ).toBeVisible();
}

test("the Projects rail row opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await railProject(page, "acme").click();
  await expectOnRunningSession(page);
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
  await expectOnRunningSession(page);
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
  await expectOnRunningSession(page);
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
  await page.getByRole("banner").getByRole("button", { name: /^Project / }).click();
  await page
    .getByRole("menu", { name: "Project picker" })
    .getByRole("menuitem", { name: /acme/ })
    .click();
  await expectOnRunningSession(page);
});

test("the previous-project shortcut opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await page.keyboard.press("Meta+Shift+ArrowLeft");
  await expectOnRunningSession(page);
});

test("archiving the current project moves to the next project's running session", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await archiveFromRail(page, "notes");
  await expectOnRunningSession(page);
});

test("opening the project's folder opens the running session that has no tab", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await rail(page).getByRole("button", { name: "Open project" }).click();
  await page.getByRole("menuitem", { name: "Open folder…" }).click();
  await expectOnRunningSession(page);
});

test("Back and Forward return to the running session the project opened", async ({
  app,
  page,
}) => {
  await runningWithNoTab(app, page);
  await railProject(page, "acme").click();
  await expectOnRunningSession(page);
  await page.getByRole("button", { name: "Back (⌘[)" }).click();
  await expect(page.getByRole("main").getByText("Sort the notes", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Forward (⌘])" }).click();
  await expectOnRunningSession(page);
});
