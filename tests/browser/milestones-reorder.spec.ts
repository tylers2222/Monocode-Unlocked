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

const lifted = (page: Page) => page.locator("[data-lifted]");

/**
 * Presses the grip of `id` with `button`, moves to the upper or lower half of
 * `target`, and stops before release. The lifted card takes space as it goes,
 * so the last move aims at where `target` is then.
 */
async function dragTo(
  page: Page,
  id: string,
  target: string,
  side: "before" | "after",
  button: "left" | "right" = "left",
) {
  await row(page, id).hover();
  const grip = (await row(page, id).locator("[data-grip]").boundingBox())!;
  const x = grip.x + grip.width / 2;
  const aim = async () => {
    const over = (await row(page, target).boundingBox())!;
    return over.y + over.height * (side === "before" ? 0.25 : 0.75);
  };
  await page.mouse.move(x, grip.y + grip.height / 2);
  await page.mouse.down({ button });
  await page.mouse.move(x, await aim(), { steps: 8 });
  await page.mouse.move(x, await aim(), { steps: 2 });
}

/** The lifted card sits on `side` of `target`, with the drop line on its top edge, as frame M·6 draws. */
async function expectLiftedBeside(
  page: Page,
  target: string,
  side: "before" | "after",
) {
  await expect(lifted(page)).toHaveAttribute("data-lifted", side);
  const card = (await lifted(page).boundingBox())!;
  const line = (await lifted(page).locator("[data-drop-line]").boundingBox())!;
  const over = (await row(page, target).boundingBox())!;
  expect(Math.abs(line.y - card.y)).toBeLessThan(8);
  if (side === "after")
    expect(card.y).toBeGreaterThanOrEqual(over.y + over.height);
  else expect(card.y + card.height).toBeLessThanOrEqual(over.y);
}

test("lane 5: a drag inside a group reorders, and the order holds after a restart", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before");
  await expectLiftedBeside(page, HARNESS.id, "before");
  await expect(lifted(page)).toContainText(PROMPTFOO.title);
  await expect(row(page, PROMPTFOO.id)).toHaveCSS("opacity", "0.35");
  await page.mouse.up();
  await expect(row(page, PROMPTFOO.id)).toHaveCSS("opacity", "1");
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
  await expectLiftedBeside(page, PROMPTFOO.id, "after");
  // The next group's header stays visible under the lifted card.
  const header = main(page)
    .getByRole("region", { name: "Idea", exact: true })
    .locator("h3");
  const card = (await lifted(page).boundingBox())!;
  expect((await header.boundingBox())!.y).toBeGreaterThanOrEqual(
    card.y + card.height,
  );
  await page.screenshot({ path: test.info().outputPath("reorder.png") });
  await page.mouse.up();
  await expect
    .poll(() => titlesIn(page, "Planned"))
    .toEqual([HARNESS.id, PROMPTFOO.id, VOICE.id]);
  expect((await storedOrder(page)).find(([id]) => id === VOICE.id)).toEqual([
    VOICE.id,
    "planned",
  ]);
  await expect(lifted(page)).toHaveCount(0);
});

test("keyboard: Alt+ArrowUp/Down moves the focused row, past group edges, and keeps focus", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.evaluate(() => {
    window.addEventListener("keydown", (event) => {
      if (event.key.startsWith("Arrow"))
        document.body.dataset.altKeyHandled = String(event.defaultPrevented);
    });
  });
  await row(page, SHARE.id).focus();
  await page.keyboard.press("Alt+ArrowUp");
  await expect.poll(() => titlesIn(page, "Idea")).toEqual([SHARE.id, VOICE.id]);
  await expect(row(page, SHARE.id)).toBeFocused();
  // The page does not also scroll: the key's default action is stopped.
  await expect(page.locator("body")).toHaveAttribute(
    "data-alt-key-handled",
    "true",
  );

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

test("an agent deletes the row being dragged: no crash, no write, and the notice on release", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before");
  await expect(lifted(page)).toBeVisible();
  await page.evaluate(
    ({ id, event, project }) => {
      const store = window.appBackend!.store;
      store.milestones = store.milestones.filter((m) => m.id !== id);
      return window.appBackend!.emit(event, { projectPath: project });
    },
    { id: PROMPTFOO.id, event: CHANGED, project: ORBIT },
  );
  await expect(row(page, PROMPTFOO.id)).toHaveCount(0);
  await expect(row(page, HARNESS.id)).toBeVisible();
  expect(errors).toEqual([]);
  await expect(lifted(page)).toHaveCount(0);
  await page.mouse.move(0, 10, { steps: 2 });
  await page.mouse.up();

  await expect(notice(page)).toBeVisible();
  expect(errors).toEqual([]);
  expect(await reorders(page)).toEqual([]);
  expect(await titlesIn(page, "Planned")).toEqual([HARNESS.id]);
});

test("a right-button drag on the grip moves nothing", async ({ app, page }) => {
  await openPage(app, page);
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before", "right");
  await expect(lifted(page)).toHaveCount(0);
  await page.mouse.up({ button: "right" });
  expect(await reorders(page)).toEqual([]);
  expect(await titlesIn(page, "Planned")).toEqual([HARNESS.id, PROMPTFOO.id]);
});

test("a cancelled pointer ends the drag: the card and the line go, and nothing moves", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await dragTo(page, PROMPTFOO.id, HARNESS.id, "before");
  await expect(lifted(page)).toBeVisible();
  await page.locator("[data-timeline]").dispatchEvent("pointercancel", {
    pointerId: 1,
    isPrimary: true,
  });
  await expect(lifted(page)).toHaveCount(0);
  await expect(page.locator("[data-drop-line]")).toHaveCount(0);
  await expect(row(page, PROMPTFOO.id)).toHaveCSS("opacity", "1");
  await page.mouse.up();
  expect(await reorders(page)).toEqual([]);
});

test("after a key move, focus left elsewhere does not jump back on later agent changes", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, SHARE.id).focus();
  await page.keyboard.press("Alt+ArrowUp");
  await expect(row(page, SHARE.id)).toBeFocused();
  await page.evaluate(() => (document.activeElement as HTMLElement).blur());
  await expect(page.locator("body")).toBeFocused();
  // An agent retitles another row, then moves the moved row to another group,
  // which mounts it again.
  const agent = (id: string, change: Partial<Milestone>) =>
    page.evaluate(
      ({ id, change, event, project }) => {
        Object.assign(
          window.appBackend!.store.milestones.find((m) => m.id === id)!,
          change,
        );
        return window.appBackend!.emit(event, { projectPath: project });
      },
      { id, change, event: CHANGED, project: ORBIT },
    );
  await agent(HARNESS.id, { title: "Giga Mono harness (agent retitle)" });
  await expect(row(page, HARNESS.id)).toContainText("agent retitle");
  await expect(page.locator("body")).toBeFocused();
  await agent(SHARE.id, { status: "planned" });
  await expect.poll(() => titlesIn(page, "Planned")).toContain(SHARE.id);
  await expect(page.locator("body")).toBeFocused();
});
