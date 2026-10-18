import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";

const KEEP = "/Users/test/keep";
const GONE = "/Users/test/gone";

const removals = (page: Page) =>
  page.evaluate(() =>
    window.appBackend!.calls.filter(
      (call) => call.command === "milestones_remove_project",
    ),
  );

test("only Delete removes a project's milestones; Archive keeps them", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [KEEP, GONE] },
    {
      handlers: {
        milestones_remove_project: () => null,
        remove_project_logo: () => null,
        remove_project_chat_background: () => null,
      },
    },
  );

  const menu = async (name: string, item: string) => {
    await page
      .getByRole("button", { name, exact: true })
      .first()
      .click({ button: "right" });
    await page.getByRole("menuitem", { name: item }).click();
  };

  await menu("keep", "Archive");
  await expect(
    page.getByRole("button", { name: "keep", exact: true }),
  ).toHaveCount(0);
  expect(await removals(page)).toEqual([]);

  await menu("gone", "Delete");
  await page
    .getByRole("dialog", { name: "Delete gone" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect
    .poll(() => removals(page))
    .toEqual([
      { command: "milestones_remove_project", args: { projectPath: GONE } },
    ]);
});
