import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test } from "./app/fixture";

const ORBIT = "/Users/test/orbit";

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

async function openPage(
  app: { mount: (seed: object) => Promise<void> },
  page: Page,
  milestones: Milestone[],
) {
  await app.mount({ projects: [ORBIT], milestones });
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

const row = (page: Page, order: number) =>
  page.locator(`[data-milestone="${`orbit-${order}`.padEnd(36, "0")}"]`);

const DATED = [
  milestone(0, "done", "Shipped", { target: "2026-10-15" }),
  milestone(1, "planned", "This year", { target: "2026-12-12" }),
  milestone(2, "planned", "Next year", { target: "2027-01-09" }),
];

test("rows with an empty or null target show Later", async ({ app, page }) => {
  await openPage(app, page, [
    milestone(0, "planned", "Stored as null"),
    milestone(1, "planned", "Stored as empty", { target: "" }),
  ]);
  await expect(row(page, 0)).toContainText("Later");
  await expect(row(page, 1)).toContainText("Later");
});

test("rows keep M-7's saved order inside a group, dated or Later", async ({
  app,
  page,
}) => {
  await openPage(app, page, [
    milestone(0, "planned", "Later first"),
    milestone(1, "planned", "Dated second", { target: "2026-11-21" }),
    milestone(2, "planned", "Past third", { target: "2025-01-05" }),
  ]);
  await expect(
    page.locator("[data-milestone] .truncate:first-child"),
  ).toHaveText(["Later first", "Dated second", "Past third"]);
  await expect(row(page, 2)).toContainText("Jan 5, 2025");
});

test("the Done arrow hides and shows the Done rows", async ({ app, page }) => {
  await openPage(app, page, [
    milestone(0, "done", "Shipped one", { target: "2026-10-15" }),
    milestone(1, "planned", "Next"),
  ]);
  const arrow = page.getByRole("button", { name: "Hide Done" });
  await expect(arrow).toHaveAttribute("aria-expanded", "true");
  await arrow.click();
  await expect(page.getByText("Shipped one")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Show Done" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(page.locator("[data-today]")).toBeVisible();
  await expect(page.getByText("Next", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Show Done" }).click();
  await expect(page.getByText("Shipped one")).toBeVisible();
});

for (const timezoneId of ["Australia/Sydney", "America/Los_Angeles"]) {
  test.describe(`in ${timezoneId}`, () => {
    test.use({ timezoneId });

    test("a stored date shows as that day, not the day before", async ({
      app,
      page,
    }) => {
      await openPage(app, page, DATED);
      await expect(page.getByText("Today · Oct 18")).toBeVisible();
      await expect(row(page, 0)).toContainText("Oct 15");
      await expect(row(page, 1)).toContainText("Dec 12");
      await expect(row(page, 2)).toContainText("Jan 9, 2027");
    });
  });
}

test.describe("with locale de-DE", () => {
  test.use({ locale: "de-DE" });

  test("dates show in English", async ({ app, page }) => {
    await openPage(app, page, DATED);
    expect(await page.evaluate(() => navigator.language)).toBe("de-DE");
    await expect(page.getByText("Today · Oct 18")).toBeVisible();
    await expect(row(page, 1)).toContainText("Dec 12");
    await expect(row(page, 2)).toContainText("Jan 9, 2027");
    await page.screenshot({ path: test.info().outputPath("de-DE.png") });
  });
});
