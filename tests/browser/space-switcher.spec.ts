import { expect, test } from "@playwright/test";

// ADE-179 on ADE-170 frames 1 and 2's data, with the other Space's news.
test("the switcher shows news from the other Space, closed and open", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/space-switcher.html");
  const trigger = page.getByRole("button", { name: "Space: Space 1" });
  await expect(trigger).toContainText("tyler.main@example.com");
  await expect(trigger).toContainText("tyler.dev@example.com");
  await expect(page.locator("[data-space-news]")).toHaveAttribute(
    "title",
    "Space 2: 2 new replies",
  );
  await page.screenshot({ path: testInfo.outputPath("frame1-closed-badge.png") });

  await trigger.click();
  const space2 = page.getByRole("menuitemradio", { name: /Space 2/ });
  await expect(space2).toContainText("1 working");
  await expect(space2.locator("[data-space-unread]")).toHaveText("2");
  await expect(space2).toContainText("No Codex login");
  await page.screenshot({ path: testInfo.outputPath("frame2-open-badge.png") });

  // Frame 13: each Space's colour as an 8 px dot, Space 1 blue and Space 2 violet.
  const dot = (scope: typeof trigger) => scope.locator("[data-space-dot]");
  for (const [scope, rgb] of [
    [trigger, "rgb(69, 155, 247)"],
    [page.getByRole("menuitemradio", { name: /Space 1/ }), "rgb(69, 155, 247)"],
    [space2, "rgb(176, 105, 211)"],
  ] as const) {
    await expect(dot(scope)).toHaveCSS("background-color", rgb);
    await expect(dot(scope)).toHaveCSS("width", "8px");
  }

  await page.getByRole("menuitem", { name: "Manage Spaces" }).click();
  await expect(page.locator("body")).toHaveAttribute("data-settings", "");
});
