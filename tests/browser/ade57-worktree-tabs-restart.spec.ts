import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import type { Block } from "../../src/features/sessions/model/session";
import type { WorkspaceSnapshot } from "../../src/features/workspace/model/workspaceSnapshot";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";

// ADE-57. Each case opens the app on the reduced real database
// (`worktreeTabsRestart.realdb.json`: the saved tabs of ~/Desktop/monocode and
// what session_store.rs returned for them, from a copy of the shipped app's
// database, ids and text made up). The user opens one of the project's idle
// worktree sessions in its worktree, then the app restarts, reloads, closes
// or crashes.

type Record_ = SessionSummary & { blocks: Block[]; title: string };
const realDb = JSON.parse(
  readFileSync(
    new URL(
      "../../src/features/workspace/model/fixtures/worktreeTabsRestart.realdb.json",
      import.meta.url,
    ),
    "utf8",
  ),
) as {
  snapshot: { projectCwd: string; tabs: { id: string; focusedId: string }[] };
  railSessions: string[];
  records: Record<string, Record_>;
};
const records = Object.values(realDb.records);
const PROJECT = realDb.snapshot.projectCwd;
/** feat/ade-50-approvals-table: one idle session, no tab. */
const OPENED = realDb.records[realDb.railSessions[0]];
const TREE = OPENED.branch!;

/** Omp in RPC mode accepts each request; no turn ends on its own. */
const handlers: MountOptions["handlers"] = {
  harness_resolve_omp: () => ({ path: "/usr/local/bin/omp" }),
  harness_spawn: () => 4242,
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
  omp_session_interjections: () => [],
  omp_active_assistant_texts: () => [],
  session_checkpoint_keep: () => null,
  control_owns_session: () => true,
  mono_messages_unqueued: () => [],
  mono_messages_pending_notices: () => [],
  // reservedProjectName.ts tells the user once that a project named
  // "monocode" shares the app's name; the dialog answers with nothing.
  "plugin:dialog|message": () => null,
  // window.rs: close-requested asks whether a Mono chat keeps the app alive,
  // then destroys the window; the page itself stays for the reopen.
  mono_chat_keep_alive: () => false,
  destroy_window: () => null,
  // worktrees.rs list: the checkout and the worktrees its sessions work in.
  git_worktrees: (store, { cwd }) => {
    const tree = (path: string, branch: string, isMain: boolean) => ({
      path,
      branch,
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
    const trees = new Map(
      store.sessions
        .filter((session) => session.cwd === cwd && session.worktreeCwd)
        .map((session) => [session.worktreeCwd!, session.branch ?? "detached"]),
    );
    return {
      worktrees: [
        tree(cwd as string, "main", true),
        ...[...trees].sort().map(([path, branch]) => tree(path, branch, false)),
      ],
      defaultRoot: `${cwd}-worktrees`,
    };
  },
};

async function mount(app: AppShell, workspace: unknown = realDb.snapshot) {
  const blocks: Record<string, Block[]> = {};
  const sessions = records.map(({ blocks: saved, ...summary }) => {
    blocks[summary.id] = saved;
    return summary;
  });
  await app.mount(
    {
      home: "/Users/test",
      projects: [PROJECT],
      sessions,
      blocks,
      workspace,
    },
    { handlers },
  );
}

const banner = (page: Page) => page.getByRole("banner");
const tab = (page: Page, title: string) =>
  banner(page).getByRole("button", {
    name: `monocode · ${title}`,
    exact: true,
  });

/** The tabs of the workspace on screen, in strip order. */
const strip = (page: Page) =>
  banner(page)
    .getByRole("button", { name: /^monocode · / })
    .evaluateAll((buttons) =>
      buttons.map((button) => button.getAttribute("aria-label")),
    );

const switcher = (page: Page) =>
  page.getByRole("button", { name: "Switch working copy" });
const worktreeRow = (page: Page) =>
  page
    .getByRole("listbox", { name: "Working copies" })
    .getByRole("option", { name: new RegExp(TREE) });

async function pickWorktree(page: Page) {
  await switcher(page).click();
  await worktreeRow(page).click();
  await expect(switcher(page)).toHaveText(TREE);
}

/** From the default workspace, open OPENED in its own worktree. */
async function openInWorktree(page: Page) {
  await pickWorktree(page);
  await page
    .getByRole("complementary")
    .getByRole("button", {
      name: new RegExp(`^(?!Archive ).* ${OPENED.title}$`),
    })
    .click();
  await expect(tab(page, OPENED.title)).toBeVisible();
  await expect.poll(() => strip(page)).toEqual([`monocode · ${OPENED.title}`]);
}

/**
 * The saved workspace holds OPENED's tab in its worktree. Picking an empty
 * worktree first opens a blank tab there, which a save can catch before
 * OPENED replaces it, so any worktree entry is not enough.
 */
const savedInWorktree = (page: Page) =>
  page.evaluate((id) => {
    // The fake store keeps what saveWorkspaceSnapshot wrote.
    const saved = window.appBackend!.store
      .workspace as WorkspaceSnapshot | null;
    const tab = saved?.tabs.find((entry) => entry.focusedId === id);
    return (
      !!tab && !!saved?.tabWorkspaces?.some(({ tabId }) => tabId === tab.id)
    );
  }, OPENED.id);

async function booted(page: Page) {
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible({
    timeout: 30_000,
  });
}

/**
 * Runs one App save path alone: waits for the normal-use save, empties the
 * saved workspace (that save skips a key it already wrote), runs `save`, and
 * waits for it to write the worktree tab back. Then reopens on that store.
 */
async function saveAloneAndReopen(page: Page, save: () => Promise<unknown>) {
  await expect.poll(() => savedInWorktree(page)).toBe(true);
  await page.evaluate(() => {
    window.appBackend!.store.workspace = null;
  });
  await save();
  await expect.poll(() => savedInWorktree(page)).toBe(true);
  await page.evaluate(() => window.appBackend!.keepStore());
  await page.reload();
  await booted(page);
}

/**
 * Ends the page with no quit, as a crash does, once the normal-use save
 * landed. The next load gets the store as it stood then, so the unload save
 * the reload runs cannot add to it.
 */
async function crashAndReopen(page: Page) {
  await expect.poll(() => savedInWorktree(page)).toBe(true);
  const store = await page.evaluate(() =>
    JSON.stringify(window.appBackend!.store),
  );
  await page.addInitScript((store) => {
    sessionStorage.setItem("appBackendStore", store);
  }, store);
  await page.reload();
  await booted(page);
}

for (const [entry, again] of [
  ["restart", (app: AppShell) => app.restart()],
  [
    "reload",
    (_app: AppShell, page: Page) =>
      saveAloneAndReopen(page, () =>
        // The app's own reload save runs on pagehide (App.tsx reap).
        page.evaluate(() => window.dispatchEvent(new Event("pagehide"))),
      ),
  ],
  [
    "close-requested",
    (app: AppShell, page: Page) =>
      saveAloneAndReopen(page, () => app.emit("tauri://close-requested")),
  ],
  ["crash", (_app: AppShell, page: Page) => crashAndReopen(page)],
] as const) {
  test(`a worktree tab comes back in its worktree after a ${entry}`, async ({
    app,
    page,
  }) => {
    await mount(app);
    const defaultStrip = await strip(page);
    await openInWorktree(page);

    await again(app, page);

    // The app opens on the default workspace, as before.
    await expect(switcher(page)).toHaveText("Workspace");
    await expect.poll(() => strip(page)).toEqual(defaultStrip);
    await switcher(page).click();
    await expect(worktreeRow(page).getByLabel("1 open tab")).toBeVisible();
    await page.screenshot({ path: test.info().outputPath(`F1-${entry}.png`) });

    await worktreeRow(page).click();
    await expect(switcher(page)).toHaveText(TREE);
    await expect
      .poll(() => strip(page))
      .toEqual([`monocode · ${OPENED.title}`]);
    await expect(tab(page, OPENED.title)).toHaveClass(/bg-selection/);
    await page.screenshot({ path: test.info().outputPath(`F2-${entry}.png`) });
  });
}

/** Tabs 01 and 02 of the fixture, both in worktrees, as workspaceSnapshot
 * saves them (worktreeTabsRestart.realdb.test.ts proves the save and boot). */
const [first, second] = realDb.snapshot.tabs;
const titleOf = (tab: typeof first) => realDb.records[tab.focusedId].title;
const rowOf = (page: Page, tab: typeof first) =>
  page.getByRole("listbox", { name: "Working copies" }).getByRole("option", {
    name: new RegExp(realDb.records[tab.focusedId].branch!),
  });
const allInWorktrees = {
  ...realDb.snapshot,
  tabs: [first, second],
  activeTabId: "",
  tabWorkspaces: [first, second].map((tab) => ({
    tabId: tab.id,
    path: realDb.records[tab.focusedId].worktreeCwd!,
  })),
};

/** The default workspace is shown with no tab, and each worktree keeps one. */
async function expectDefaultEmpty(page: Page) {
  await expect(switcher(page)).toHaveText("Workspace");
  await expect.poll(() => strip(page)).toEqual([]);
  await switcher(page).click();
  for (const tab of [first, second])
    await expect(rowOf(page, tab).getByLabel("1 open tab")).toBeVisible();
}

test("with every tab in a worktree, boot opens the default workspace with no tab and each worktree keeps its tab", async ({
  app,
  page,
}) => {
  await mount(app, allInWorktrees);
  await expectDefaultEmpty(page);
  await page.screenshot({
    path: test.info().outputPath("F1-all-in-worktrees.png"),
  });

  await rowOf(page, second).click();
  await expect
    .poll(() => strip(page))
    .toEqual([`monocode · ${titleOf(second)}`]);
  await expect(tab(page, titleOf(second))).toHaveClass(/bg-selection/);
  await page.screenshot({
    path: test.info().outputPath("F2-all-in-worktrees.png"),
  });
});

test("quitting on a worktree tab with no default-workspace tab restarts on the default workspace with no tab", async ({
  app,
  page,
}) => {
  await mount(app, allInWorktrees);
  await switcher(page).click();
  await rowOf(page, second).click();
  await expect(tab(page, titleOf(second))).toHaveClass(/bg-selection/);

  await app.restart();

  await expectDefaultEmpty(page);
  await page.screenshot({
    path: test.info().outputPath("F1-all-in-worktrees-restart.png"),
  });
});

test("a chat running in a worktree at quit comes back in that worktree, still working", async ({
  app,
  page,
}) => {
  await mount(app);
  const defaultStrip = await strip(page);
  await openInWorktree(page);
  const composer = page
    .getByPlaceholder("Ask, build, / for commands")
    .locator("visible=true");
  await composer.fill("Keep going on the approvals table");
  await composer.press("Enter");
  await expect(
    page.getByRole("main").getByRole("button", { name: "Stop" }),
  ).toBeVisible();

  await app.restart();

  await expect.poll(() => strip(page)).toEqual(defaultStrip);
  await switcher(page).click();
  await expect(
    worktreeRow(page).getByLabel("1 open tab, working"),
  ).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("F3-switcher.png") });
  await worktreeRow(page).click();
  await expect.poll(() => strip(page)).toEqual([`monocode · ${OPENED.title}`]);
  await expect(
    page.getByText("Turn interrupted when MonoCode quit."),
  ).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("F3-worktree.png") });
});
