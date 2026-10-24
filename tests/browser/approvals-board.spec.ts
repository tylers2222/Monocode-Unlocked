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
  await app.mount(REAL_DB, {
    handlers: {
      ...allowReservedNameDialog,
      "plugin:opener|open_url": () => null,
    },
  });
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
  await expect(gate).toContainText(
    "monocodefiled by Monocode Generalraised by MonoCode Fork Orchestrator",
  );
  await expect(gate.getByText("Pros")).toBeVisible();
  await expect(gate.getByRole("listitem")).toHaveText([
    "One caller identity and one gate, as the ticket asks",
    "No rebase or second code path to delete later",
    "S2 and S3 slip until #109 merges, likely a day",
    "This instance idles on account 1 meanwhile",
  ]);
  await expect(gate.getByText("Recommended")).toHaveCount(1);
  // A·2 draws the pill's label in #A9CFFB; the light theme uses --link-color.
  const recommended = gate.getByText("Recommended", { exact: true });
  await expect(recommended).toHaveCSS("color", "rgb(169, 207, 251)");
  await page.evaluate(() =>
    document.documentElement.classList.add("theme-light"),
  );
  await expect(recommended).toHaveCSS("color", "rgb(8, 99, 196)");
  await page.evaluate(() =>
    document.documentElement.classList.remove("theme-light"),
  );
  for (const text of [
    "Wait for #109, then build S2 on its caller identity",
    "S2 and S3 start when #109 merges. One gate, one identity path.",
    "Split the identity part of #109 into its own small PR",
    "MonoCode 2 opens it today; both instances build on it.",
  ])
    await expect(gate.getByText(text, { exact: true })).toBeVisible();
  for (const role of ["filed by", "raised by"])
    await expect(
      gate.locator("span", { hasText: role }).locator("svg.pixel-mascot"),
      `${role} shows the Mono's mascot`,
    ).toHaveCount(1);
  await expect(
    gate.getByRole("button", {
      name: /^(PR #109 caller identity|ADE-51|Approvals board frames)$/,
    }),
  ).toHaveText(["PR #109 caller identity", "ADE-51", "Approvals board frames"]);
  await gate.getByRole("button", { name: "ADE-51" }).click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        window
          .appBackend!.calls.filter(
            (call) => call.command === "plugin:opener|open_url",
          )
          .map((call) => (call.args as { url: string }).url),
      ),
    )
    .toEqual(["https://linear.app/adebuild/issue/ADE-51"]);
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
          origin: spoof,
          tradeoffs: { pros: [spoof], cons: [spoof] },
          options: [{ label: spoof, description: spoof, recommended: true }],
          links: [
            { kind: "pr", title: spoof, url: "https://example.com/pr/1" },
          ],
          createdAt: REAL_DB.now - 5 * MINUTE,
        },
      ],
    },
    { handlers: allowReservedNameDialog },
  );
  await openBoard(page, 5);
  await pickProject(page, /^monocode/);
  const spoofed = board(page).getByRole("article").first();
  const said = spoofed.getByText("Approve txet nedih now", { exact: true });
  await expect(
    said,
    "heading, raised by, pro, con, option, description, link",
  ).toHaveCount(7);
  for (const text of await said.all())
    await expect(text).toHaveAttribute("dir", "auto");
  expect(
    await spoofed.evaluate((node) => node.textContent?.includes("\u202E")),
    "bidi controls dropped",
  ).toBe(false);

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

test("a card redraws a Giga's mascot when the roster changes while the board is open", async ({
  app,
  page,
}) => {
  await app.mount(REAL_DB, { handlers: allowReservedNameDialog });
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);
  const filer = card(
    page,
    "Wait for PR #109 before the approvals tool is built?",
  )
    .locator("span", { hasText: "filed by" })
    .locator("svg.pixel-mascot");
  await expect(filer).toHaveCount(1);
  const before = await filer.innerHTML();

  // Another window gives Monocode General a new mascot: mono_roster.rs emits monos_changed.
  await page.evaluate(() => {
    const backend = window.appBackend!;
    const giga = backend.store.monos.find((mono) => mono.role === "giga")!;
    giga.data = { ...giga.data, mascot: "frog", color: "hsl(280 55% 62%)" };
    backend.store.rosterRevision += 1;
    return backend.emit("monos_changed", {
      revision: backend.store.rosterRevision,
      monos: backend.store.monos,
    });
  });
  await expect
    .poll(() => filer.innerHTML(), "the filer's mascot redraws")
    .not.toBe(before);
});

test("a one-sided card keeps both tradeoff columns and says None given", async ({
  app,
  page,
}) => {
  const approvals = REAL_DB.approvals.map((row) => {
    const { pros, cons } = row.tradeoffs;
    if (row.heading.startsWith("Wait for PR #109"))
      return { ...row, tradeoffs: { pros, cons: [] } };
    if (row.heading === "Keep Playwright at one worker in CI?")
      return { ...row, tradeoffs: { pros: [], cons } };
    if (row.heading.startsWith("Show decided cards"))
      return { ...row, tradeoffs: { pros: [], cons: [] } };
    return row;
  });
  await app.mount(
    { ...REAL_DB, approvals },
    { handlers: allowReservedNameDialog },
  );
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);

  const onlyPros = card(
    page,
    "Wait for PR #109 before the approvals tool is built?",
  ).getByRole("group");
  await expect(onlyPros).toHaveText([
    "ProsOne caller identity and one gate, as the ticket asksNo rebase or second code path to delete later",
    "ConsNone given",
  ]);
  await expect(onlyPros.nth(1).locator("li")).toHaveCount(0);

  const onlyCons = card(page, "Keep Playwright at one worker in CI?").getByRole(
    "group",
  );
  await expect(onlyCons).toHaveText([
    "ProsNone given",
    "Cons14-minute runs on every pushSlower feedback on the S6 E2E PRs",
  ]);
  await expect(onlyCons.first().locator("li")).toHaveCount(0);

  const neither = card(page, "Show decided cards for 30 days, then hide them?");
  await expect(neither.getByRole("group")).toHaveCount(0);
  await expect(neither.getByText("Tradeoffs", { exact: true })).toHaveCount(0);
  await expect(neither.getByText("Hide after 30 days")).toBeVisible();
  await shot(page, "board-one-sided");
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
