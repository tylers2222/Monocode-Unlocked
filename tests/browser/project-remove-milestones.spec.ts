import type { Page } from "@playwright/test";
import { expect, test } from "./app/fixture";

const KEEP = "/Users/test/keep";
const GONE = "/Users/test/gone";

const called = (page: Page, command: string) =>
  page.evaluate(
    (name) =>
      window.appBackend!.calls
        .filter((call) => call.command === name)
        .map(({ command, args }) => ({ command, args })),
    command,
  );

const menu = async (page: Page, name: string, item: string) => {
  await page
    .getByRole("button", { name, exact: true })
    .first()
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: item }).click();
};

const confirmDelete = (page: Page, name: string) =>
  page
    .getByRole("dialog", { name: `Delete ${name}` })
    .getByRole("button", { name: /^Delete( from .+)?$/ })
    .click();

test("only Delete reaches the scoped delete; Archive keeps the data", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [KEEP, GONE] },
    {
      handlers: {
        remove_project_logo: () => null,
        remove_project_chat_background: () => null,
      },
    },
  );

  await menu(page, "keep", "Archive");
  await expect(
    page.getByRole("button", { name: "keep", exact: true }),
  ).toHaveCount(0);
  expect(await called(page, "spaces_project_delete")).toEqual([]);

  await menu(page, "gone", "Delete");
  await confirmDelete(page, "gone");
  await expect
    .poll(() => called(page, "spaces_project_delete"))
    .toEqual([
      { command: "spaces_project_delete", args: { projectPath: GONE } },
    ]);
  // The backend deletes milestones inside T11's transaction.
  expect(await called(page, "milestones_remove_project")).toEqual([]);
  await expect(
    page.getByRole("button", { name: "gone", exact: true }),
  ).toHaveCount(0);
});

test("a shared project's scoped delete keeps its row data; the old purge stays out of reach", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [KEEP, GONE] },
    {
      handlers: {
        spaces_project_delete_preview: () => ({
          lastSpace: false,
          sessionCount: 0,
          otherSpaces: ["space-2"],
          hiddenSessionCount: 0,
        }),
        spaces_project_delete: () => ({
          deletedSessionIds: [],
          lastSpace: false,
        }),
      },
    },
  );

  await menu(page, "gone", "Delete");
  await expect(
    page.getByRole("dialog", { name: "Delete gone" }),
  ).toContainText(
    "Space 1's conversations for this project will be deleted, and it leaves Space 1's sidebar.",
  );
  await confirmDelete(page, "gone");
  await expect
    .poll(() => called(page, "spaces_project_delete"))
    .toHaveLength(1);
  expect(await called(page, "session_delete")).toEqual([]);
  expect(await called(page, "remove_project_logo")).toEqual([]);
  const recents = await page.evaluate(() =>
    localStorage.getItem("monocode.recentProjects"),
  );
  expect(recents).toContain(GONE);
});

test("a refusal shows in the menu's footer and the dialog does not open", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [KEEP, GONE] },
    {
      handlers: {
        spaces_project_delete_preview: () => {
          throw {
            code: "monoHeld",
            message: "Vibtra Orchestrator uses gone. Take it off that Mono first.",
          };
        },
      },
    },
  );

  await menu(page, "gone", "Delete");
  await expect(page.getByRole("alert")).toHaveText(
    "Vibtra Orchestrator uses gone. Take it off that Mono first.",
  );
  await expect(page.getByRole("dialog", { name: "Delete gone" })).toHaveCount(0);
  expect(await called(page, "spaces_project_delete")).toEqual([]);
});

test("a refusal after confirm closes the dialog and reopens the menu with it", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [KEEP, GONE] },
    {
      handlers: {
        spaces_project_delete: () => {
          throw {
            code: "sessionRunning",
            message: "A session is running in gone here. Stop it first.",
          };
        },
      },
    },
  );

  await menu(page, "gone", "Delete");
  await confirmDelete(page, "gone");
  await expect(page.getByRole("dialog", { name: "Delete gone" })).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveText(
    "A session is running in gone here. Stop it first.",
  );
  await expect(
    page.getByRole("button", { name: "gone", exact: true }).first(),
  ).toBeVisible();
});
