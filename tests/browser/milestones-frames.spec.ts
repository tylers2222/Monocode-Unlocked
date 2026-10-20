import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";
import {
  AGENT_ROW,
  CHANGED,
  EVERY_STATUS,
  ORBIT,
  REVIEW,
  seed,
} from "./milestones-world";

/**
 * M-11: one screenshot of every Milestones state an approved frame draws
 * (doop canvas oRuRCkta7A, page "Change M: Milestones"), named after its
 * frame, for change-m/reviews/m11-compare.md. Each shot also checks the
 * frame's words, so a state that stops matching fails here, not only by eye.
 */

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date(2026, 9, 18, 10));
});

const main = (page: Page) => page.locator("[data-app-milestones] main");
const row = (page: Page, title: string) =>
  main(page).locator("[data-milestone]", { hasText: title });
const editor = (page: Page) => page.locator("[data-milestone-editor]");
const popover = (page: Page) =>
  page.getByRole("dialog", { name: /^Milestones in / });
/** A shot once entrance animations end, so a popover or a row is not caught mid-fade. */
async function frame(page: Page, name: string) {
  await page.waitForFunction(() =>
    document
      .getAnimations()
      .every(
        (animation) =>
          animation.playState !== "running" ||
          animation.effect?.getComputedTiming().iterations === Infinity,
      ),
  );
  await page.screenshot({ path: test.info().outputPath(`${name}.png`) });
}

async function openMilestones(page: Page) {
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

test("M·1 and M·2: Automations keeps its page; Milestones is its own row under it", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await page.getByRole("button", { name: "Automations", exact: true }).click();
  await expect(page.getByRole("region", { name: "Automations" })).toBeVisible();
  await frame(page, "M1-automations");
  await openMilestones(page);
  const entries = page.getByRole("button", {
    name: /^(Automations|Milestones)$/,
  });
  await expect(entries).toHaveText(["Automations", "Milestones"]);
  await frame(page, "M0-M2-entry");
});

test("M·3: an empty project shows one line of help and one button", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await page
    .locator("[data-app-milestones] aside")
    .getByRole("button", { name: /^orbit-review/ })
    .click();
  await expect(
    main(page).getByRole("heading", {
      name: "No milestones for orbit-review yet",
    }),
  ).toBeVisible();
  await expect(main(page)).toContainText("No milestones yet");
  await expect(
    main(page).getByRole("button", { name: "Add milestone" }),
  ).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "Milestone settings" }),
  ).toBeVisible();
  await frame(page, "M3-empty");
});

test("M·4: every status on one timeline, Done first and the Today line before In progress", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await expect(main(page)).toContainText("8 milestones · 2 in progress");
  await expect(main(page).getByRole("region")).toHaveText([
    /^Done2/,
    /^In progress2/,
    /^Planned2/,
    /^Idea2/,
  ]);
  await expect(main(page).locator("[data-today]")).toContainText(
    "Today · Oct 18",
  );
  await expect(row(page, "Promptfoo evals run in CI")).toContainText("Later");
  await frame(page, "M4-every-status");
});

test("M·5: Add milestone opens one card at the top", async ({ app, page }) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Milestone list in the Mono overview panel");
  await editor(page).getByRole("radio", { name: "Planned" }).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Dec 12, 2026");
  await expect(editor(page).getByLabel("Note")).toHaveAttribute(
    "placeholder",
    "Note (optional)",
  );
  for (const name of ["Cancel", "Add"])
    await expect(
      editor(page).getByRole("button", { name, exact: true }),
    ).toBeVisible();
  await frame(page, "M5-add");
});

test("M·6: a row click edits it in place; hover shows the grip; mid-drag shows the card and the line", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await row(page, "Giga Mono harness (change H)").click();
  await expect(editor(page).getByLabel("Target date")).toHaveValue(
    "Nov 21, 2026",
  );
  for (const name of ["Delete", "Cancel", "Save"])
    await expect(
      editor(page).getByRole("button", { name, exact: true }),
    ).toBeVisible();
  await frame(page, "M6-edit");
  await editor(page).getByRole("button", { name: "Cancel" }).click();
  await expect(editor(page)).toHaveCount(0);

  const promptfoo = row(page, "Promptfoo evals run in CI");
  await promptfoo.hover();
  await expect(promptfoo.locator("[data-grip]")).toHaveCSS("opacity", "1");
  await frame(page, "M6-hover-grip");
  const grip = (await promptfoo.locator("[data-grip]").boundingBox())!;
  const above = (await row(
    page,
    "Giga Mono harness (change H)",
  ).boundingBox())!;
  const x = grip.x + grip.width / 2;
  await page.mouse.move(x, grip.y + grip.height / 2);
  await page.mouse.down();
  await page.mouse.move(x, above.y + above.height * 0.25, { steps: 8 });
  const lifted = page.locator("[data-lifted]");
  await expect(lifted).toContainText("Same suite style as change E");
  await expect(lifted.getByText("Later", { exact: true })).toHaveCSS(
    "border-top-style",
    "dashed",
  );
  // The card's one icon is its grip.
  await expect(lifted.locator("svg")).toHaveCount(1);
  await frame(page, "M6-reorder-mid-drag");
  await page.mouse.move(x, grip.y + grip.height / 2, { steps: 4 });
  await page.mouse.up();
});

test("M·7: Delete asks once inline in the edit card", async ({ app, page }) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await row(page, "Voice notes into Inbox").click();
  await editor(page).getByRole("button", { name: "Delete" }).click();
  await expect(editor(page)).toContainText(
    "Delete “Voice notes into Inbox”? This can’t be undone.",
  );
  await expect(
    editor(page).getByRole("button", { name: "Keep" }),
  ).toBeFocused();
  // The Delete click left the mouse over a row; the frame shows no hover grip.
  await page.mouse.move(1430, 890);
  await frame(page, "M7-delete");
});

test("M·8 and the M-10 decisions: the settings popover, Done hidden, one project off", async ({
  app,
  page,
}) => {
  await app.mount(
    seed(EVERY_STATUS, {
      milestoneSettings: {
        [ORBIT]: { enabled: true, hiddenStatuses: ["done"] },
        [REVIEW]: { enabled: false, hiddenStatuses: [] },
      },
    }),
  );
  await openMilestones(page);
  await expect(main(page)).toContainText("8 milestones · Done hidden");
  await expect(main(page).locator("[data-today]")).toHaveCount(0);
  await expect(
    page.locator("[data-app-milestones] aside button").last(),
  ).toHaveText("orbit-reviewOff");
  await page.getByRole("button", { name: "Milestone settings" }).click();
  await expect(popover(page)).toContainText("Milestones in orbit");
  // Tyler's M-10 decision (a) for the switch text.
  await expect(popover(page)).toContainText(
    "Off stops agents from changing milestones here.",
  );
  await expect(popover(page)).toContainText(
    /Show on the timeline\s*In progress\s*2\s*Planned\s*2\s*Idea\s*2\s*Done\s*2/,
  );
  await expect(
    popover(page).getByRole("checkbox", { name: "Done", exact: true }),
  ).not.toBeChecked();
  await expect(popover(page)).toContainText(
    "Hidden milestones stay saved, and agents can still list them.",
  );
  await frame(page, "M8-settings");
});

test("M-10 decision: every status hidden keeps the header and draws no group", async ({
  app,
  page,
}) => {
  await app.mount(
    seed(EVERY_STATUS, {
      milestoneSettings: {
        [ORBIT]: {
          enabled: true,
          hiddenStatuses: ["idea", "planned", "in_progress", "done"],
        },
      },
    }),
  );
  await openMilestones(page);
  await expect(main(page)).toContainText(
    "8 milestones · Done, In progress, Planned, Idea hidden",
  );
  await expect(main(page).getByRole("region")).toHaveCount(0);
  await frame(page, "M10-all-hidden");
});

test("M·9: an agent's add slides into its group, marked New and added by", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await page.evaluate(
    ({ row, event }) => {
      window.appBackend!.store.milestones.push(row);
      return window.appBackend!.emit(event, { projectPath: row.projectPath });
    },
    { row: AGENT_ROW, event: CHANGED },
  );
  const added = row(page, AGENT_ROW.title);
  await expect(added).toContainText("added by General");
  await expect(added).toContainText("New");
  await expect(row(page, "Voice notes into Inbox")).toContainText(
    "added by session · Execute plan M tasks",
  );
  await expect(main(page)).toContainText("9 milestones · 2 in progress");
  await frame(page, "M9-agent-live");
});
