import { expect, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

// The Access picker is one of the app's `autoFocus` popovers: its list has to
// hold focus for its ArrowUp/ArrowDown/Enter keys to work.
test("an autoFocus popover takes focus when it opens: the Access picker's keys work", async ({
  app,
  page,
}) => {
  await app.mount({
    projects: [PROJECT],
    sessions: [
      {
        id: "s-1",
        cwd: PROJECT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Fix the login redirect",
        createdAt: 1,
        updatedAt: 1,
      },
    ],
  });
  await page.getByText("Fix the login redirect").click();
  const trigger = page.locator("[data-access-picker-trigger]");
  await expect(trigger).toHaveAccessibleName("Supervised");
  await trigger.click();

  const list = page.getByRole("listbox", { name: "Access" });
  await expect(list).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(list).toHaveCount(0);
  await expect(trigger).toHaveAccessibleName("Auto-accept edits");
});
