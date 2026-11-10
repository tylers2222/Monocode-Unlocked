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
