import type { Automation } from "../../src/features/automations/model/automations";
import { expect, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

// The project picker also offers the project already chosen. Choosing it
// again must not clear the automation's Session folder (ADE-5 review r2).

const automation: Automation = {
  id: "a-1",
  name: "Morning digest",
  prompt: "Summarize yesterday's commits",
  harness: "claude",
  model: "sonnet",
  cwd: PROJECT,
  workspaceMode: "current",
  sessionFolderId: "folder-x",
  reuseSession: false,
  runtimeMode: "supervised",
  triggerKind: "time",
  triggerEvent: "",
  scheduleKind: "daily",
  minute: 0,
  time: "09:00",
  dayOfWeek: 1,
  missedRunGraceMinutes: 60,
  enabled: true,
  nextRunAt: Date.UTC(2030, 0, 1),
  createdAt: 1,
  updatedAt: 1,
};

test("choosing the automation's own project keeps its session folder and a clean editor", async ({
  app,
  page,
}) => {
  await app.mount({ projects: [PROJECT], automations: [automation] });
  await page.getByRole("button", { name: "Automations" }).click();
  await page.getByText("Morning digest").click();
  const folder = page.getByText("Removed folder");
  await expect(folder).toBeVisible();
  const editor = page.locator("form");
  await expect(editor.getByRole("button", { name: "Save" })).toBeDisabled();
  await expect(editor.getByRole("button", { name: "Reset" })).toHaveCount(0);
  await page
    .getByRole("button", { name: /current project acme/ })
    .last()
    .click();
  await page
    .getByRole("dialog", { name: "Project picker" })
    .getByRole("button", { name: /^acme / })
    .click();
  await expect(page.getByRole("dialog", { name: "Project picker" })).toHaveCount(0);
  await expect(folder).toBeVisible();
  await expect(editor.getByRole("button", { name: "Save" })).toBeDisabled();
  await expect(editor.getByRole("button", { name: "Reset" })).toHaveCount(0);
});
