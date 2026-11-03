import { expect, test } from "./app/fixture";

/**
 * ADE-179: the Space switcher above the Giga Monos (ADE-170 frames 1 and 2)
 * on the whole app. Two Spaces, each with its own logins, Monos and project;
 * switching changes only what the rail shows. Emails are made up.
 */
const ONE = "/Users/test/alpha-app";
/** A Space 1 project that is not the one open. */
const OTHER = "/Users/test/beta-lib";
const TWO = "/Users/test/studio-site";

const SEED = {
  projects: [ONE, OTHER, TWO],
  projectSpaces: { [TWO]: "space-2" },
  spaces: [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
  ],
  spaceLogins: [
    { spaceId: "space-1", provider: "codex", accountId: "dev" },
    { spaceId: "space-2", provider: "claude", accountId: "studio" },
  ],
  identities: {
    "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
    "codex:dev": { email: "tyler.dev@example.com", plan: "ChatGPT Pro" },
    "claude:studio": { email: "studio.ts@example.com", plan: "Max 5x" },
  },
  monos: [
    {
      id: "giga-1",
      role: "giga" as const,
      data: { name: "General", mascot: "ghost" },
    },
    {
      id: "mono-1",
      role: "mono" as const,
      data: { name: "Fork Orchestrator", mascot: "crab" },
    },
    {
      id: "mono-2",
      role: "mono" as const,
      data: { name: "Studio Builder", mascot: "skull" },
      spaceId: "space-2",
    },
  ],
};

test("the switcher shows the active Space's logins and switches the rail to the other Space", async ({
  app,
  page,
}, testInfo) => {
  await page.addInitScript(() =>
    localStorage.setItem("monocode.maskEmails", "0"),
  );
  await app.mount(SEED);
  const rail = page.locator('nav[aria-label="Projects"]');
  const monos = rail.locator("[data-mono-rail]");
  const trigger = rail.getByRole("button", { name: "Space: Space 1" });

  // Frame 1: Space 1 and both its logins, above the Giga Monos.
  await expect(trigger).toBeVisible();
  await expect(trigger).toContainText("tyler.main@example.com");
  await expect(trigger).toContainText("tyler.dev@example.com");
  await expect(monos).toContainText("General");
  await expect(monos).toContainText("Fork Orchestrator");
  await expect(monos).not.toContainText("Studio Builder");
  await expect(rail).toContainText("alpha-app");
  await expect(rail).toContainText("beta-lib");
  await expect(rail).not.toContainText("studio-site");
  await page.screenshot({ path: testInfo.outputPath("frame1-closed.png") });

  // Frame 2: one row per Space, each with both logins and its plan.
  await trigger.click();
  const menu = page.getByRole("menu", { name: "Spaces" });
  const space1 = menu.getByRole("menuitemradio", { name: /Space 1/ });
  const space2 = menu.getByRole("menuitemradio", { name: /Space 2/ });
  await expect(space1).toHaveAttribute("aria-checked", "true");
  await expect(space1).toContainText("tyler.main@example.com · Max 20x");
  await expect(space1).toContainText("tyler.dev@example.com · ChatGPT Pro");
  await expect(space2).toContainText("studio.ts@example.com · Max 5x");
  await expect(space2).toContainText("No Codex login");
  await expect(menu.getByRole("menuitem", { name: "New Space" })).toBeVisible();
  await expect(
    menu.getByRole("menuitem", { name: "Manage Spaces" }),
  ).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("frame2-open.png") });

  // Picking Space 2 saves only which Space shows, and the rail follows.
  const before = await page.evaluate(() => window.appBackend!.calls.length);
  await space2.click();
  await expect(menu).toHaveCount(0);
  await expect(
    rail.getByRole("button", { name: "Space: Space 2" }),
  ).toBeVisible();
  await expect(monos).toContainText("Studio Builder");
  await expect(monos).not.toContainText("Fork Orchestrator");
  await expect(rail).toContainText("studio-site");
  // Space 1's projects leave the rail; the project open in the window keeps
  // its row until the tabs follow the Space (frame 10, not in ADE-179).
  await expect(rail).not.toContainText("beta-lib");
  await expect(rail.getByRole("button", { name: "alpha-app" })).toBeVisible();
  const sent = await page.evaluate(
    (from) => window.appBackend!.calls.slice(from).map((call) => call.command),
    before,
  );
  expect(sent.filter((command) => command === "spaces_settings_set")).toEqual([
    "spaces_settings_set",
  ]);
  expect(
    sent.filter((command) => /kill|stop|cancel|interrupt/.test(command)),
    "a switch stops nothing",
  ).toEqual([]);
  expect(await page.evaluate(() => window.appBackend!.store.activeSpaceId)).toBe(
    "space-2",
  );
  await page.screenshot({ path: testInfo.outputPath("switched-space2.png") });
});
