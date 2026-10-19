import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
  ProjectMilestones,
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

/** 2026-10-19 01:30 in Sydney, 2026-10-18 07:30 in Los Angeles. */
const SPLIT_DAY = new Date("2026-10-18T14:30:00Z");
const ZONES = [
  { timezoneId: "Australia/Sydney", today: "Today · Oct 19" },
  { timezoneId: "America/Los_Angeles", today: "Today · Oct 18" },
];

/** `milestones_list` from the real store: milestones.rs list_reply_for_every_target_shape_matches_the_page_fixture. */
const REAL: ProjectMilestones = JSON.parse(
  readFileSync(
    new URL("./fixtures/milestones-real-list.json", import.meta.url),
    "utf8",
  ),
);
const REAL_LABELS: Record<string, string> = {
  "2026-10-15": "Oct 15",
  "2026-10-18": "Oct 18",
  "2026-10-19": "Oct 19",
  "2026-12-12": "Dec 12",
  "2026-01-01": "Jan 1",
  "2027-01-09": "Jan 9, 2027",
  "2025-01-05": "Jan 5, 2025",
  "2028-02-29": "Feb 29, 2028",
  "2026-12-31": "Dec 31",
};

for (const { timezoneId, today } of ZONES) {
  test.describe(`in ${timezoneId}`, () => {
    test.use({ timezoneId });

    test("a stored date shows as that day, and Today is the local day", async ({
      app,
      page,
    }) => {
      await page.clock.setFixedTime(SPLIT_DAY);
      await openPage(app, page, DATED);
      await expect(page.locator("[data-today]")).toHaveText(today);
      await expect(row(page, 0)).toContainText("Oct 15");
      await expect(row(page, 1)).toContainText("Dec 12");
      await expect(row(page, 2)).toContainText("Jan 9, 2027");
    });

    test.describe("with locale de-DE", () => {
      test.use({ locale: "de-DE" });

      test("every target shape the real store sends renders in English", async ({
        app,
        page,
      }) => {
        await page.clock.setFixedTime(SPLIT_DAY);
        await openPage(app, page, REAL.milestones);
        expect(
          await page.evaluate(() => ({
            language: navigator.language,
            month: new Intl.DateTimeFormat(undefined, { month: "long" }).format(
              new Date(2026, 2, 5),
            ),
            zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
          })),
        ).toEqual({ language: "de-DE", month: "März", zone: timezoneId });
        await expect(page.locator("[data-today]")).toHaveText(today);
        expect(REAL.milestones.map((m) => m.target)).toContain(null);
        for (const milestone of REAL.milestones) {
          const shown = page.locator(`[data-milestone="${milestone.id}"]`);
          await expect(shown.locator(".tabular-nums")).toHaveText(
            milestone.target === null ? "Later" : REAL_LABELS[milestone.target],
          );
        }
      });
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
