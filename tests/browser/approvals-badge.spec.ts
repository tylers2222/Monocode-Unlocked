import { readFileSync } from "node:fs";
import type { Locator, Page } from "@playwright/test";
import { cardOf, type CardRow } from "./app/approvals";
import { expect, test, type FakeSeed, type MountOptions } from "./app/fixture";

const REAL_DB: Required<Pick<FakeSeed, "monos" | "projects">> & {
  approvals: CardRow[];
} = JSON.parse(
  readFileSync(
    new URL("./approvals-badge.realdb.json", import.meta.url),
    "utf8",
  ),
);
const GIGA = REAL_DB.monos.find((mono) => mono.role === "giga")!;
const card = (row: CardRow) =>
  cardOf(row, { id: GIGA.id, name: String(GIGA.data.name) });
const OPEN = REAL_DB.approvals.filter((row) => row.status === "open");
const CLOSED = REAL_DB.approvals.filter((row) => row.status !== "open");
const CLOSING: CardRow["status"][] = [
  "approved",
  "rejected",
  "changes_requested",
  "withdrawn",
];

const allowReservedNameDialog: MountOptions["handlers"] = {
  "plugin:dialog|message": (_store, { message }) => {
    if (!String(message).startsWith("MonoCode is reserved for the Giga"))
      throw new Error(`unexpected dialog: ${String(message)}`);
    return null;
  },
};

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date(2026, 9, 20, 10));
});

async function setCards(
  page: Page,
  rows: CardRow[],
  trigger: "focus" | "visible" | "changed",
) {
  await page.evaluate(
    ({ cards, trigger }) => {
      window.appBackend!.store.approvals = cards;
      if (trigger === "focus") window.dispatchEvent(new Event("focus"));
      else if (trigger === "visible")
        document.dispatchEvent(new Event("visibilitychange"));
      else return window.appBackend!.emit("approvals-changed");
    },
    { cards: rows.map(card), trigger },
  );
}

const openInStore = (page: Page) =>
  page.evaluate(
    () =>
      window.appBackend!.store.approvals.filter(
        (card) => card.status.state === "open",
      ).length,
  );

const countLoads = (page: Page) =>
  page.evaluate(
    () =>
      window.appBackend!.calls.filter(
        (call) => call.command === "approvals_open_counts",
      ).length,
  );

const button = (page: Page, name: string) =>
  page.getByRole("button", { name, exact: true });

const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

async function box(locator: Locator) {
  const box = await locator.boundingBox();
  if (!box) throw new Error(`${locator} has no box`);
  return box;
}

async function firstCountAnswered(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.appBackend!.calls.some(
          (call) =>
            call.command === "approvals_open_counts" &&
            call.reply !== undefined,
        ),
      ),
    )
    .toBe(true);
}

async function expectPill(page: Page, open: number, pill: string) {
  expect(await openInStore(page), "matches open cards").toBe(open);
  const row = button(page, `Approvals, ${open} open`);
  await expect(row).toBeVisible();
  await expect(row.locator("[data-rail-badge]")).toHaveText(pill);
  return row;
}

async function expectNoPill(page: Page) {
  expect(await openInStore(page), "matches open cards").toBe(0);
  const row = button(page, "Approvals");
  await expect(row).toBeVisible();
  await expect(row.locator("[data-rail-badge]")).toHaveCount(0);
}

test("the Approvals row counts the open cards of a copy of the app database", async ({
  app,
  page,
}) => {
  await app.mount(
    { ...REAL_DB, approvals: CLOSED.map(card) },
    { handlers: allowReservedNameDialog },
  );
  await firstCountAnswered(page);
  await expectNoPill(page);
  await expect(
    button(page, "Milestones").locator("xpath=following-sibling::*[1]"),
  ).toHaveAccessibleName("Approvals");
  await shot(page, "badge-0");

  await setCards(page, [...CLOSED, OPEN[0]], "focus");
  const one = await expectPill(page, 1, "1");
  const [row, pill, text] = await Promise.all([
    box(one),
    box(one.locator("[data-rail-badge]")),
    box(one.getByText("Approvals", { exact: true })),
  ]);
  expect(row.x + row.width - (pill.x + pill.width)).toBeLessThanOrEqual(12);
  expect(pill.x).toBeGreaterThan(text.x + text.width);
  await shot(page, "badge-1");

  await setCards(page, [...CLOSED, ...OPEN.slice(0, 12)], "visible");
  await expectPill(page, 12, "12");
  await shot(page, "badge-12");

  await setCards(page, [...CLOSED, ...OPEN.slice(0, 13)], "changed");
  await expectPill(page, 13, "13");

  await setCards(page, REAL_DB.approvals, "focus");
  await expectPill(page, 130, "99+");
  await shot(page, "badge-99");

  const decided = OPEN.slice(3).map((card, index) => ({
    ...card,
    status: CLOSING[index % CLOSING.length],
  }));
  await setCards(page, [...CLOSED, ...decided, ...OPEN.slice(0, 3)], "changed");
  const three = await expectPill(page, 3, "3");

  await three.click();
  await expect(three).toHaveClass(/bg-selection/);
  for (const other of ["Milestones", "Automations"]) {
    await expect(button(page, other)).not.toHaveClass(/bg-selection/);
    await expect(page.getByRole("region", { name: other })).toHaveCount(0);
  }
  await expectPill(page, 3, "3");
  await shot(page, "on-board");

  await setCards(page, CLOSED, "focus");
  await expectNoPill(page);

  for (let toggle = 0; toggle < 6; toggle++)
    await page
      .getByRole("button", { name: /^(Toggle Projects|Expand projects)$/ })
      .first()
      .click();
  await expectNoPill(page);
  const loads = await countLoads(page);
  await setCards(page, [...CLOSED, ...OPEN.slice(0, 2)], "changed");
  await expectPill(page, 2, "2");
  expect(await countLoads(page), "one load per approvals-changed").toBe(
    loads + 1,
  );
});

type HeldReply = { hold?: Promise<void>; release?: () => void };

test("a count reply that lands after a newer one does not replace it", async ({
  app,
  page,
}) => {
  await app.mount(
    { ...REAL_DB, approvals: [...CLOSED, ...OPEN.slice(0, 3)].map(card) },
    {
      handlers: {
        ...allowReservedNameDialog,
        approvals_open_counts: async (store) => {
          const total = store.approvals.filter(
            (row) => row.status.state === "open",
          ).length;
          const held = window as unknown as HeldReply;
          const hold = held.hold;
          held.hold = undefined;
          if (hold) await hold;
          return { total, byProject: {} };
        },
      },
    },
  );
  await expect(button(page, "Approvals, 3 open")).toBeVisible();

  await page.evaluate(() => {
    const held = window as unknown as HeldReply;
    const { promise, resolve } = Promise.withResolvers<void>();
    held.hold = promise;
    held.release = resolve;
  });
  await setCards(page, [...CLOSED, ...OPEN.slice(0, 5)], "focus");
  await setCards(page, [...CLOSED, ...OPEN.slice(0, 7)], "changed");
  await expect(button(page, "Approvals, 7 open")).toBeVisible();

  await page.evaluate(() => (window as unknown as HeldReply).release!());
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.appBackend!.calls.every(
          (call) =>
            call.command !== "approvals_open_counts" ||
            call.reply !== undefined,
        ),
      ),
    )
    .toBe(true);
  await expect(
    button(page, "Approvals, 7 open"),
    "the older reply is dropped",
  ).toBeVisible();
});

for (const mode of ["compact", "hidden"] as const) {
  test(`the ${mode} rail's Approvals button has the corner dot while cards are open`, async ({
    app,
    page,
  }) => {
    await page.addInitScript((mode) => {
      localStorage.setItem("monocode.projectRailOpen", "0");
      localStorage.setItem("monocode.collapsedProjectRailMode", mode);
    }, mode);
    await app.mount(
      { ...REAL_DB, approvals: [...CLOSED, ...OPEN.slice(0, 3)].map(card) },
      { handlers: allowReservedNameDialog },
    );
    expect(await openInStore(page), "matches open cards").toBe(3);
    const three = button(page, "Approvals, 3 open");
    await expect(three).toBeVisible();
    await expect(three.locator("span.bg-accent")).toBeVisible();
    await shot(page, `rail-${mode}`);

    await setCards(page, CLOSED, "focus");
    expect(await openInStore(page), "matches open cards").toBe(0);
    const none = button(page, "Approvals");
    await expect(none).toBeVisible();
    await expect(none.locator("span.bg-accent")).toHaveCount(0);
  });
}
