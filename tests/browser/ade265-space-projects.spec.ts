import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";

/**
 * ADE-235 frames 16-18, option A, on the whole app: a Space 2 Mono's project
 * shows in Space 2's rail and picker with no mark, and Space 1 does not change.
 * Project and Mono names are the live example; emails are not used here.
 */
const SITE = "/Users/test/example-site";
const VIBTRA = "/Users/test/Desktop/vibtra";
const ONLY_ONE = "/Users/test/example-app";

const SEED = {
  projects: [SITE, VIBTRA, ONLY_ONE],
  projectSpaces: { [SITE]: "space-2" },
  activeSpaceId: "space-2",
  spaces: [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
  ],
  monos: [
    {
      id: "mono-v",
      role: "mono" as const,
      data: { name: "Vibtra Orchestrator", mascot: "crab", projects: [VIBTRA] },
      spaceId: "space-2",
    },
  ],
};

const rail = (page: Page) => page.locator('nav[aria-label="Projects"]');

const called = (page: Page, command: string) =>
  page.evaluate(
    (name) => window.appBackend!.calls.filter((call) => call.command === name),
    command,
  );

test("R1, P1: Vibtra's project shows in Space 2's rail, like any other project", async ({
  app,
  page,
}) => {
  await app.mount(SEED);
  await expect(rail(page)).toContainText("vibtra");
  await expect(rail(page)).toContainText("example-site");
  await expect(rail(page)).not.toContainText("example-app");
});

test("P2: a Mono's project outside recents still shows", async ({ app, page }) => {
  await app.mount({ ...SEED, projects: [SITE] });
  await expect(rail(page)).toContainText("vibtra");
});

test("Space 1 keeps its rail: a recent with no row shows and an assigned one does not", async ({
  app,
  page,
}) => {
  // Open a Space 1 project: the open project keeps its row whatever its Space.
  await app.mount({
    ...SEED,
    projects: [ONLY_ONE, VIBTRA, SITE],
    activeSpaceId: "space-1",
  });
  await expect(rail(page)).toContainText("example-app");
  await expect(rail(page)).toContainText("vibtra");
  await expect(rail(page)).not.toContainText("example-site");
});

test("T13: switching Space refetches the list and replaces the rail", async ({
  app,
  page,
}) => {
  const onlyTwo = "/Users/test/example-two";
  await app.mount({
    ...SEED,
    projects: [...SEED.projects, onlyTwo],
    projectSpaces: { ...SEED.projectSpaces, [onlyTwo]: "space-2" },
  });
  await expect(rail(page)).toContainText("example-two");
  await rail(page).getByRole("button", { name: "Space: Space 2" }).click();
  await page.getByRole("menuitemradio", { name: /Space 1/ }).click();
  await expect(rail(page)).toContainText("example-app");
  await expect(rail(page)).not.toContainText("example-two");
  // The open project keeps its row while open, whatever its Space.
  await expect(rail(page)).toContainText("example-site");
});

test("H1: stepping to a project on the rail writes no membership", async ({
  app,
  page,
}) => {
  await app.mount(SEED, { handlers: { session_checkpoint_keep: () => null } });
  await rail(page).getByRole("button", { name: "vibtra", exact: true }).click();
  await expect(rail(page)).toContainText("vibtra");
  expect(await called(page, "spaces_project_add")).toEqual([]);
});

test("T14: a failed list read shows every project", async ({ app, page }) => {
  await app.mount(SEED, {
    handlers: {
      spaces_project_list: () => {
        throw "database is locked";
      },
    },
  });
  await expect(rail(page)).toContainText("example-app");
  await expect(rail(page)).toContainText("example-site");
  await expect(rail(page)).toContainText("vibtra");
});

const SPACE_ONE = {
  ...SEED,
  projects: [ONLY_ONE, VIBTRA, SITE],
  activeSpaceId: "space-1",
};

const picker = (page: Page) =>
  page.getByRole("dialog", { name: "Project picker" });

/** Frame 16: the picker lists the rail's projects, with no mark (option A). */
const PICKERS = [
  {
    space: "Space 2",
    seed: SEED,
    open: "example-site",
    shows: ["example-site", "vibtra"],
    hides: "example-app",
  },
  {
    space: "Space 1",
    seed: SPACE_ONE,
    open: "example-app",
    shows: ["example-app", "vibtra"],
    hides: "example-site",
  },
];

async function expectPickerList(
  page: Page,
  { shows, hides, space }: (typeof PICKERS)[number],
) {
  await expect(picker(page)).toBeVisible();
  for (const name of shows) await expect(picker(page)).toContainText(name);
  await expect(picker(page)).not.toContainText(hides);
  await expect(picker(page)).not.toContainText("Also in");
  await page.screenshot({
    path: test.info().outputPath(`picker-${space.replace(" ", "-")}.png`),
  });
}

for (const frame of PICKERS) {
  test(`frame 16: the sidebar picker (hidden rail) lists ${frame.space}'s projects`, async ({
    app,
    page,
  }) => {
    await page.addInitScript(() =>
      localStorage.setItem("monocode.collapsedProjectRailMode", "hidden"),
    );
    await app.mount(frame.seed);
    await page.getByRole("button", { name: "Toggle Projects" }).click();
    await page
      .getByRole("complementary")
      .getByRole("button", { name: new RegExp(frame.open) })
      .first()
      .click();
    await expectPickerList(page, frame);
  });

  test(`frame 16: the compact rail's picker lists ${frame.space}'s projects`, async ({
    app,
    page,
  }) => {
    await app.mount(frame.seed);
    await page.getByRole("button", { name: "Toggle Projects" }).click();
    await page
      .getByRole("button", {
        name: `Switch project, current project ${frame.open}`,
      })
      .click();
    await expectPickerList(page, frame);
  });
}

// T1 and T2: each way a user adds a project on purpose lists it in the Space.
const addedArgs = async (page: Page) =>
  (await called(page, "spaces_project_add")).map(
    (call) => call.args as { projectPath: string; knownToWindow: boolean },
  );

test("T1: Open folder lists the picked folder in the Space", async ({
  app,
  page,
}) => {
  await app.mount(SEED, {
    handlers: {
      "plugin:dialog|open": () => "/Users/test/acme",
      session_checkpoint_keep: () => null,
    },
  });
  await rail(page).getByRole("button", { name: "Open project" }).click();
  await page.getByRole("menuitem", { name: "Open folder…" }).click();
  await expect(rail(page)).toContainText("acme");
  await expect.poll(() => addedArgs(page)).toEqual([
    { projectPath: "/Users/test/acme", knownToWindow: false },
  ]);
});

test("T2: Restore in Settings > Archive lists the project in the Space", async ({
  app,
  page,
}) => {
  const old = "/Users/test/old-app";
  await page.addInitScript(
    (path) =>
      localStorage.setItem(
        "monocode.archivedProjects",
        JSON.stringify([{ path, archivedAt: 1 }]),
      ),
    old,
  );
  await app.mount(SEED, { handlers: { session_checkpoint_keep: () => null } });
  await rail(page).getByRole("button", { name: /^Settings/ }).click();
  await rail(page).getByRole("button", { name: "Archive", exact: true }).click();
  const settings = page.getByRole("region", { name: "Settings" });
  await settings.getByRole("button", { name: "Restore" }).click();
  // The archive entry clears only once the Space lists the project.
  await expect(settings.getByRole("button", { name: "Restore" })).toHaveCount(0);
  await expect.poll(() => addedArgs(page)).toEqual([
    { projectPath: old, knownToWindow: true },
  ]);
});

test("T1: opening a folder on a machine lists it in the Space", async ({
  app,
  page,
}) => {
  await app.mount(SEED, {
    handlers: {
      session_checkpoint_keep: () => null,
      // Handlers run in the page from their source: no outer names.
      remote_machines: () => [
        {
          id: "machine-1",
          name: "Build box",
          endpoint: "ws://build.test:7000",
          environmentId: "env-1",
        },
      ],
      remote_request: (_store, { method, params }) => {
        if (method === "projects.browse") {
          return { path: "/home/me/code/app", parent: "/home/me/code", entries: [] };
        }
        if (method === "projects.open") {
          const { cwd } = params as { cwd: string };
          return { id: "remote-app", cwd, name: "app" };
        }
        throw "the machine is offline";
      },
    },
  });
  await rail(page).getByRole("button", { name: "Open project" }).click();
  await page.getByRole("menuitem", { name: "Open folder on a machine…" }).click();
  const dialog = page.getByRole("dialog", { name: "Open folder on a machine" });
  await expect(dialog.getByLabel("Folder path on the machine")).toHaveValue(
    "/home/me/code/app",
  );
  await dialog.getByRole("button", { name: "Open", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect
    .poll(async () => (await addedArgs(page)).map((args) => args.projectPath))
    .toEqual(["remote://env-1/home/me/code/app"]);
});
