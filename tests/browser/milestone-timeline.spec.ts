import { expect, test } from "@playwright/test";

test("milestone timeline renders frame M·4 in order and tabs through its rows", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/milestone-timeline.html");
  await expect(page.locator("h3")).toHaveText([
    "Done2",
    "In progress2",
    "Planned2",
    "Idea2",
  ]);
  await page.screenshot({ path: testInfo.outputPath("m4.png") });

  const rows = page.locator("[data-milestone]");
  await page.keyboard.press("Tab");
  for (let index = 0; index < (await rows.count()); index++) {
    await expect(rows.nth(index)).toBeFocused();
    await page.keyboard.press("Tab");
  }
});

test("an agent-added row shows the New label and its added-by mark", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/milestone-timeline.html?live");
  const fresh = page.locator('[data-milestone="new"]');
  await expect(fresh).toContainText("added by General");
  await expect(fresh).toContainText("New");
  await expect(page.locator('[data-milestone="m6"]')).toContainText(
    "added by session · Execute plan M tasks",
  );
  await page.screenshot({ path: testInfo.outputPath("m9.png") });
});
