import type { Automation } from "../../src/features/automations/model/automations";
import { expect, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

const automation = (id: string, name: string): Automation => ({
  id,
  name,
  prompt: "Summarize yesterday's commits",
  harness: "claude",
  model: "sonnet",
  cwd: PROJECT,
  workspaceMode: "current",
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
});

test("the shell opens Automations from the rail and follows live changes", async ({
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
    monos: [
      {
        id: "mono-1",
        role: "mono",
        data: { name: "Pip", mascot: "ghost", projects: [PROJECT] },
      },
    ],
    automations: [automation("a-1", "Morning digest")],
  });
  await expect(page.getByText("Fix the login redirect")).toBeVisible();
  await expect(page.getByText("Pip")).toBeVisible();

  await page.getByRole("button", { name: "Automations" }).click();
  await expect(page.getByText("Morning digest")).toBeVisible();

  await page.evaluate(
    (next) => window.appBackend!.store.automations.push(next),
    automation("a-2", "Weekly release notes"),
  );
  await app.emit("monocode:automations-changed");
  await expect(page.getByText("Weekly release notes")).toBeVisible();

  await page.screenshot({
    path: test.info().outputPath("automations.png"),
    fullPage: true,
  });
});
