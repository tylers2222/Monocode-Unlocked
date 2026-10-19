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

// M-9b: the longest gap between painted frames while a row is dragged
// through 50 rows (one group), five drags. Browser-harness numbers.
test("perf: dragging through 50 rows has no frame gap over 100 ms", async ({
  app,
  page,
}) => {
  const fifty = milestones
    .slice(0, 50)
    .map((row) => ({ ...row, status: "planned" as const }));
  await app.mount({ projects: [PROJECT], milestones: fifty });
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.locator("[data-milestone]")).toHaveCount(50);
  const worst: number[] = [];
  for (let run = 0; run < 5; run++) {
    const first = page.locator(`[data-milestone="${fifty[0].id}"]`);
    await first.hover();
    const grip = (await first.locator("[data-grip]").boundingBox())!;
    const last = (await page
      .locator(`[data-milestone="${fifty[49].id}"]`)
      .boundingBox())!;
    await page.evaluate(() => {
      const gaps: number[] = [];
      let previous = performance.now();
      const tick = (now: number) => {
        gaps.push(now - previous);
        previous = now;
        (
          window as unknown as { dragFrames?: { gaps: number[]; id: number } }
        ).dragFrames!.id = requestAnimationFrame(tick);
      };
      (
        window as unknown as { dragFrames: { gaps: number[]; id: number } }
      ).dragFrames = {
        gaps,
        id: requestAnimationFrame(tick),
      };
    });
    await page.mouse.move(grip.x + 8, grip.y + 8);
    await page.mouse.down();
    await page.mouse.move(grip.x + 8, last.y + last.height - 4, { steps: 60 });
    // The drag really ran: the row is lifted and a drop line is drawn.
    await expect(page.locator("[data-drop-line]")).toHaveCount(1);
    const gaps = await page.evaluate(() => {
      const frames = (
        window as unknown as { dragFrames: { gaps: number[]; id: number } }
      ).dragFrames;
      cancelAnimationFrame(frames.id);
      return frames.gaps.slice(1);
    });
    await page.keyboard.press("Escape");
    await page.mouse.move(grip.x + 8, grip.y + 8);
    await page.mouse.up();
    worst.push(Math.max(...gaps));
  }
  console.log(
    `PERF drag-worst-frame ${worst.map((ms) => ms.toFixed(1)).join(" ")}`,
  );
  expect(Math.max(...worst)).toBeLessThan(100);
});

test("perf: a settings toggle repaints the timeline within 150 ms", async ({
  app,
  page,
}) => {
  await boot(app, page);
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.locator("[data-milestone]")).toHaveCount(200);
  await page.getByRole("button", { name: "Milestone settings" }).click();
  const timings: number[] = [];
  for (let run = 0; run < 5; run++) {
    for (const hide of [true, false]) {
      // From the click on the Done box to the first frame painted after the
      // Done group left (or came back to) the timeline.
      const ms = await page.evaluate((hide) => {
        const box = document.querySelector<HTMLInputElement>(
          '[role="dialog"] input[aria-label="Done"]',
        )!;
        const done = () =>
          document.querySelector('section[aria-label="Done"]') !== null;
        const start = performance.now();
        box.click();
        return new Promise<number>((resolve) => {
          const frame = () =>
            done() === hide
              ? requestAnimationFrame(frame)
              : requestAnimationFrame(() => resolve(performance.now() - start));
          requestAnimationFrame(frame);
        });
      }, hide);
      if (hide) timings.push(ms);
    }
  }
  console.log(
    `PERF settings-toggle ${timings.map((ms) => ms.toFixed(1)).join(" ")}`,
  );
  expect(Math.max(...timings)).toBeLessThan(150);
});
