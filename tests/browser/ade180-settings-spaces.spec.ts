import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";

// ADE-180: Settings > Spaces, built to ADE-170 frames 3, 5 and 11-14. Data is
// made up. The Spaces commands keep their rows on `window.spacesFake`, so a
// write shows on the next read, as monocode.db would.

test.use({ viewport: { width: 1440, height: 900 } });

const VIOLET = "hsl(280 55% 62%)";
const GREEN = "hsl(142 55% 50%)";

const HANDLERS = {
  spaces_list: () =>
    (
      (window as unknown as Record<string, unknown>).spacesFake as {
        spaces: unknown[];
      }
    ).spaces,
  spaces_logins_list: () =>
    (
      (window as unknown as Record<string, unknown>).spacesFake as {
        logins: unknown[];
      }
    ).logins,
  spaces_settings_get: () => ({
    activeSpaceId: "space-1",
    routeGrantTtlMs: 21_600_000,
  }),
  spaces_create: (_store: unknown, args: { name: string; color: string }) => {
    const fake = (window as unknown as Record<string, unknown>).spacesFake as {
      spaces: unknown[];
    };
    const space = {
      id: `space-${fake.spaces.length + 1}`,
      name: args.name,
      color: args.color,
      createdAt: fake.spaces.length + 1,
    };
    fake.spaces.push(space);
    return space;
  },
  spaces_update: (
    _store: unknown,
    args: { id: string; name: string; color: string },
  ) => {
    const fake = (window as unknown as Record<string, unknown>).spacesFake as {
      spaces: { id: string; name: string; color: string }[];
    };
    const space = fake.spaces.find((entry) => entry.id === args.id)!;
    space.name = args.name;
    space.color = args.color;
    return null;
  },
  spaces_login_set: (
    _store: unknown,
    args: { login: { spaceId: string; provider: string; accountId: string } },
  ) => {
    const fake = (window as unknown as Record<string, unknown>).spacesFake as {
      logins: { spaceId: string; provider: string }[];
    };
    fake.logins = [
      ...fake.logins.filter(
        (login) =>
          login.spaceId !== args.login.spaceId ||
          login.provider !== args.login.provider,
      ),
      args.login,
    ];
    return null;
  },
  spaces_accounts_list: () => [
    { provider: "claude", id: "account-second", label: "Claude account 2" },
    { provider: "claude", id: "account-studio", label: "Studio" },
    { provider: "codex", id: "account-dev", label: "Dev" },
  ],
  provider_account_identity: (
    _store: unknown,
    args: { provider: string; accountId: string },
  ) =>
    ({
      "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
      "claude:account-second": {
        email: "second.ts@example.com",
        plan: "Max 5x",
      },
      "claude:account-studio": {
        email: "studio.ts@example.com",
        plan: "Max 5x",
      },
      "codex:account-dev": {
        email: "tyler.dev@example.com",
        plan: "ChatGPT Pro",
      },
    })[`${args.provider}:${args.accountId}`] ?? null,
};

// The rows the Spaces commands start from (page global, see above).
const SPACES_FAKE = {
  spaces: [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: VIOLET, createdAt: 2 },
  ],
  logins: [
    { spaceId: "space-1", provider: "claude", accountId: "default" },
    { spaceId: "space-1", provider: "codex", accountId: "account-dev" },
    { spaceId: "space-2", provider: "claude", accountId: "account-studio" },
  ],
};

// The login list as the browser keeps it until ADE-171 moves it to the
// database (spaces_accounts_list above answers once it has).
const STORED_ACCOUNTS = JSON.stringify({
  claude: [
    { id: "account-second", provider: "claude", label: "Claude account 2" },
    { id: "account-studio", provider: "claude", label: "Studio" },
  ],
  codex: [{ id: "account-dev", provider: "codex", label: "Dev" }],
});

const settings = (page: Page) => page.getByRole("region", { name: "Settings" });
const spaceRow = (page: Page, id: string) =>
  settings(page).locator(`[data-space-id="${id}"]`);
const tile = (page: Page, id: string) =>
  spaceRow(page, id).locator("[data-space-color]");
const duration = (page: Page) =>
  settings(page).getByRole("button", { name: /^Use the other login for: / });
const dialog = (page: Page) => page.getByRole("dialog");
const loginPicker = (page: Page, provider: "claude" | "codex") =>
  dialog(page)
    .locator(`[data-space-login-row="${provider}"]`)
    .getByRole("button", { name: /login: / });
const writes = (page: Page) =>
  page.evaluate(() =>
    window
      .appBackend!.calls.filter(({ command }) =>
        ["spaces_create", "spaces_update", "spaces_login_set"].includes(
          command,
        ),
      )
      .map(({ command, args }) => ({ command, args })),
  );
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

test.beforeEach(async ({ app, page }) => {
  await page.addInitScript(
    ({ accounts, fake }) => {
      localStorage.setItem("monocode.providerAccounts.v1", accounts);
      // The frames draw the emails; masking is the default.
      localStorage.setItem("monocode.maskEmails", "0");
      (window as unknown as Record<string, unknown>).spacesFake = fake;
    },
    { accounts: STORED_ACCOUNTS, fake: SPACES_FAKE },
  );
  await app.mount({}, { handlers: HANDLERS });
  await openSpaces(page);
});

test("frames 3, 5, 13, 14: each Space with its colour and both logins, and the card's default time", async ({
  page,
}) => {
  // One row per Space, this window marked, both logins on each.
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
  // Frame 13: the tile in the Space's colour; Space 1 has none, so blue.
  await expect(tile(page, "space-1")).toHaveAttribute(
    "data-space-color",
    "hsl(211 92% 62%)",
  );
  await expect(tile(page, "space-2")).toHaveAttribute(
    "data-space-color",
    VIOLET,
  );
  // Frame 14 A+A: only the pencil, and no counts.
  await expect(spaceRow(page, "space-2").getByRole("button")).toHaveText([""]);
  await expect(
    spaceRow(page, "space-2").getByRole("button", { name: "Edit Space 2" }),
  ).toBeVisible();
  await expect(settings(page)).not.toContainText("Monos ·");
  await expect(duration(page)).toHaveAccessibleName(
    "Use the other login for: 6 hours",
  );
  await shot(page, "frame-13-14");

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

test("frames 11, 12: New Space, then Edit Space picks its logins", async ({
  page,
}) => {
  // Frame 11: name and colour, then Create.
  await settings(page).getByRole("button", { name: "New Space" }).click();
  await expect(dialog(page)).toContainText(
    "A Space has its own logins, Gigas, Monos, projects and sessions.",
  );
  await dialog(page).getByLabel("Space name").fill("Client work");
  await dialog(page).getByRole("button", { name: "Green" }).click();
  await shot(page, "frame-11");
  await dialog(page).getByRole("button", { name: "Create Space" }).click();

  // Create opens Edit Space for the new Space.
  await expect(dialog(page)).toContainText("Edit Space");
  await expect(dialog(page)).toContainText(
    "One per provider. New sessions in Client work sign in with these.",
  );
  await expect(loginPicker(page, "codex")).toHaveAccessibleName(
    "Codex login: No Codex login",
  );
  await loginPicker(page, "claude").click();
  await page.getByRole("option", { name: /^Studio/ }).click();
  await dialog(page).getByRole("button", { name: "Save" }).click();
  await expect(dialog(page)).toHaveCount(0);

  expect(await writes(page)).toEqual([
    { command: "spaces_create", args: { name: "Client work", color: GREEN } },
    {
      command: "spaces_update",
      args: { id: "space-3", name: "Client work", color: GREEN },
    },
    {
      command: "spaces_login_set",
      args: {
        login: {
          spaceId: "space-3",
          provider: "claude",
          accountId: "account-studio",
        },
      },
    },
  ]);
  await expect(spaceRow(page, "space-3")).toContainText(
    "studio.ts@example.com",
  );
  await expect(tile(page, "space-3")).toHaveAttribute(
    "data-space-color",
    GREEN,
  );

  // Frame 12: Space 2's pencil, the Claude picker open.
  await spaceRow(page, "space-2")
    .getByRole("button", { name: "Edit Space 2" })
    .click();
  await expect(loginPicker(page, "claude")).toHaveText(
    "Studio · studio.ts@example.com",
  );
  await loginPicker(page, "claude").click();
  await expect(page.getByRole("option")).toHaveText([
    "Default accounttyler.main@example.com · Max 20x",
    "Claude account 2second.ts@example.com · Max 5x",
    "Studiostudio.ts@example.com · Max 5x",
  ]);
  await shot(page, "frame-12-pick");
  await page.getByRole("option", { name: /^Studio/ }).click();

  // Frame 12: the Codex row's Add a login.
  const codex = dialog(page).locator('[data-space-login-row="codex"]');
  await codex.getByRole("button", { name: "Add a login" }).click();
  await codex.getByLabel("New Codex account").fill("Studio Codex");
  await expect(codex).toContainText(
    "Same sign-in as Settings › Providers › Add account.",
  );
  await expect(codex.getByRole("button", { name: "Add a login" })).toHaveCount(
    0,
  );
  await shot(page, "frame-12-add");
});
