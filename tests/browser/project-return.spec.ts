import { expect, test } from "@playwright/test";

test.use({ viewport: { width: 1180, height: 340 } });

const cases = [
  {
    scenario: "before",
    name: "before the fix, opening the project lands on the remembered idle session",
    landing: "a",
  },
  {
    scenario: "one",
    name: "opening a project lands on its one running session",
    landing: "b",
  },
  {
    scenario: "idle",
    name: "opening a project with only idle sessions keeps the remembered session",
    landing: "a",
  },
  {
    scenario: "two",
    name: "opening a project with two running sessions lands on the newest turn",
    landing: "c",
  },
];

for (const { scenario, name, landing } of cases) {
  test(name, async ({ page }, testInfo) => {
    await page.goto(`/tests/browser/project-return.html?s=${scenario}`);
    await expect(page.locator("[data-title-tab-id]")).toHaveCount(3);
    await expect(page.locator("[data-landing]")).toHaveAttribute(
      "data-landing",
      landing,
    );
    await page.screenshot({
      path: testInfo.outputPath(`project-return-${scenario}.png`),
    });
  });
}
