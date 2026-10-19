import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test, type FakeSeed } from "./app/fixture";

const ORBIT = "/Users/test/orbit";
const CHANGED = "monocode:milestones-changed";
const STALE =
  "The list changed while you moved that milestone, so it did not move. This is the latest order.";

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-10-18T12:00:00Z"));
});

function milestone(
  order: number,
  status: MilestoneStatus,
  title: string,
  fields: Partial<Milestone> = {},
): Milestone {
  return {
    id: `orbit-${order}`.padEnd(36, "0"),
    projectPath: ORBIT,
    title,
    note: "",
    status,
    target: null,
    order,
    createdBy: "user",
    createdAt: order,
    updatedAt: order,
    ...fields,
  };
}

/** The rows of approved frame M·6. */
const ROWS = [
  milestone(0, "in_progress", "Traces in Mono chats (change G)", {
    target: "2026-10-24",
  }),
  milestone(1, "in_progress", "Milestones page (change M)", {
    target: "2026-10-31",
  }),
  milestone(2, "planned", "Giga Mono harness (change H)", {
    target: "2026-11-21",
  }),
  milestone(3, "planned", "Promptfoo evals run in CI"),
  milestone(4, "idea", "Voice notes into Inbox"),
  milestone(5, "idea", "Share one Mono between two Macs"),
];
const [TRACES, , HARNESS, PROMPTFOO, VOICE, SHARE] = ROWS;

async function openPage(
  app: { mount: (seed: FakeSeed) => Promise<void> },
  page: Page,
  rows: Milestone[] = ROWS,
) {
  await app.mount({ projects: [ORBIT], milestones: rows });
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

const main = (page: Page) => page.locator("[data-app-milestones] main");
const row = (page: Page, id: string) =>
  page.locator(`[data-milestone="${id}"]`);
const titlesIn = (page: Page, group: string) =>
  main(page)
    .getByRole("region", { name: group, exact: true })
    .locator("[data-milestone]")
    .evaluateAll((rows) =>
      rows.map((row) => row.getAttribute("data-milestone")),
    );
const storedOrder = (page: Page) =>
  page.evaluate(() =>
    [...window.appBackend!.store.milestones]
      .sort((a, b) => a.order - b.order)
      .map((m) => [m.id, m.status]),
  );
const reorders = (page: Page) =>
  page.evaluate(() =>
    window.appBackend!.calls.filter(
      ({ command }) => command === "milestones_reorder",
    ),
  );
const notice = (page: Page) => main(page).getByText(STALE);

/** Presses the grip of `id`, moves to the upper or lower half of `target`, and stops before release. */
async function dragTo(
  page: Page,
  id: string,
  target: string,
  side: "before" | "after",
) {
  await row(page, id).hover();
  const grip = (await row(page, id).locator("[data-grip]").boundingBox())!;
  const over = (await row(page, target).boundingBox())!;
  await page.mouse.move(grip.x + grip.width / 2, grip.y + grip.height / 2);
  await page.mouse.down();
  const y = over.y + over.height * (side === "before" ? 0.25 : 0.75);
  await page.mouse.move(grip.x + grip.width / 2, y, { steps: 8 });
}

test("lane 5: a drag inside a group reorders, and the order holds after a restart", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before");
  await expect(
    row(page, HARNESS.id).locator("[data-drop-line]"),
  ).toHaveAttribute("data-drop-line", "before");
  await page.screenshot({ path: test.info().outputPath("reorder.png") });
  await page.mouse.up();
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([PROMPTFOO.id, HARNESS.id]);
  expect(await reorders(page)).toEqual([
    {
      command: "milestones_reorder",
      args: {
        projectPath: ORBIT,
        ids: [TRACES, ROWS[1], PROMPTFOO, HARNESS, VOICE, SHARE].map(
          (m) => m.id,
        ),
        by: "user",
      },
      reply: expect.any(Array),
    },
  ]);

  await app.restart();
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([PROMPTFOO.id, HARNESS.id]);
});

test("a drag across a group header moves the row and changes its status", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await dragTo(page, VOICE.id, PROMPTFOO.id, "after");
  await page.mouse.up();
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([HARNESS.id, PROMPTFOO.id, VOICE.id]);
  expect((await storedOrder(page)).find(([id]) => id === VOICE.id)).toEqual([
    VOICE.id,
    "planned",
  ]);
  await expect(row(page, VOICE.id)).not.toHaveCSS("transform", /matrix/);
});

test("keyboard: Alt+ArrowUp/Down moves the focused row, past group edges, and keeps focus", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, SHARE.id).focus();
  await page.keyboard.press("Alt+ArrowUp");
  await expect.poll(() => titlesIn(page, "Idea")).toEqual([SHARE.id, VOICE.id]);
  await expect(row(page, SHARE.id)).toBeFocused();

  await page.keyboard.press("Alt+ArrowUp");
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([HARNESS.id, PROMPTFOO.id, SHARE.id]);
  await expect(row(page, SHARE.id)).toBeFocused();
  expect((await storedOrder(page)).find(([id]) => id === SHARE.id)).toEqual([
    SHARE.id,
    "planned",
  ]);

  await page.keyboard.press("Alt+ArrowDown");
  await expect.poll(() => titlesIn(page, "Idea")).toEqual([SHARE.id, VOICE.id]);
  await expect(row(page, SHARE.id)).toBeFocused();
  await expect(page.locator("[data-milestone-editor]")).toHaveCount(0);
});

test("lane 8: an agent changes the list under a page reorder; stale_order reloads, says so, and nothing is lost", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  const agentRow = milestone(9, "idea", "Added by an agent, not yet seen", {
    createdBy: "mono:general",
  });
  await page.evaluate((row) => {
    window.appBackend!.store.milestones.push(row);
  }, agentRow);
  await row(page, SHARE.id).focus();
  await page.keyboard.press("Alt+ArrowUp");

  await expect(notice(page)).toBeVisible();
  await expect(row(page, agentRow.id)).toBeVisible();
  expect(await reorders(page)).toMatchObject([{ refused: "stale_order" }]);
  expect(await storedOrder(page)).toEqual(
    [...ROWS, agentRow].map((m) => [m.id, m.status]),
  );
  await page.screenshot({ path: test.info().outputPath("stale.png") });

  await row(page, SHARE.id).focus();
  await page.keyboard.press("Alt+ArrowUp");
  await expect
    .poll(() => titlesIn(page, "Idea"))
    .toEqual([SHARE.id, VOICE.id, agentRow.id]);
  await expect(notice(page)).toHaveCount(0);
});

test("an agent reorder during a drag: the drop is refused, the list refreshes, and the notice shows", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before");
  await page.evaluate(
    ({ ids, event, project }) => {
      ids.forEach((id, order) => {
        window.appBackend!.store.milestones.find((m) => m.id === id)!.order =
          order;
      });
      return window.appBackend!.emit(event, { projectPath: project });
    },
    {
      ids: [VOICE, SHARE, TRACES, ROWS[1], HARNESS, PROMPTFOO].map((m) => m.id),
      event: CHANGED,
      project: ORBIT,
    },
  );
  await expect.poll(() => titlesIn(page, "Idea")).toEqual([VOICE.id, SHARE.id]);
  await page.mouse.up();

  await expect(notice(page)).toBeVisible();
  expect(await reorders(page)).toEqual([]);
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([HARNESS.id, PROMPTFOO.id]);
});
