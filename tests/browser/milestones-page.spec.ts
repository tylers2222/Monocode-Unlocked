import type { Page } from "@playwright/test";
import type { Automation } from "../../src/features/automations/model/automations";
import type { Milestone } from "../../src/features/milestones/model/milestones";
import { expect, mount, test, type BackendReport } from "./app/fixture";
import {
  AGENT_ROW,
  CHANGED,
  EVERY_STATUS,
  ORBIT,
  REVIEW,
  seed,
} from "./milestones-world";

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date(2026, 9, 18, 10));
});

const rows = (page: Page) =>
  page.locator("[data-app-milestones] [data-milestone] .truncate:first-child");

async function openMilestones(page: Page) {
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

/** Writes the row as Rust would, then sends the event Rust sends to every window. */
async function agentAdds(page: Page, row: Milestone) {
  await page.evaluate(
    ({ row, event }) => {
      window.appBackend!.store.milestones.push(row);
      return window.appBackend!.emit(event, { projectPath: row.projectPath });
    },
    { row, event: CHANGED },
  );
}

const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

test("lane 1: Automations still opens, and the Milestones row opens its own page", async ({
  app,
  page,
}) => {
  const digest: Automation = {
    id: "a-1",
    name: "Morning digest",
    prompt: "Summarize yesterday's commits",
    harness: "claude",
    model: "sonnet",
    cwd: ORBIT,
    workspaceMode: "current",
    reuseSession: false,
    runtimeMode: "supervised",
    triggerKind: "time",
    triggerEvent: "",
    scheduleKind: "daily",
    minute: 0,
    time: "09:00",
    dayOfWeek: 1,
    missedRunGraceMinutes: 60,
    enabled: true,
    nextRunAt: Date.UTC(2030, 0, 1),
    createdAt: 1,
    updatedAt: 1,
  };
  await app.mount(seed(EVERY_STATUS, { automations: [digest] }));

  await page.getByRole("button", { name: "Automations", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Open Morning digest" }),
  ).toBeVisible();

  await openMilestones(page);
  await expect(page.getByRole("region", { name: "Automations" })).toHaveCount(
    0,
  );
  await expect(rows(page)).toHaveCount(8);
  await expect(
    page.getByRole("button", { name: "Milestones", exact: true }),
  ).toHaveClass(/bg-selection/);
  await expect(
    page.getByRole("button", { name: "Automations", exact: true }),
  ).not.toHaveClass(/bg-selection/);
  await shot(page, "regression");

  await page.getByRole("button", { name: "Automations", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Open Morning digest" }),
  ).toBeVisible();
  await expect(page.getByRole("region", { name: "Milestones" })).toHaveCount(0);
});

test("lane 2: a project with no milestones shows the empty state", async ({
  app,
  page,
}) => {
  await app.mount(seed([]));
  await openMilestones(page);
  await expect(
    page.getByRole("heading", { name: "No milestones for orbit yet" }),
  ).toBeVisible();
  await expect(
    page.getByText("No milestones yet", { exact: true }),
  ).toBeVisible();
  await expect(rows(page)).toHaveCount(0);
  await shot(page, "empty");
});

test("lane 3: a project with milestones in every status", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await expect(page.getByText("8 milestones · 2 in progress")).toBeVisible();
  await expect(rows(page)).toHaveText(
    EVERY_STATUS.slice(0, 8).map((row) => row.title),
  );
  await expect(page.locator("[data-app-milestones] aside button")).toHaveText([
    "orbit8",
    "orbit-site3",
    "orbit-review",
  ]);
  const filter = page.getByRole("textbox", { name: "Filter projects" });
  await filter.fill("site");
  await expect(page.locator("[data-app-milestones] aside button")).toHaveText([
    "orbit-site3",
  ]);
  await filter.fill("ORBIT");
  await expect(page.locator("[data-app-milestones] aside button")).toHaveText([
    "orbit8",
    "orbit-site3",
    "orbit-review",
  ]);
  await filter.fill("");
  await expect(page.locator("[data-app-milestones] aside button")).toHaveCount(
    3,
  );
  await expect(
    page.locator("[data-app-milestones] aside button span[dir=auto]"),
  ).toHaveText(["orbit", "orbit-site", "orbit-review"]);
  await expect(
    page.getByText("added by session · Execute plan M tasks"),
  ).toBeVisible();
  await shot(page, "several");
});

test("lane 4: an agent's add appears within a second, marked New, without a click", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await expect(rows(page)).toHaveCount(8);

  const started = Date.now();
  await agentAdds(page, AGENT_ROW);
  const added = page.locator(`[data-milestone="${AGENT_ROW.id}"]`);
  await expect(added).toBeVisible({ timeout: 1000 });
  expect(Date.now() - started).toBeLessThan(1000);
  await expect(added).toContainText("added by General");
  await expect(added).toContainText("New");
  await expect(added).toHaveCSS("animation-name", "approval-toast-in");
  await expect(page.getByText("9 milestones · 2 in progress")).toBeVisible();
  await expect(
    page.locator("[data-app-milestones] aside button").first(),
  ).toHaveText("orbit9");
  await shot(page, "agent-live");
});

test("lane 5: switching from project A to B shows only B's rows", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await expect(rows(page)).toHaveCount(8);

  await page
    .locator("[data-app-milestones] aside button", { hasText: "orbit-site" })
    .click();
  await expect(page.getByRole("heading", { level: 2 })).toHaveText(
    "orbit-site",
  );
  await expect(rows(page)).toHaveText([
    "Signed Windows build",
    "Linux tray icon",
    "Portable settings",
  ]);
  await shot(page, "switch");
});

test("lane 6: two windows on project A both show an agent's add", async ({
  app,
  page,
  context,
}) => {
  const data = seed(EVERY_STATUS);
  await app.mount(data);
  const second = await context.newPage();
  const reports: BackendReport[] = [];
  await second.exposeFunction("appBackendReport", (report: BackendReport) => {
    reports.push(report);
  });
  await second.clock.setFixedTime(new Date(2026, 9, 18, 10));
  await mount(second, data);

  for (const window of [page, second]) await openMilestones(window);
  // Each page holds its own copy of the store, so the row lands in each copy.
  for (const window of [page, second]) await agentAdds(window, AGENT_ROW);
  for (const window of [page, second])
    await expect(
      window.locator(`[data-milestone="${AGENT_ROW.id}"]`),
    ).toBeVisible({
      timeout: 1000,
    });
  await shot(page, "two-windows");
  await second.screenshot({
    path: test.info().outputPath("two-windows-second.png"),
  });
  expect(reports).toEqual([]);
});

test("lane 7: after a restart the page shows the same rows", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  await openMilestones(page);
  await agentAdds(page, AGENT_ROW);
  await expect(rows(page)).toHaveCount(9);
  const before = await rows(page).allTextContents();

  await app.restart();
  await openMilestones(page);
  await expect(rows(page)).toHaveText(before);
  await shot(page, "restart");
});

test("lane 8: settings hide Done here and mark the off project Off", async ({
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
  await expect(page.getByText("8 milestones · Done hidden")).toBeVisible();
  await expect(page.getByRole("region", { name: "Done" })).toHaveCount(0);
  await expect(page.locator("[data-app-milestones] [data-today]")).toHaveCount(
    0,
  );
  await expect(rows(page)).toHaveCount(6);
  await expect(
    page.locator("[data-app-milestones] aside button").last(),
  ).toHaveText("orbit-reviewOff");
  await shot(page, "off");
});

for (const mode of ["compact", "hidden"] as const) {
  test(`lane 9: the entry opens the page from the ${mode} rail`, async ({
    app,
    page,
  }) => {
    await page.addInitScript((mode) => {
      localStorage.setItem("monocode.projectRailOpen", "0");
      localStorage.setItem("monocode.collapsedProjectRailMode", mode);
    }, mode);
    await app.mount(seed(EVERY_STATUS));
    await openMilestones(page);
    await expect(rows(page)).toHaveCount(8);
    await shot(page, `rail-${mode}`);
  });
}

test("lane 10: Back leaves the page for the surface it covered", async ({
  app,
  page,
}) => {
  await app.mount(seed(EVERY_STATUS));
  const covered = page.getByRole("heading", {
    name: "What should we work on in orbit?",
  });
  await expect(covered).toBeVisible();

  await openMilestones(page);
  await expect(covered).toBeHidden();
  await page.getByRole("button", { name: /^Back/ }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Milestones", exact: true }),
  ).not.toHaveClass(/bg-selection/);
  await expect(covered).toBeVisible();
  await shot(page, "back");
});
