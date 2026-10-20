import type { Page } from "@playwright/test";
import { expect } from "@playwright/test";
import type { FakeSeed, Handler } from "./backend";

const TITLE = "Plan the release";

/** A session in `project`: sending from it is where the app notices a renamed folder. */
export function sessionIn(project: string): NonNullable<FakeSeed["sessions"]> {
  return [
    {
      id: "s-rename",
      cwd: project,
      harness: "claude",
      model: "sonnet",
      runtimeMode: "supervised",
      title: TITLE,
      createdAt: 1,
      updatedAt: 1,
    },
  ];
}

/** project_location.rs: where a folder is now, after `renameFolder` moved it. */
export const followRenamedFolder: Record<string, Handler> = {
  resolve_project_location: (_, { path }) => ({
    path:
      (window as { renamedFolder?: Record<string, string> }).renamedFolder?.[
        path as string
      ] ?? path,
    identity: "project-folder",
  }),
};

/**
 * Renames the folder on disk, then sends from its session: the app finds the
 * folder at `to` and moves the project there (session_rebase_project).
 */
export async function renameFolder(page: Page, from: string, to: string) {
  await page.evaluate(
    ([from, to]) => Object.assign(window, { renamedFolder: { [from]: to } }),
    [from, to],
  );
  // From a page such as Milestones, the project's row opens its sessions.
  const session = page.getByText(TITLE);
  if (!(await session.isVisible()))
    await page
      .locator(`button[aria-label="${from.split("/").pop()}"]`)
      .first()
      .click();
  await session.click();
  const field = page.getByPlaceholder("Ask, build, / for commands");
  await field.fill("Ship it");
  await field.press("Enter");
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.appBackend!.calls.some(
          ({ command }) => command === "session_rebase_project",
        ),
      ),
    )
    .toBe(true);
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
}
