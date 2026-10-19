import type { Page } from "@playwright/test";
import type { Automation } from "../../src/features/automations/model/automations";
import type { Milestone } from "../../src/features/milestones/model/milestones";
import { expect, test } from "./app/fixture";

// Browser-harness timings, not the packaged app: run with MILESTONES_PERF=1.
test.skip(!process.env.MILESTONES_PERF, "perf probes run on demand");

const PROJECT = "/Users/test/orbit";
const STATUSES = ["done", "in_progress", "planned", "idea"] as const;

const milestones: Milestone[] = Array.from({ length: 200 }, (_, order) => ({
  id: `m-${order}`.padEnd(36, "0"),
  projectPath: PROJECT,
  title: `Milestone ${order}`,
  note: "A one-line note about where this goes.",
  status: STATUSES[order % 4],
  target: order % 3 ? "2026-11-07" : null,
  order,
  createdBy: "user",
  createdAt: order,
  updatedAt: order,
}));

const digest: Automation = {
  id: "a-1",
  name: "Morning digest",
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
};

/** Clicks a rail action and waits until `count` matches of `ready` are painted. */
function timeOpen(page: Page, label: string, ready: string, count: number) {
  return page.evaluate(
    async ({ label, ready, count }) => {
      const button = [...document.querySelectorAll("nav button")].find(
        (candidate) => candidate.getAttribute("aria-label") === label,
      ) as HTMLElement;
      const start = performance.now();
      button.click();
      await new Promise<void>((resolve) => {
        const tick = () =>
          document.querySelectorAll(ready).length >= count
            ? resolve()
            : requestAnimationFrame(tick);
        tick();
      });
      await new Promise(requestAnimationFrame);
      return performance.now() - start;
    },
    { label, ready, count },
  );
}

async function boot(
  app: { mount: (seed: object) => Promise<void> },
  page: Page,
) {
  await app.mount({ projects: [PROJECT], milestones, automations: [digest] });
  // Let the idle preloads land, as they do before a user's first click.
  await page.waitForTimeout(1500);
}

test("perf: Automations opens", async ({ app, page }) => {
  await boot(app, page);
  const ms = await timeOpen(
    page,
    "Automations",
    '[aria-label="Open Morning digest"]',
    1,
  );
  console.log(`PERF automations-open ${ms.toFixed(1)}`);
});

test("perf: Milestones opens with 200 rows painted", async ({ app, page }) => {
  await boot(app, page);
  const ms = await timeOpen(page, "Milestones", "[data-milestone]", 200);
  console.log(`PERF milestones-open ${ms.toFixed(1)}`);
  expect(ms).toBeLessThan(300);
});

test("perf: an agent's add is visible", async ({ app, page }) => {
  await boot(app, page);
  await timeOpen(page, "Milestones", "[data-milestone]", 200);
  const ms = await page.evaluate(async () => {
    const row = {
      ...window.appBackend!.store.milestones[0],
      id: "agent-row".padEnd(36, "0"),
      title: "Added by an agent",
      order: 200,
      createdBy: "mono:general",
    };
    const start = performance.now();
    window.appBackend!.store.milestones.push(row);
    await window.appBackend!.emit("monocode:milestones-changed", {
      projectPath: row.projectPath,
    });
    await new Promise<void>((resolve) => {
      const tick = () =>
        document.querySelector(`[data-milestone="${row.id}"]`)
          ? resolve()
          : requestAnimationFrame(tick);
      tick();
    });
    await new Promise(requestAnimationFrame);
    return performance.now() - start;
  });
  console.log(`PERF agent-add ${ms.toFixed(1)}`);
  expect(ms).toBeLessThan(1000);
});

// M-9a: from the Save click to the edited row painted, with 200 rows.
test("perf: a save in the editor paints the row", async ({ app, page }) => {
  await boot(app, page);
  await timeOpen(page, "Milestones", "[data-milestone]", 200);
  const runs: number[] = [];
  for (let run = 0; run < 5; run++) {
    const title = `Saved title ${run}`;
    await page.locator(`[data-milestone="${milestones[199].id}"]`).click();
    await page
      .locator("[data-milestone-editor]")
      .getByLabel("Title")
      .fill(title);
    runs.push(
      await page.evaluate(
        async ({ id, title }) => {
          const save = [
            ...document.querySelectorAll("[data-milestone-editor] button"),
          ].find((button) => button.textContent === "Save") as HTMLElement;
          const start = performance.now();
          save.click();
          const painted = Promise.withResolvers<void>();
          const tick = () =>
            document
              .querySelector(`[data-milestone="${id}"]`)
              ?.textContent?.includes(title)
              ? painted.resolve()
              : requestAnimationFrame(tick);
          tick();
          await painted.promise;
          await new Promise(requestAnimationFrame);
          return performance.now() - start;
        },
        { id: milestones[199].id, title },
      ),
    );
  }
  const median = [...runs].sort((a, b) => a - b)[2];
  console.log(
    `PERF save-to-row ${runs.map((ms) => ms.toFixed(1)).join(" ")} median ${median.toFixed(1)}`,
  );
  expect(median).toBeLessThan(200);
});
