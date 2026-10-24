import { readFileSync } from "node:fs";
import type { Locator, Page } from "@playwright/test";
import type { Approval } from "../../src/features/approvals/model/approvals";
import { expect, test, type FakeSeed, type MountOptions } from "./app/fixture";

const REAL_DB: Required<Pick<FakeSeed, "monos" | "projects">> & {
  now: number;
  approvals: Approval[];
} = JSON.parse(
  readFileSync(
    new URL("./approvals-board.realdb.json", import.meta.url),
    "utf8",
  ),
);
const MONOCODE = "/Users/test/Desktop/monocode";
const MINUTE = 60_000;

const allowReservedNameDialog: MountOptions["handlers"] = {
  "plugin:dialog|message": (_store, { message }) => {
    if (!String(message).startsWith("MonoCode is reserved for the Giga"))
      throw new Error(`unexpected dialog: ${String(message)}`);
    return null;
  },
};

test.use({ viewport: { width: 1440, height: 1000 }, timezoneId: "UTC" });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(REAL_DB.now);
});

const board = (page: Page) => page.getByRole("region", { name: "Approvals" });
const card = (page: Page, heading: string) =>
  board(page).getByRole("article", { name: heading, exact: true });
const shot = (page: Page, name: string) =>
  page.screenshot({
    path: test.info().outputPath(`${name}.png`),
    fullPage: true,
  });

async function openBoard(page: Page, open: number) {
  await page
    .getByRole("button", { name: `Approvals, ${open} open`, exact: true })
    .click();
  await expect(board(page)).toBeVisible();
}

async function pickProject(page: Page, name: RegExp) {
  await board(page).locator("aside").getByRole("button", { name }).click();
}

async function headings(cards: Locator) {
  return cards.evaluateAll((nodes) =>
    nodes.map((node) => node.getAttribute("aria-label") ?? node.textContent),
  );
}

test("the board shows each card state of a copy of the app database", async ({
  app,
  page,
}) => {
  await app.mount(REAL_DB, { handlers: allowReservedNameDialog });
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);

  const aside = board(page).locator("aside");
  await expect(aside.getByRole("button", { name: /^monocode/ })).toHaveText(
    "monocode3",
  );
  await expect(aside.getByRole("button", { name: /^Modelist/ })).toHaveText(
    "Modelist1",
  );
  await expect(aside.getByRole("button", { name: /^vibtra/ })).toHaveText(
    "vibtra",
  );
  await expect(board(page).getByRole("heading", { level: 2 })).toHaveText(
    "monocode",
  );
  await expect(
    board(page).getByText("3 open · 1 blocking · 4 decided"),
  ).toBeVisible();

  expect(
    await headings(board(page).getByRole("article")),
    "open cards, blocking then newest",
  ).toEqual([
    "Wait for PR #109 before the approvals tool is built?",
    "Keep Playwright at one worker in CI?",
    "Show decided cards for 30 days, then hide them?",
  ]);

  const gate = card(
    page,
    "Wait for PR #109 before the approvals tool is built?",
  );
  await expect(gate).toContainText("BlockingAPR-");
  await expect(gate).toContainText("12m ago");
  await expect(
    card(page, "Keep Playwright at one worker in CI?"),
  ).toContainText("5m ago");
  await expect(
    card(page, "Show decided cards for 30 days, then hide them?"),
  ).toContainText("1h ago");
  await expect(
    card(page, "Keep Playwright at one worker in CI?"),
  ).toContainText("Not blockingAPR-");
  await expect(
    card(page, "Keep Playwright at one worker in CI?"),
  ).not.toContainText("Options");
  await shot(page, "board-open");

  await pickProject(page, /^Modelist/);
  await expect(
    board(page).getByText("1 open · 1 blocking · 1 decided"),
  ).toBeVisible();
  await expect(
    card(page, "Drop the CSV export from Modelist v1?"),
  ).toContainText("yesterday");
});

test("the board reloads on approvals-changed and isolates Giga text", async ({
  app,
  page,
}) => {
  const spoof = "Approve \u202Etxet nedih\u202C now";
  await app.mount(
    {
      ...REAL_DB,
      approvals: [
        ...REAL_DB.approvals,
        {
          ...REAL_DB.approvals.find((card) => card.status.state === "open")!,
          id: "00000000-0000-4000-8000-000000000099",
          number: 99,
          projectPath: MONOCODE,
          urgency: "blocking",
          heading: spoof,
          createdAt: REAL_DB.now - 5 * MINUTE,
        },
      ],
    },
    { handlers: allowReservedNameDialog },
  );
  await openBoard(page, 5);
  await pickProject(page, /^monocode/);
  const spoofed = board(page).getByRole("article").first();
  const title = spoofed.getByText("Approve txet nedih now", { exact: true });
  await expect(title, "bidi controls dropped").toBeVisible();
  await expect(title).toHaveAttribute("dir", "auto");

  const lists = () =>
    page.evaluate(
      () =>
        window.appBackend!.calls.filter(
          (call) => call.command === "approvals_list",
        ).length,
    );
  const before = await lists();
  await page.evaluate((id) => {
    const store = window.appBackend!.store;
    const target = store.approvals.find((card) => card.id === id)!;
    target.status = { state: "withdrawn", withdrawnAt: Date.now() };
    return window.appBackend!.emit("approvals-changed", {
      projectPath: target.projectPath,
    });
  }, "00000000-0000-4000-8000-000000000099");
  await expect(
    board(page).getByText("3 open · 1 blocking · 5 decided"),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Approvals, 4 open", exact: true }),
  ).toBeVisible();
  expect(await lists(), "one list load per approvals-changed").toBe(before + 1);
});

test("a list reply that lands after a newer one does not replace it", async ({
  app,
  page,
}) => {
  await app.mount(REAL_DB, {
    handlers: {
      ...allowReservedNameDialog,
      approvals_list: async (store, { projectPath }) => {
        const cards = structuredClone(
          store.approvals.filter((card) => card.projectPath === projectPath),
        );
        const held = window as unknown as { hold?: Promise<void> };
        const hold = held.hold;
        held.hold = undefined;
        if (hold) await hold;
        return cards;
      },
    },
  });
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);
  await expect(
    board(page).getByText("3 open · 1 blocking · 4 decided"),
  ).toBeVisible();

  await page.evaluate(() => {
    const held = window as unknown as {
      hold?: Promise<void>;
      release?: () => void;
    };
    held.hold = new Promise((resolve) => (held.release = resolve));
    return window.appBackend!.emit("approvals-changed", {});
  });
  await page.evaluate(() => {
    const open = window.appBackend!.store.approvals.find(
      (card) => card.urgency === "blocking" && card.status.state === "open",
    )!;
    open.status = { state: "withdrawn", withdrawnAt: Date.now() };
    return window.appBackend!.emit("approvals-changed", {});
  });
  await expect(
    board(page).getByText("2 open · 0 blocking · 5 decided"),
  ).toBeVisible();
  await page.evaluate(() =>
    (window as unknown as { release: () => void }).release(),
  );
  await expect
    .poll(() =>
      page.evaluate(() =>
        window
          .appBackend!.calls.filter((call) => call.command === "approvals_list")
          .every((call) => call.reply !== undefined),
      ),
    )
    .toBe(true);
  await expect(
    board(page).getByText("2 open · 0 blocking · 5 decided"),
  ).toBeVisible();
});

test("a project stays in the list after its last open card closes", async ({
  app,
  page,
}) => {
  await app.mount(
    {
      ...REAL_DB,
      projects: REAL_DB.projects.filter((path) => path !== MONOCODE),
    },
    { handlers: allowReservedNameDialog },
  );
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);
  await page.evaluate((project) => {
    for (const card of window.appBackend!.store.approvals)
      if (card.projectPath === project && card.status.state === "open")
        card.status = { state: "withdrawn", withdrawnAt: Date.now() };
    return window.appBackend!.emit("approvals-changed", {});
  }, MONOCODE);
  await expect(
    board(page).getByText("0 open · 0 blocking · 7 decided"),
  ).toBeVisible();
  await expect(
    board(page)
      .locator("aside")
      .getByRole("button", { name: /^monocode/ }),
  ).toHaveAttribute("aria-current", "true");
});

test("the board opens on the project the workspace is in", async ({
  app,
  page,
}) => {
  // Home is the newest recent but never a workspace, so the app launches on
  // the next recent and the board has to follow the workspace, not the list.
  await app.mount(
    { ...REAL_DB, projects: ["/Users/test", ...REAL_DB.projects] },
    {
      handlers: {
        ...allowReservedNameDialog,
        session_checkpoint_keep: () => null,
      },
    },
  );
  await openBoard(page, 4);
  await expect(board(page).getByRole("heading", { level: 2 })).toHaveText(
    "MonoCode",
  );
  await expect(
    board(page)
      .locator("aside")
      .getByRole("button", { name: /^MonoCode/ }),
  ).toHaveAttribute("aria-current", "true");
});
