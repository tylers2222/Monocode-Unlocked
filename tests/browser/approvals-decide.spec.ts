import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
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
const GATE = "Wait for PR #109 before the approvals tool is built?";

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

async function mountOn(
  page: Page,
  mount: () => Promise<void>,
  project: RegExp,
) {
  await mount();
  await page.getByRole("button", { name: /^Approvals, \d+ open$/ }).click();
  await board(page)
    .locator("aside")
    .getByRole("button", { name: project })
    .click();
}

const decisions = (page: Page) =>
  page.evaluate(() =>
    window
      .appBackend!.calls.filter((call) => call.command === "approvals_decide")
      .map(({ args, refused }) => ({ args, refused })),
  );

const badge = (page: Page, open: number) =>
  expect(
    page.getByRole("button", { name: `Approvals, ${open} open`, exact: true }),
  ).toBeVisible();

test("the picker ticks by key and click, and Approve sends the ticked options", async ({
  app,
  page,
}) => {
  await mountOn(
    page,
    () => app.mount(REAL_DB, { handlers: allowReservedNameDialog }),
    /^monocode/,
  );
  const gate = card(page, GATE);
  const rows = gate.getByRole("checkbox");
  const approve = gate.getByRole("button", { name: /^Approve/ });
  await expect(rows).toHaveCount(3);
  for (const row of await rows.all()) await expect(row).not.toBeChecked();
  await expect(approve).toBeDisabled();
  await expect(approve).toHaveText("Approve");
  await expect(gate.getByText("Tick an option to approve")).toBeHidden();

  await gate.focus();
  await expect(gate.getByText("Tick an option to approve")).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Space");
  await expect(rows.nth(1)).toBeChecked();
  await expect(approve).toHaveText("Approve 1 option");
  await expect(approve).toBeEnabled();
  await page.keyboard.press("3");
  await expect(rows.nth(2)).toBeChecked();
  await expect(approve).toHaveText("Approve 2 options");
  await shot(page, "picker-two");

  await rows.nth(2).click();
  await expect(rows.nth(2)).not.toBeChecked();
  await expect(approve).toHaveText("Approve 1 option");
  const comment = gate.getByRole("textbox", { name: "Comment" });
  await expect(comment).toHaveAttribute("dir", "auto");
  await comment.fill("Option 2 first, then #109.");
  await comment.press("1");
  await expect(
    rows.nth(0),
    "typing a digit in the comment ticks nothing",
  ).not.toBeChecked();
  await comment.press("Meta+Enter");

  await badge(page, 3);
  expect(await decisions(page)).toEqual([
    {
      args: {
        id: REAL_DB.approvals.find((row) => row.heading === GATE)!.id,
        verdict: "approved",
        note: "Option 2 first, then #109.1",
        chosen: ["Build S2 on tyler/main now, rebase onto #109 later"],
      },
      refused: undefined,
    },
  ]);
  await expect(card(page, GATE)).toHaveCount(0);
  await board(page)
    .locator("main")
    .getByRole("button", { name: /^Approved.*#109 before/ })
    .click();
  const decided = card(page, GATE);
  await expect(decided).toContainText("Approved by you · Oct 19, 10:54 pm");
  await expect(decided.getByRole("checkbox", { checked: true })).toHaveText(
    "Build S2 on tyler/main now, rebase onto #109 later",
  );
  await expect(decided).toContainText(
    "Sent to Monocode General as a queued follow-up · 10:54 pm",
  );
});

test("the cursor starts on the recommended row, wherever it is", async ({
  app,
  page,
}) => {
  const seed = {
    ...REAL_DB,
    approvals: REAL_DB.approvals.map((row) =>
      row.heading === GATE
        ? { ...row, options: [...row.options].reverse() }
        : row,
    ),
  };
  await mountOn(
    page,
    () => app.mount(seed, { handlers: allowReservedNameDialog }),
    /^monocode/,
  );
  const gate = card(page, GATE);
  await gate.focus();
  await page.keyboard.press("Space");
  await expect(gate.getByRole("checkbox", { checked: true })).toHaveCount(1);
  await expect(gate.getByRole("checkbox").nth(2)).toBeChecked();
  await shot(page, "picker-open");
});

test("Reject and Ask for changes send the ticks and the comment; a blank comment is none", async ({
  app,
  page,
}) => {
  await mountOn(
    page,
    () => app.mount(REAL_DB, { handlers: allowReservedNameDialog }),
    /^monocode/,
  );
  const ci = card(page, "Keep Playwright at one worker in CI?");
  await expect(ci.getByRole("button", { name: "Approve" })).toBeEnabled();
  await ci.getByRole("textbox", { name: "Comment" }).fill("   ");
  await ci.getByRole("button", { name: "Reject" }).click();
  await badge(page, 3);

  const hide = card(page, "Show decided cards for 30 days, then hide them?");
  await hide.getByRole("checkbox").first().click();
  await hide.getByRole("textbox", { name: "Comment" }).fill("Make it 60 days.");
  await hide.getByRole("button", { name: "Ask for changes" }).click();
  await badge(page, 2);

  expect((await decisions(page)).map(({ args }) => args)).toEqual([
    {
      id: REAL_DB.approvals.find((row) =>
        row.heading.startsWith("Keep Playwright"),
      )!.id,
      verdict: "rejected",
      note: null,
      chosen: [],
    },
    {
      id: REAL_DB.approvals.find((row) =>
        row.heading.startsWith("Show decided"),
      )!.id,
      verdict: "changes_requested",
      note: "Make it 60 days.",
      chosen: ["Hide after 30 days"],
    },
  ]);
  await expect(
    board(page).getByText("1 open · 1 blocking · 6 decided"),
  ).toBeVisible();
});

test("a card whose Giga was removed still decides, and nothing is sent", async ({
  app,
  page,
}) => {
  await mountOn(
    page,
    () => app.mount(REAL_DB, { handlers: allowReservedNameDialog }),
    /^Modelist/,
  );
  const csv = card(page, "Drop the CSV export from Modelist v1?");
  await expect(
    csv.getByText("Deciding closes the card here. Nothing is sent."),
  ).toBeVisible();
  await csv.getByRole("checkbox").first().click();
  await expect(csv.getByRole("button", { name: /^Approve/ })).toHaveText(
    "Approve 1 option",
  );
  await shot(page, "removed-open");
  await csv.getByRole("button", { name: /^Approve/ }).click();
  await badge(page, 3);
  await board(page)
    .locator("main")
    .getByRole("button", { name: /^Approved.*CSV export/ })
    .click();
  const decided = card(page, "Drop the CSV export from Modelist v1?");
  await expect(decided).toContainText(
    "Not sent: the Giga that filed this was removed.",
  );
  await expect(decided.getByRole("deletion")).toHaveText("Modelist General");
  await shot(page, "removed-decided");
});

const [WAIT, BUILD, SPLIT] = REAL_DB.approvals
  .find((row) => row.heading === GATE)!
  .options.map((option) => option.label);

test("a Giga update between ticking and deciding never moves a tick to another option", async ({
  app,
  page,
}) => {
  await mountOn(
    page,
    () => app.mount(REAL_DB, { handlers: allowReservedNameDialog }),
    /^monocode/,
  );
  const gate = card(page, GATE);
  await gate.getByRole("checkbox", { name: BUILD }).click();
  await gate.getByRole("checkbox", { name: SPLIT }).click();
  await expect(gate.getByRole("button", { name: /^Approve/ })).toHaveText(
    "Approve 2 options",
  );

  await page.evaluate(
    ({ heading, wait, split }) => {
      const card = window.appBackend!.store.approvals.find(
        (row) => row.heading === heading,
      )!;
      card.options = card.options
        .filter((option) => option.label !== wait)
        .map((option) =>
          option.label === split
            ? { ...option, label: "Split the identity part out" }
            : option,
        );
      return window.appBackend!.emit("approvals-changed", {});
    },
    { heading: GATE, wait: WAIT, split: SPLIT },
  );
  const rows = gate.getByRole("checkbox");
  await expect(rows).toHaveText([
    /Build S2 on tyler\/main now/,
    /Split the identity part out/,
  ]);
  await expect(rows.nth(0), "the tick stays on its option").toBeChecked();
  await expect(
    rows.nth(1),
    "a renamed option is a new option",
  ).not.toBeChecked();
  await expect(gate.getByRole("button", { name: /^Approve/ })).toHaveText(
    "Approve 1 option",
  );
  await gate.getByRole("button", { name: /^Approve/ }).click();
  await badge(page, 3);
  expect((await decisions(page)).map(({ args }) => args)).toEqual([
    expect.objectContaining({ verdict: "approved", chosen: [BUILD] }),
  ]);
});

test("the key hints count only the options a number can tick", async ({
  app,
  page,
}) => {
  const approvals = REAL_DB.approvals.map((row) =>
    row.heading.startsWith("Show decided")
      ? { ...row, options: row.options.slice(0, 1) }
      : row.heading.startsWith("Keep Playwright")
        ? {
            ...row,
            options: Array.from({ length: 12 }, (_, index) => ({
              label: `Worker count ${index + 1}`,
              description: null,
              recommended: false,
            })),
          }
        : row,
  );
  await mountOn(
    page,
    () =>
      app.mount(
        { ...REAL_DB, approvals },
        { handlers: allowReservedNameDialog },
      ),
    /^monocode/,
  );
  const one = card(page, "Show decided cards for 30 days, then hide them?");
  await one.focus();
  await expect(one.locator("kbd")).toHaveText([
    "↑",
    "↓",
    "Space",
    "1",
    "⌘",
    "↩",
  ]);
  await expect(one).not.toContainText("–");

  const twelve = card(page, "Keep Playwright at one worker in CI?");
  await twelve.focus();
  await expect(twelve.locator("kbd")).toHaveText([
    "↑",
    "↓",
    "Space",
    "1",
    "9",
    "⌘",
    "↩",
  ]);
  await page.keyboard.press("9");
  await expect(
    twelve.getByRole("checkbox", { name: "Worker count 9" }),
  ).toBeChecked();
});
