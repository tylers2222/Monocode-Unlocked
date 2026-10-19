import { readFileSync, writeFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test, type FakeSeed } from "./app/fixture";

const ORBIT = "/Users/test/orbit";
const CHANGED = "monocode:milestones-changed";
/** The page's writes, replayed by milestones.rs `page_writes_replay_on_the_real_store`. */
const FIXTURE = new URL(
  "./fixtures/milestones-page-writes.json",
  import.meta.url,
);

test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-10-18T12:00:00Z"));
});

function milestone(
  order: number,
  status: MilestoneStatus,
  title: string,
  fields: Partial<Milestone> = {},
): Milestone {
  return {
    id: `orbit-${order}`.padEnd(36, "0"),
    projectPath: ORBIT,
    title,
    note: "",
    status,
    target: null,
    order,
    createdBy: "user",
    createdAt: order,
    updatedAt: order,
    ...fields,
  };
}

/** The rows of approved frames M·5 to M·7. */
const ROWS = [
  milestone(0, "in_progress", "Traces in Mono chats (change G)", {
    note: "Every Mono turn gets the “Worked for 2m 14s” fold line, closed by default.",
    target: "2026-10-24",
  }),
  milestone(1, "in_progress", "Milestones page (change M)", {
    note: "This page: per-project goals on a simple timeline. Agents add with milestones.add.",
    target: "2026-10-31",
  }),
  milestone(2, "planned", "Giga Mono harness (change H)", {
    note: "A Giga has only Delegate, Do a task, Message/Reply, Check, Remember and Ask Tyler. Made with a + button; a tool allowlist on the existing harness, no new auth.",
    target: "2026-11-21",
  }),
  milestone(3, "planned", "Promptfoo evals run in CI", {
    note: "Same suite style as change E; fail the PR when a tool eval regresses.",
  }),
  milestone(4, "idea", "Voice notes into Inbox", {
    note: "Hold a key, talk, and the note lands in Inbox for a Mono to pick up.",
  }),
  milestone(5, "idea", "Share one Mono between two Macs"),
];
const [TRACES, , HARNESS, , VOICE] = ROWS;

const seed = (milestones: Milestone[] = ROWS): FakeSeed => ({
  projects: [ORBIT],
  milestones,
});

async function openPage(
  app: { mount: (seed: FakeSeed) => Promise<void> },
  page: Page,
  rows = ROWS,
) {
  await app.mount(seed(rows));
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await expect(page.getByRole("region", { name: "Milestones" })).toBeVisible();
}

const editor = (page: Page) => page.locator("[data-milestone-editor]");
const row = (page: Page, id: string) =>
  page.locator(`[data-milestone="${id}"]`);
const group = (page: Page, name: string) =>
  page
    .locator("[data-app-milestones] main")
    .getByRole("region", { name, exact: true });
const stored = (page: Page) =>
  page.evaluate(() => window.appBackend!.store.milestones);
const writes = (page: Page) =>
  page.evaluate(() =>
    window.appBackend!.calls.filter(({ command }) =>
      ["milestones_add", "milestones_update", "milestones_remove"].includes(
        command,
      ),
    ),
  );
const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

async function fillNew(
  page: Page,
  {
    title,
    note,
    status,
    date,
  }: { title: string; note?: string; status?: string; date?: string },
) {
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page).getByLabel("Title").fill(title);
  if (note) await editor(page).getByLabel("Note").fill(note);
  if (status) await editor(page).getByRole("radio", { name: status }).click();
  if (date) {
    await editor(page).getByRole("radio", { name: "Date" }).click();
    await editor(page).getByLabel("Target date").fill(date);
  }
}

test("lane 1: a milestone added on the page is stored with createdBy user, as milestones.list returns it", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await fillNew(page, { title: "Milestone list in the Mono overview" });
  await editor(page).getByRole("button", { name: "Add", exact: true }).click();
  await expect(editor(page)).toHaveCount(0);
  await expect(
    page.getByText("Milestone list in the Mono overview"),
  ).toBeVisible();
  const added = (await stored(page)).find(
    (m) => m.title === "Milestone list in the Mono overview",
  );
  expect(added).toMatchObject({
    projectPath: ORBIT,
    createdBy: "user",
    status: "planned",
    target: null,
  });
  await shot(page, "regression");
});

test("lane 2: add with every field matches the add frame and the stored row", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await fillNew(page, {
    title: "Milestone list in the Mono overview",
    note: "So a Mono sees its project’s next goal when it starts a turn.",
    status: "In progress",
    date: "Dec 12, 2026",
  });
  await shot(page, "add");
  await editor(page).getByLabel("Title").press("Enter");
  await expect(editor(page)).toHaveCount(0);
  const added = (await stored(page)).find((m) =>
    m.title.startsWith("Milestone list"),
  );
  expect(added).toMatchObject({
    note: "So a Mono sees its project’s next goal when it starts a turn.",
    status: "in_progress",
    target: "2026-12-12",
    createdBy: "user",
  });
  await expect(row(page, added!.id)).toContainText("Dec 12");
  await expect(
    group(page, "In progress").locator("[data-milestone]"),
  ).toHaveCount(3);
});

test("lane 3: edit title and note in place, as the edit frame shows", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, HARNESS.id).click();
  await expect(editor(page).getByLabel("Title")).toHaveValue(HARNESS.title);
  await expect(editor(page).getByLabel("Target date")).toHaveValue(
    "Nov 21, 2026",
  );
  await shot(page, "edit");
  await editor(page)
    .getByLabel("Title")
    .fill("Giga Mono harness (change H, v2)");
  await editor(page).getByLabel("Note").fill("Tool allowlist only.");
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(editor(page)).toHaveCount(0);
  await expect(row(page, HARNESS.id)).toContainText(
    "Giga Mono harness (change H, v2)",
  );
  await expect(row(page, HARNESS.id)).toContainText("Tool allowlist only.");
  expect(await writes(page)).toEqual([
    {
      command: "milestones_update",
      args: {
        projectPath: ORBIT,
        id: HARNESS.id,
        patch: {
          title: "Giga Mono harness (change H, v2)",
          note: "Tool allowlist only.",
        },
        by: "user",
      },
      reply: expect.objectContaining({ id: HARNESS.id }),
    },
  ]);
});

test("lane 4: a status change moves the row to its new group", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).click();
  await editor(page).getByRole("radio", { name: "Done" }).click();
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(
    group(page, "Done").locator(`[data-milestone="${VOICE.id}"]`),
  ).toBeVisible();
  await expect(group(page, "Idea").locator("[data-milestone]")).toHaveCount(1);
  expect((await stored(page)).find((m) => m.id === VOICE.id)?.status).toBe(
    "done",
  );
  await shot(page, "status");
});

test("lane 6: delete asks once inline, then the row is gone from the page and the store", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).click();
  await editor(page).getByRole("button", { name: "Delete" }).click();
  await expect(editor(page)).toContainText(
    "Delete “Voice notes into Inbox”? This can’t be undone.",
  );
  await shot(page, "delete");
  await editor(page).getByRole("button", { name: "Keep" }).click();
  await expect(editor(page).getByLabel("Title")).toHaveValue(VOICE.title);
  expect(await writes(page)).toEqual([]);

  await editor(page).getByRole("button", { name: "Delete" }).click();
  await editor(page)
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(row(page, VOICE.id)).toHaveCount(0);
  expect((await stored(page)).some((m) => m.id === VOICE.id)).toBe(false);
});

test("lane 7: an agent edits the row the editor has open; the page keeps the agent's value", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, TRACES.id).click();
  await page.evaluate(
    ({ id, event, project }) => {
      const target = window.appBackend!.store.milestones.find(
        (m) => m.id === id,
      )!;
      target.title = "Traces in Mono chats (agent)";
      return window.appBackend!.emit(event, { projectPath: project });
    },
    { id: TRACES.id, event: CHANGED, project: ORBIT },
  );
  await shot(page, "conflict");
  await editor(page).getByLabel("Note").fill("Folded by default.");
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(row(page, TRACES.id)).toContainText(
    "Traces in Mono chats (agent)",
  );
  await expect(row(page, TRACES.id)).toContainText("Folded by default.");
  expect((await stored(page)).find((m) => m.id === TRACES.id)).toMatchObject({
    title: "Traces in Mono chats (agent)",
    note: "Folded by default.",
  });

  await row(page, TRACES.id).click();
  await page.evaluate(
    ({ id, event, project }) => {
      window.appBackend!.store.milestones.find((m) => m.id === id)!.note =
        "Agent note";
      return window.appBackend!.emit(event, { projectPath: project });
    },
    { id: TRACES.id, event: CHANGED, project: ORBIT },
  );
  await editor(page).getByRole("button", { name: "Cancel" }).click();
  await expect(row(page, TRACES.id)).toContainText("Agent note");
});

test("lane 9: an invalid date is refused in the editor with an inline error and no write", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await fillNew(page, { title: "Bad date", date: "Feb 30, 2026" });
  await editor(page).getByRole("button", { name: "Add", exact: true }).click();
  await expect(editor(page).getByRole("alert")).toHaveText(
    "Use a date like Dec 12, 2026 or 2026-12-12",
  );
  await expect(editor(page).getByLabel("Target date")).toHaveAttribute(
    "aria-invalid",
    "true",
  );
  await shot(page, "bad-date");
  expect(await writes(page)).toEqual([]);
  await editor(page).getByLabel("Target date").fill("12/11/2026");
  await editor(page).getByLabel("Title").press("Enter");
  expect(await writes(page)).toEqual([]);
});

test("lane 10: keyboard only: add and save, open a row with Enter or Space, Escape cancels", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.getByRole("button", { name: "Add milestone" }).focus();
  await page.keyboard.press("Enter");
  await page.keyboard.type("Keyboard milestone");
  await page.keyboard.press("Tab");
  await page.keyboard.type("Typed without a pointer");
  await page.keyboard.press("Meta+Enter");
  await expect(editor(page)).toHaveCount(0);
  const added = (await stored(page)).find(
    (m) => m.title === "Keyboard milestone",
  );
  expect(added).toMatchObject({
    note: "Typed without a pointer",
    createdBy: "user",
  });

  await row(page, VOICE.id).focus();
  await page.keyboard.press("Space");
  await expect(editor(page).getByLabel("Title")).toBeFocused();
  await page.keyboard.type(" (unsaved)");
  await page.keyboard.press("Escape");
  await expect(editor(page)).toHaveCount(0);
  await expect(row(page, VOICE.id)).not.toContainText("(unsaved)");

  await row(page, VOICE.id).focus();
  await page.keyboard.press("Enter");
  await expect(editor(page).getByLabel("Title")).toHaveValue(VOICE.title);
  await shot(page, "keyboard");
  await page.keyboard.press("Escape");
  expect(await writes(page)).toHaveLength(1);
});

test("Date to Later clears the stored date; Later to Date with an empty field does not save", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, HARNESS.id).click();
  await editor(page).getByRole("radio", { name: "Later" }).click();
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(row(page, HARNESS.id)).toContainText("Later");
  expect(
    (await stored(page)).find((m) => m.id === HARNESS.id)?.target,
  ).toBeNull();

  await row(page, HARNESS.id).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await expect(editor(page).getByLabel("Target date")).toHaveValue("");
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(editor(page).getByLabel("Target date")).toHaveAttribute(
    "aria-invalid",
    "true",
  );
  expect(await writes(page)).toHaveLength(1);
});

test("rows with an empty or null target show Later and open as Later", async ({
  app,
  page,
}) => {
  await openPage(app, page, [
    milestone(0, "planned", "Stored as null"),
    milestone(1, "planned", "Stored as empty", { target: "" }),
  ]);
  for (const id of ["orbit-0", "orbit-1"]) {
    await expect(row(page, id.padEnd(36, "0"))).toContainText("Later");
  }
  await row(page, "orbit-1".padEnd(36, "0")).click();
  await expect(
    editor(page).getByRole("radio", { name: "Later" }),
  ).toHaveAttribute("aria-checked", "true");
});

test("a past date saves and rows keep M-7's order inside a group (by order, dated or Later)", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await fillNew(page, { title: "Already late", date: "Jan 5, 2025" });
  await editor(page).getByLabel("Title").press("Enter");
  const late = (await stored(page)).find((m) => m.title === "Already late")!;
  expect(late.target).toBe("2025-01-05");
  await expect(row(page, late.id)).toContainText("Jan 5, 2025");
  await expect(
    group(page, "Planned").locator("[data-milestone] .truncate:first-child"),
  ).toHaveText([
    "Giga Mono harness (change H)",
    "Promptfoo evals run in CI",
    "Already late",
  ]);
});

test("the Done arrow hides and shows the Done rows", async ({ app, page }) => {
  await openPage(app, page, [
    milestone(0, "done", "Shipped one", { target: "2026-10-15" }),
    milestone(1, "planned", "Next"),
  ]);
  const arrow = page.getByRole("button", { name: "Hide Done" });
  await expect(arrow).toHaveAttribute("aria-expanded", "true");
  await arrow.click();
  await expect(page.getByText("Shipped one")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Show Done" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(page.locator("[data-today]")).toBeVisible();
  await page.getByRole("button", { name: "Show Done" }).click();
  await expect(page.getByText("Shipped one")).toBeVisible();
});

for (const timezoneId of ["Australia/Sydney", "America/Los_Angeles"]) {
  test.describe(`in ${timezoneId}`, () => {
    test.use({ timezoneId });

    test("a date entered saves and shows as that day, not the day before", async ({
      app,
      page,
    }) => {
      await openPage(app, page);
      await fillNew(page, { title: "Zone check", date: "December 12, 2026" });
      await editor(page).getByLabel("Title").press("Enter");
      const added = (await stored(page)).find((m) => m.title === "Zone check")!;
      expect(added.target).toBe("2026-12-12");
      await expect(row(page, added.id)).toContainText("Dec 12");
      await row(page, added.id).click();
      await expect(editor(page).getByLabel("Target date")).toHaveValue(
        "Dec 12, 2026",
      );
    });
  });
}

test.describe("with locale de-DE", () => {
  test.use({ locale: "de-DE" });

  test("dates show in English and English month names parse", async ({
    app,
    page,
  }) => {
    await openPage(app, page);
    expect(await page.evaluate(() => navigator.language)).toBe("de-DE");
    await expect(page.getByText("Today · Oct 18")).toBeVisible();
    await expect(row(page, HARNESS.id)).toContainText("Nov 21");
    await row(page, HARNESS.id).click();
    await expect(editor(page).getByLabel("Target date")).toHaveValue(
      "Nov 21, 2026",
    );
    await editor(page).getByLabel("Target date").fill("2026-12-24");
    await editor(page).getByRole("button", { name: "Save" }).click();
    await expect(row(page, HARNESS.id)).toContainText("Dec 24");
  });
});

// Real-state proof: milestones.rs replays this capture through the real
// commands on a real SQLite store. RECORD_MILESTONE_WRITES=1 rewrites it.
test("the page's writes match the committed fixture that Rust replays", async ({
  app,
  page,
}) => {
  await openPage(app, page, []);
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Milestone list in the Mono overview");
  await editor(page).getByLabel("Note").fill("Shown when a Mono starts a turn");
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Dec 12, 2026");
  await editor(page).getByLabel("Title").press("Enter");
  await fillNew(page, { title: "Throwaway" });
  await editor(page).getByLabel("Title").press("Enter");
  await expect(page.locator("[data-milestone]")).toHaveCount(2);
  const [first, second] = await stored(page);

  await row(page, first.id).click();
  await editor(page).getByRole("radio", { name: "Later" }).click();
  await editor(page).getByRole("button", { name: "Save" }).click();
  await row(page, first.id).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("2027-01-15");
  await editor(page).getByRole("radio", { name: "Done" }).click();
  await editor(page).getByRole("button", { name: "Save" }).click();
  await row(page, second.id).click();
  await editor(page).getByRole("button", { name: "Delete" }).click();
  await editor(page)
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(page.locator("[data-milestone]")).toHaveCount(1);

  const captured = (await writes(page)).map(({ command, args, reply }) =>
    command === "milestones_add"
      ? { command, args, reply: { id: (reply as Milestone).id } }
      : { command, args },
  );
  if (process.env.RECORD_MILESTONE_WRITES)
    writeFileSync(FIXTURE, `${JSON.stringify(captured, null, 2)}\n`);
  expect(captured).toEqual(JSON.parse(readFileSync(FIXTURE, "utf8")));
});
