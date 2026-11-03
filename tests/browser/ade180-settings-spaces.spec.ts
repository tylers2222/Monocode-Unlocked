import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";

// ADE-180: Settings > Spaces, built to ADE-170 frames 3 and 5. Data is made up.

test.use({ viewport: { width: 1440, height: 900 } });

const HANDLERS = {
  spaces_list: () => [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
  ],
  spaces_settings_get: () => ({
    activeSpaceId: "space-1",
    routeGrantTtlMs: 21_600_000,
  }),
  spaces_accounts_list: () => [
    { provider: "claude", id: "account-studio", label: "Studio" },
    { provider: "codex", id: "account-dev", label: "Dev" },
  ],
  spaces_logins_list: () => [
    { spaceId: "space-1", provider: "claude", accountId: "default" },
    { spaceId: "space-1", provider: "codex", accountId: "account-dev" },
    { spaceId: "space-2", provider: "claude", accountId: "account-studio" },
  ],
  provider_account_identity: (
    _store: unknown,
    args: { provider: string; accountId: string },
  ) =>
    ({
      "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
      "codex:account-dev": {
        email: "tyler.dev@example.com",
        plan: "ChatGPT Pro",
      },
      "claude:account-studio": {
        email: "studio.ts@example.com",
        plan: "Max 5x",
      },
    })[`${args.provider}:${args.accountId}`] ?? null,
};

const settings = (page: Page) => page.getByRole("region", { name: "Settings" });
const spaceRow = (page: Page, id: string) =>
  settings(page).locator(`[data-space-id="${id}"]`);
const duration = (page: Page) =>
  settings(page).getByRole("button", { name: /^Use the other login for: / });
const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

async function openSpaces(page: Page) {
  await page.getByRole("button", { name: /^Settings/ }).click();
  await page
    .getByRole("navigation", { name: "Projects" })
    .getByRole("button", { name: "Spaces", exact: true })
    .click();
  await expect(settings(page).locator("h1")).toHaveText("Spaces");
}

test("Settings > Spaces lists both logins per Space and sets the card's default time", async ({
  app,
  page,
}) => {
  await app.mount({}, { handlers: HANDLERS });
  await openSpaces(page);

  // Frame 3: one row per Space, this window marked, both logins on each.
  await expect(spaceRow(page, "space-1")).toContainText("Space 1This window");
  await expect(spaceRow(page, "space-1")).toContainText(
    "tyler.main@example.com",
  );
  await expect(spaceRow(page, "space-1")).toContainText(
    "tyler.dev@example.com",
  );
  await expect(spaceRow(page, "space-2")).toContainText(
    "studio.ts@example.com",
  );
  await expect(spaceRow(page, "space-2")).toContainText("No Codex login");
  await expect(spaceRow(page, "space-2")).not.toContainText("This window");
  await expect(
    spaceRow(page, "space-1").getByRole("button", { name: "Remove Space 1" }),
  ).toHaveCount(0);
  await expect(
    spaceRow(page, "space-2").getByRole("button", { name: "Remove Space 2" }),
  ).toBeVisible();
  await expect(duration(page)).toHaveAccessibleName(
    "Use the other login for: 6 hours",
  );
  await shot(page, "frame-3");

  // Frame 5: the same Select as the rest of Settings, open, 6 hours default.
  await duration(page).click();
  const listbox = page.getByRole("listbox", {
    name: "Use the other login for",
  });
  await expect(listbox.getByRole("option")).toHaveText([
    "1 hour",
    "3 hours",
    "6 hoursdefault",
    "12 hours",
    "24 hours",
    "Until the limit resets",
  ]);
  await shot(page, "frame-5");

  await listbox.getByRole("option", { name: "3 hours" }).click();
  await expect(duration(page)).toHaveAccessibleName(
    "Use the other login for: 3 hours",
  );

  // The choice outlives the page: leave Settings and come back.
  await page
    .getByRole("navigation", { name: "Projects" })
    .getByRole("button", { name: "Back", exact: true })
    .click();
  await openSpaces(page);
  await expect(duration(page)).toHaveAccessibleName(
    "Use the other login for: 3 hours",
  );
});
