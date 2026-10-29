import { expect, test } from "@playwright/test";

// Frame A4b (https://doop.design/c/oRuRCkta7A?frame=EJB-jYc-Dw): an open card
// in a chat that cannot answer says why, after Open run. Defects: the reason
// is missing there, or it shows on a card that can be answered.
test("an open card says why when its chat cannot answer, and only then", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/habit-question-card.html");
  const reason = "This chat can't answer. Answer in the run.";
  const answerable = page.locator('[data-chat="answerable"]');
  const readOnly = page.locator('[data-chat="read-only"]');

  await expect(readOnly.getByText(reason)).toBeVisible();
  await expect(
    readOnly.getByRole("button", { name: "Continue" }),
  ).toBeDisabled();
  const openRun = await readOnly
    .getByRole("button", { name: "Open run" })
    .boundingBox();
  const why = await readOnly.getByText(reason).boundingBox();
  expect(why!.x).toBeGreaterThan(openRun!.x + openRun!.width);

  await expect(
    answerable.getByRole("button", { name: "Open run" }),
  ).toBeVisible();
  await expect(answerable.getByText(reason)).toHaveCount(0);

  await readOnly.screenshot({ path: testInfo.outputPath("a4b-read-only.png") });
});
