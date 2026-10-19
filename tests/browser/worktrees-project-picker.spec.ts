import { expect, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

// The project picker also offers the project already chosen. Choosing it
// again must leave the Worktrees page as it was, error included (ADE-5 r3).
test("choosing the current project again keeps the Worktrees error", async ({
  app,
  page,
}) => {
  await app.mount(
    { projects: [PROJECT] },
    {
      handlers: {
        reveal_path: () => {
          throw "Finder is not available";
        },
        // worktrees.rs list: the main checkout and one more worktree.
        git_worktrees: (_store, { cwd }) => ({
          worktrees: [true, false].map((isMain) => ({
            path: isMain ? cwd : `${cwd}-worktrees/feature`,
            branch: isMain ? "main" : "feature",
            head: "0000000000000000000000000000000000000000",
            isMain,
            locked: false,
            prunable: false,
            missing: false,
            dirty: false,
            unpushed: 0,
            sessionIds: [],
          })),
          defaultRoot: `${cwd}-worktrees`,
        }),
      },
    },
  );
  await page.getByRole("button", { name: /^Settings/ }).click();
  await page.getByRole("button", { name: "Worktrees", exact: true }).click();
  const settings = page.getByRole("region", { name: "Settings" });
  await settings.getByRole("button", { name: "Reveal feature" }).click();
  const error = settings.getByText("Finder is not available");
  await expect(error).toBeVisible();
  await settings.getByRole("button", { name: /current project acme/ }).click();
  await page
    .getByRole("dialog", { name: "Project picker" })
    .getByRole("button", { name: /^acme / })
    .click();
  await expect(page.getByRole("dialog", { name: "Project picker" })).toHaveCount(0);
  await expect(error).toBeVisible();
});
