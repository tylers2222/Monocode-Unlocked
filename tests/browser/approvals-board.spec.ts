import { readFileSync } from "node:fs";
import type { Locator, Page } from "@playwright/test";
import Delete02Icon from "@hugeicons/core-free-icons/Delete02Icon";
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
// icons.tsx's Trash2. An icon renders no name, so it is matched by its path data.
const TRASH2 = `svg${Delete02Icon.flatMap(([tag, attrs]) =>
  tag === "path" ? [`:has(> path[d="${String(attrs.d)}"])`] : [],
).join("")}`;

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
const rows = (page: Page) =>
  board(page)
    .locator("main")
    .getByRole("button", {
      name: /^(Approved|Rejected|Changes requested|Withdrawn)/,
    });
const row = (page: Page, heading: string) =>
  rows(page).filter({ hasText: heading });
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
  await expect(board(page).getByText(/^Decided\s*4$/)).toBeVisible();
  await expect(rows(page), "decided rows, newest decision first").toHaveText([
    /^ApprovedUse a fake clock in the E2E suiteMonocode General · waiting to send · 2h ago$/,
    /^Changes requestedIndex approvals on \(project_id, status\) onlyMonocode General · yesterday$/,
    /^RejectedStore the Giga and Mono discussion on the cardMonocode General · not sent · yesterday$/,
    /^WithdrawnBump Playwright to 1.49Monocode General · Oct 17$/,
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

  await expect(
    row(page, "Store the Giga")
      .getByText("not sent", { exact: true })
      .locator(TRASH2),
    "the row's Trash2",
  ).toHaveCount(1);

  await row(page, "Index approvals").click();
  const changes = card(page, "Index approvals on (project_id, status) only");
  await expect(changes).toContainText("Changes requestedNot blockingAPR-");
  await expect(changes).toContainText("decided yesterday");
  await expect(changes).toContainText(
    "Changes requested by you · Oct 18, 6:05 pm",
  );
  await expect(changes.getByRole("checkbox", { checked: true })).toHaveText(
    "Also index (requested_by, status)",
  );
  await expect(changes.getByRole("checkbox", { checked: false })).toHaveText(
    "Index (project_id, status) only",
  );
  await expect(changes).toContainText(
    "“Take option 2 as well, then file it again with the query plan from the copied database.”",
  );
  await expect(changes).toContainText(
    "Sent to Monocode General as a queued follow-up · Oct 18, 6:05 pm",
  );

  await row(page, "Store the Giga").click();
  const rejected = card(page, "Store the Giga and Mono discussion on the card");
  await expect(rejected).toContainText("Rejected by you · Oct 18, 5:51 pm");
  await expect(rejected.getByRole("checkbox")).toHaveCount(0);
  await expect(rejected).toContainText(
    "“No. The Giga has the context; the board stays small.”",
  );
  // Removed from the Giga's queue by the user: never sent (A·5c), grey, no action.
  await expect(rejected).not.toContainText("Sent to");
  const unqueued = rejected.getByText(
    "Not sent: you removed it from Monocode General’s queue.",
  );
  await expect(unqueued).toBeVisible();
  await expect(
    unqueued.locator("xpath=..").locator(TRASH2),
    "the line's Trash2",
  ).toHaveCount(1);
  await expect(rejected.getByRole("button", { name: /^Open / })).toHaveCount(0);
  await expect(rejected.locator(".text-amber-400")).toHaveCount(0);

  await row(page, "Bump Playwright").click();
  const withdrawn = card(page, "Bump Playwright to 1.49");
  await expect(withdrawn).toContainText("WithdrawnNot blockingAPR-");
  await expect(withdrawn).toContainText(
    "Withdrawn by Monocode General · Oct 17, 4:12 pm",
  );
  await expect(withdrawn).toContainText(
    "Nothing to send: the filing Giga withdrew it. You cannot decide a withdrawn card.",
  );
  await expect(withdrawn).not.toContainText("Sent to");

  await row(page, "Use a fake clock").click();
  const waiting = card(page, "Use a fake clock in the E2E suite");
  await expect(waiting).toContainText("decided 8:54 pm");
  await expect(waiting).toContainText("Approved by you · Oct 19, 8:54 pm");
  await expect(waiting).toContainText(
    "Waiting to send: Monocode General has no chat to receive it yet.",
  );
  await expect(
    waiting.getByRole("button", { name: "Open Monocode General" }),
  ).toBeVisible();
  // The Giga still exists: none of A·5's removed-Giga marks (strike, "removed", amber).
  await expect(waiting.getByRole("deletion")).toHaveCount(0);
  await expect(waiting).not.toContainText("removed");
  await expect(waiting).not.toContainText("Not sent");
  await expect(waiting.locator(".text-amber-400")).toHaveCount(0);
  await expect(waiting).not.toContainText("Sent to");
  await shot(page, "board-decided");

  await pickProject(page, /^Modelist/);
  await expect(
    board(page).getByText("1 open · 1 blocking · 3 decided"),
  ).toBeVisible();
  await expect(
    card(page, "Drop the CSV export from Modelist v1?"),
  ).toContainText("yesterday");
  const orphan = card(page, "Drop the CSV export from Modelist v1?");
  await expect(orphan.getByRole("deletion")).toHaveText("Modelist General");
  await expect(orphan).toContainText("filed by Modelist Generalremoved");
  await expect(orphan).toContainText(
    "The Giga that filed this was removed. Handle this manually.",
  );
  await expect(orphan).toContainText(
    "You can still decide it to close the card. Nothing is sent: there is no Giga to receive it.",
  );
  // A removed filer wins over the queue: one card was waiting for the Giga's
  // next chat and one was removed from its queue before the Giga was deleted.
  await expect(rows(page), "each row reads A·5's not sent").toHaveText([
    /^RejectedRename presets to profiles in v1\?Modelist General not sent · 7h ago$/,
    /^ApprovedPin the model list to one provider API version\?Modelist General not sent · 8h ago$/,
    /^ApprovedKeep the model picker in v1\?Modelist General not sent · 9h ago$/,
  ]);
  await expect(rows(page).getByRole("deletion")).toHaveText([
    "Modelist General",
    "Modelist General",
    "Modelist General",
  ]);
  for (const heading of [
    "Rename presets to profiles in v1?",
    "Pin the model list to one provider API version?",
    "Keep the model picker in v1?",
  ]) {
    await row(page, heading).click();
    const gone = card(page, heading);
    const line = gone.locator(".text-amber-400", {
      hasText: "Not sent: the Giga that filed this was removed.",
    });
    await expect(line, `${heading} shows A·5's amber line`).toHaveCount(1);
    await expect(line.locator("svg")).toHaveCount(1);
    await expect(gone).not.toContainText("Waiting to send");
    await expect(gone).not.toContainText("’s queue");
    await expect(gone.getByRole("button", { name: /^Open / })).toHaveCount(0);
  }
  await shot(page, "board-removed");
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
  await expect(row(page, "Approve txet")).toContainText("1m ago");
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

test("a closed card isolates its filer's name and strikes it once the Giga is gone", async ({
  app,
  page,
}) => {
  const name = "Mono \u202Elareneg\u202C";
  const shown = "Mono lareneg";
  const approvals = REAL_DB.approvals.map((row) => {
    if (row.heading === "Bump Playwright to 1.49")
      return { ...row, requestedBy: { id: null, name } };
    if (
      row.heading.startsWith("Index approvals") &&
      row.status.state === "decided"
    )
      return {
        ...row,
        requestedBy: { ...row.requestedBy, name },
        status: { ...row.status, decidedAt: REAL_DB.now - 30 * MINUTE },
      };
    if (
      row.heading.startsWith("Use a fake clock") ||
      row.heading.startsWith("Store the Giga")
    )
      return { ...row, requestedBy: { ...row.requestedBy, name } };
    return row;
  });
  await app.mount(
    { ...REAL_DB, approvals },
    { handlers: allowReservedNameDialog },
  );
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);

  const withdrawnRow = row(page, "Bump Playwright");
  await expect(withdrawnRow.getByRole("deletion")).toHaveText(shown);
  const sentRow = row(page, "Index approvals");
  await expect(sentRow.getByRole("deletion")).toHaveCount(0);
  await expect(sentRow).toContainText(`${shown} · 30m ago`);

  await withdrawnRow.click();
  await expect(card(page, "Bump Playwright to 1.49")).toContainText(
    `Withdrawn by ${shown} · Oct 17, 4:12 pm`,
  );
  await sentRow.click();
  await expect(
    card(page, "Index approvals on (project_id, status) only"),
  ).toContainText(`Sent to ${shown} as a queued follow-up · 10:24 pm`);

  await row(page, "Use a fake clock").click();
  const waiting = card(page, "Use a fake clock in the E2E suite");
  await expect(waiting).toContainText(
    `Waiting to send: ${shown} has no chat to receive it yet.`,
  );
  await expect(
    waiting.getByRole("button", { name: `Open ${shown}`, exact: true }),
  ).toBeVisible();

  await row(page, "Store the Giga").click();
  await expect(
    card(page, "Store the Giga and Mono discussion on the card"),
  ).toContainText(`Not sent: you removed it from ${shown}’s queue.`);

  const names = board(page).locator("main").getByText(shown, { exact: true });
  await expect(
    names,
    "four cards: filed by, Withdrawn by, Sent to, the waiting line and its Open button, and the removed-from-queue line",
  ).toHaveCount(9);
  for (const text of await names.all())
    await expect(text).toHaveAttribute("dir", "auto");
  expect(
    await board(page).evaluate((node) => node.textContent?.includes("\u202E")),
    "bidi controls dropped",
  ).toBe(false);
});

test("a decision waiting for the Giga's chat offers the Giga, then turns Sent once delivered", async ({
  app,
  page,
}) => {
  // The Giga has no chat, so its dropped decision waits (A·5b).
  const monos = REAL_DB.monos.map((mono) =>
    mono.role === "giga"
      ? { ...mono, data: { ...mono.data, sessionId: undefined } }
      : mono,
  );
  await app.mount(
    { ...REAL_DB, monos },
    {
      handlers: {
        ...allowReservedNameDialog,
        session_checkpoint_keep: () => null,
      },
    },
  );
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);
  const waitingRow = row(page, "Use a fake clock");
  await expect(
    waitingRow.getByText("waiting to send", { exact: true }).locator("svg"),
    "the clock",
  ).toHaveCount(1);
  await expect(waitingRow.getByRole("deletion")).toHaveCount(0);
  await expect(waitingRow.locator(".text-amber-400")).toHaveCount(0);
  await waitingRow.click();
  const fakeClock = card(page, "Use a fake clock in the E2E suite");
  await expect(
    fakeClock
      .getByText(/^Waiting to send: /)
      .locator("xpath=..")
      .locator("xpath=./*[local-name()='svg']"),
    "the waiting line's clock",
  ).toHaveCount(1);
  await shot(page, "board-waiting");

  // Open gives the Giga a chat (monos_update), and the fake's requeue sends
  // the decision there, as #125's does.
  await fakeClock
    .getByRole("button", { name: "Open Monocode General" })
    .click();
  await expect(
    page.getByRole("textbox", { name: "Message Monocode General" }),
  ).toBeVisible();
  await openBoard(page, 4);
  await pickProject(page, /^monocode/);
  await expect(row(page, "Use a fake clock")).toHaveText(
    /^ApprovedUse a fake clock in the E2E suiteMonocode General · 2h ago$/,
  );
  await row(page, "Use a fake clock").click();
  await expect(fakeClock).toContainText(
    "Sent to Monocode General as a queued follow-up · 8:54 pm",
  );
  await expect(fakeClock.getByRole("button", { name: /^Open / })).toHaveCount(
    0,
  );

  // With the board open, the user removes that decision from the Giga's queue
  // in another window: #125 emits approvals-changed and the card redraws.
  await page.evaluate(() => {
    const backend = window.appBackend!;
    const card = backend.store.approvals.find((row) =>
      row.heading.startsWith("Use a fake clock"),
    )!;
    if (card.status.state === "decided")
      card.status = { ...card.status, undelivered: "removed" };
    return backend.emit("approvals-changed", null);
  });
  await expect(fakeClock).toContainText(
    "Not sent: you removed it from Monocode General’s queue.",
  );
  await expect(fakeClock).not.toContainText("Sent to");
});
