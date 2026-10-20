import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneSettings,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test, type AppShell, type FakeSeed } from "./app/fixture";
import {
  followRenamedFolder,
  renameFolder,
  sessionIn,
} from "./app/projectRename";

const ORBIT = "/Users/test/orbit";
const SITE = "/Users/test/orbit-site";
const RENAMED = "/Users/test/orbit-renamed";
const CHANGED = "monocode:milestones-changed";

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-10-18T12:00:00Z"));
});

/** Two rows in each status, as frame M·8 counts them. */
function rowsOf(project: string): Milestone[] {
  const statuses: MilestoneStatus[] = [
    "in_progress",
    "planned",
    "idea",
    "done",
  ];
  return statuses.flatMap((status, group) =>
    [0, 1].map((at) => ({
      id: `${project.slice(12)}-${status}-${at}`.padEnd(36, "0"),
      projectPath: project,
      title: `${status.replace("_", " ")} ${at + 1}`,
      note: "",
      status,
      target: null,
      order: group * 2 + at,
      createdBy: "user" as const,
      createdAt: 1,
      updatedAt: 1,
    })),
  );
}

async function openPage(
  app: AppShell,
  page: Page,
  seed: FakeSeed = {},
  options?: Parameters<AppShell["mount"]>[1],
) {
  await app.mount(
    {
      projects: [ORBIT, SITE],
      milestones: [...rowsOf(ORBIT), ...rowsOf(SITE)],
      ...seed,
    },
    options,
  );
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

const main = (page: Page) => page.locator("[data-app-milestones] main");
const group = (page: Page, name: string) =>
  main(page).getByRole("region", { name, exact: true });
const settingsButton = (page: Page) =>
  page.getByRole("button", { name: "Milestone settings" });
const popover = (page: Page) =>
  page.getByRole("dialog", { name: /^Milestones in / });
const onSwitch = (page: Page) =>
  popover(page).getByRole("switch", { name: "Milestones for this project" });
const statusBox = (page: Page, name: string) =>
  popover(page).getByRole("checkbox", { name, exact: true });
const projectRow = (page: Page, name: string) =>
  page
    .locator("[data-app-milestones] aside")
    .getByRole("button", { name: new RegExp(`^${name}(?: |$)`) });
const stored = (page: Page, project: string) =>
  page.evaluate(
    (project) => window.appBackend!.store.milestoneSettings[project] ?? null,
    project,
  );
const settingsWrites = (page: Page) =>
  page.evaluate(() =>
    window
      .appBackend!.calls.filter(
        ({ command }) => command === "milestone_settings_set",
      )
      .map(({ args }) => args),
  );
const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

test("lane 1: hiding Done in the settings hides the Done group of that project", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await expect(group(page, "Done")).toBeVisible();
  await settingsButton(page).click();
  await expect(popover(page)).toContainText("Milestones in orbit");
  await expect(statusBox(page, "Done")).toBeChecked();
  await statusBox(page, "Done").click();

  await expect(group(page, "Done")).toHaveCount(0);
  await expect(statusBox(page, "Done")).not.toBeChecked();
  await expect(main(page)).toContainText("8 milestones · Done hidden");
  expect(await settingsWrites(page)).toEqual([
    { projectPath: ORBIT, patch: { hiddenStatuses: ["done"] } },
  ]);
  expect(await stored(page, ORBIT)).toEqual({
    enabled: true,
    hiddenStatuses: ["done"],
  });
  await shot(page, "regression");
});

test("lane 2: Done hidden in one project still shows in another", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await settingsButton(page).click();
  await statusBox(page, "Done").click();
  await expect(group(page, "Done")).toHaveCount(0);
  await expect(popover(page).getByText("2", { exact: true })).toHaveCount(4);
  await shot(page, "settings");

  await page.keyboard.press("Escape");
  await expect(popover(page)).toHaveCount(0);
  await projectRow(page, "orbit-site").click();
  await expect(group(page, "Done")).toBeVisible();
  expect(await stored(page, SITE)).toBeNull();
  await shot(page, "isolation");

  // A write from the other project's settings lands on that project only.
  await settingsButton(page).click();
  await expect(popover(page)).toContainText("Milestones in orbit-site");
  await statusBox(page, "Idea").click();
  await expect(group(page, "Idea")).toHaveCount(0);
  expect(await stored(page, SITE)).toEqual({
    enabled: true,
    hiddenStatuses: ["idea"],
  });
  expect((await stored(page, ORBIT))?.hiddenStatuses).toEqual(["done"]);
});

test("the popover closes on a second click of its button and on a click outside it", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await settingsButton(page).click();
  await expect(popover(page)).toBeVisible();
  await expect(settingsButton(page)).toHaveAttribute("aria-expanded", "true");
  await settingsButton(page).click();
  await expect(popover(page)).toHaveCount(0);
  await expect(settingsButton(page)).toHaveAttribute("aria-expanded", "false");

  // Another project's row is outside the popover: the click closes it and
  // opens that project.
  await settingsButton(page).click();
  await expect(popover(page)).toBeVisible();
  await projectRow(page, "orbit-site").click();
  await expect(popover(page)).toHaveCount(0);
  await expect(settingsButton(page)).toHaveAttribute("aria-expanded", "false");
  await expect(main(page).locator("h2")).toHaveText("orbit-site");
});

test("lane 3: turning a project off marks it Off; the page keeps its timeline", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await settingsButton(page).click();
  await expect(onSwitch(page)).toHaveAttribute("aria-checked", "true");
  await onSwitch(page).click();

  await expect(onSwitch(page)).toHaveAttribute("aria-checked", "false");
  await expect(projectRow(page, "orbit")).toHaveText("orbitOff");
  await expect(projectRow(page, "orbit-site")).not.toContainText("Off");
  await expect(group(page, "In progress")).toBeVisible();
  expect(await stored(page, ORBIT)).toEqual({
    enabled: false,
    hiddenStatuses: [],
  });
  await shot(page, "off");
});

/** An agent's milestones.add, as the control CLI sends it to Rust. */
const agentAdd = (page: Page, project: string) =>
  page.evaluate(
    (projectPath) =>
      window.__TAURI_INTERNALS__
        .invoke("milestones_add", {
          projectPath,
          milestone: { title: "From an agent", createdBy: "mono:general" },
        })
        .then(
          () => "ok",
          (code: unknown) => String(code),
        ),
    project,
  );

test("lanes 4 and 5: off refuses an agent with milestones_off and the page stays; on again lets it add", async ({
  app,
  page,
}) => {
  await openPage(app, page, {
    milestoneSettings: { [ORBIT]: { enabled: false, hiddenStatuses: [] } },
  });
  const rows = page.locator("[data-milestone]");
  await expect(rows).toHaveCount(8);

  expect(await agentAdd(page, ORBIT)).toBe("milestones_off");
  await expect(rows).toHaveCount(8);
  await expect(page.getByText("From an agent")).toHaveCount(0);
  await shot(page, "off-agent");

  await settingsButton(page).click();
  await onSwitch(page).click();
  await expect(projectRow(page, "orbit")).toHaveText("orbit8");
  expect(await agentAdd(page, ORBIT)).toBe("ok");
  await expect(page.getByText("From an agent")).toBeVisible();
  await shot(page, "on");
});

test("lane 6: the settings hold after a restart", async ({ app, page }) => {
  await openPage(app, page);
  await settingsButton(page).click();
  await statusBox(page, "Idea").click();
  await expect(group(page, "Idea")).toHaveCount(0);
  await onSwitch(page).click();
  await expect(projectRow(page, "orbit")).toHaveText("orbitOff");

  await app.restart();
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(projectRow(page, "orbit")).toHaveText("orbitOff");
  await expect(group(page, "Idea")).toHaveCount(0);
  await expect(group(page, "Planned")).toBeVisible();
  await settingsButton(page).click();
  await expect(onSwitch(page)).toHaveAttribute("aria-checked", "false");
  await expect(statusBox(page, "Idea")).not.toBeChecked();
  await shot(page, "restart");
});

test("lane 7: after the app follows a renamed project folder, its settings follow the new path", async ({
  app,
  page,
}) => {
  // One project, so its session shows in the sidebar.
  await app.mount(
    {
      projects: [ORBIT],
      milestones: rowsOf(ORBIT),
      milestoneSettings: {
        [ORBIT]: { enabled: false, hiddenStatuses: ["done"] },
      },
      sessions: sessionIn(ORBIT),
    },
    { handlers: followRenamedFolder },
  );
  await renameFolder(page, ORBIT, RENAMED);

  await expect(projectRow(page, "orbit-renamed")).toHaveText(
    "orbit-renamedOff",
  );
  await expect(group(page, "Done")).toHaveCount(0);
  await expect(group(page, "Planned")).toBeVisible();
  expect(await stored(page, ORBIT)).toBeNull();
  expect(await stored(page, RENAMED)).toEqual({
    enabled: false,
    hiddenStatuses: ["done"],
  });
  await shot(page, "rename");
});

test("lane 8: every status hidden leaves no group on the timeline and says so", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await settingsButton(page).click();
  for (const name of ["In progress", "Planned", "Idea", "Done"])
    await statusBox(page, name).click();

  await expect(page.locator("[data-milestone]")).toHaveCount(0);
  await expect(main(page)).toContainText(
    "8 milestones · Done, In progress, Planned, Idea hidden",
  );
  await expect(main(page).getByRole("region")).toHaveCount(0);
  expect((await stored(page, ORBIT))?.hiddenStatuses).toEqual([
    "idea",
    "planned",
    "in_progress",
    "done",
  ]);
  await page.keyboard.press("Escape");
  await shot(page, "all-hidden");
});

test("lane 9: a setting changed in another window shows here", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await settingsButton(page).click();
  await expect(statusBox(page, "Planned")).toBeChecked();
  // The other window's write: Rust stores it and tells every window.
  await page.evaluate(
    ({ project, settings }) => {
      window.appBackend!.store.milestoneSettings[project] = settings;
    },
    {
      project: ORBIT,
      settings: {
        enabled: false,
        hiddenStatuses: ["planned"],
      } satisfies MilestoneSettings,
    },
  );
  await app.emit(CHANGED, { projectPath: ORBIT });

  await expect(group(page, "Planned")).toHaveCount(0);
  await expect(statusBox(page, "Planned")).not.toBeChecked();
  await expect(onSwitch(page)).toHaveAttribute("aria-checked", "false");
  await expect(projectRow(page, "orbit")).toHaveText("orbitOff");
  await shot(page, "two-windows");
});

/** Tab, or Alt+Tab in WebKit, whose Tab skips buttons and checkboxes. */
async function tabTo(page: Page, browserName: string, name: string) {
  const key = browserName === "webkit" ? "Alt+Tab" : "Tab";
  for (let press = 0; press < 30; press++) {
    await page.keyboard.press(key);
    const focused = await page.evaluate(
      () => document.activeElement?.getAttribute("aria-label") ?? null,
    );
    if (focused === name) return;
  }
  throw new Error(`${key} never reached ${name}`);
}

test("lane 10: keyboard only: every setting is reachable and works", async ({
  app,
  page,
  browserName,
}) => {
  await openPage(app, page);
  await page.getByPlaceholder("Filter projects").focus();
  await tabTo(page, browserName, "Milestone settings");
  await page.keyboard.press("Enter");
  await expect(popover(page)).toBeFocused();

  await tabTo(page, browserName, "Milestones for this project");
  await page.keyboard.press("Space");
  await expect(onSwitch(page)).toHaveAttribute("aria-checked", "false");
  for (const name of ["In progress", "Planned", "Idea", "Done"]) {
    await tabTo(page, browserName, name);
    await page.keyboard.press("Space");
    await expect(statusBox(page, name)).not.toBeChecked();
  }
  await page.keyboard.press("Space");
  await expect(statusBox(page, "Done")).toBeChecked();
  await shot(page, "keyboard");

  await page.keyboard.press("Escape");
  await expect(popover(page)).toHaveCount(0);
  await expect(settingsButton(page)).toBeFocused();
  expect(await stored(page, ORBIT)).toEqual({
    enabled: false,
    hiddenStatuses: ["idea", "planned", "in_progress"],
  });
});

test("a refused write shows Rust's answer in the page's notice and changes nothing", async ({
  app,
  page,
}) => {
  await openPage(
    app,
    page,
    {},
    {
      handlers: {
        milestone_settings_set: () => {
          throw "database is locked";
        },
      },
    },
  );
  await settingsButton(page).click();
  await statusBox(page, "Done").click();
  await expect(main(page).getByText("database is locked")).toBeVisible();
  await expect(group(page, "Done")).toBeVisible();
  await expect(statusBox(page, "Done")).toBeChecked();
});
