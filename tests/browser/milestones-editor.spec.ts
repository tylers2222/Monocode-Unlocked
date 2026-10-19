import { readFileSync, writeFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test } from "./app/fixture";

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

/** The rows of approved frame M·6. */
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
const [TRACES, , HARNESS, PROMPTFOO, VOICE] = ROWS;

async function openPage(
  app: { mount: (seed: object) => Promise<void> },
  page: Page,
) {
  await app.mount({ projects: [ORBIT], milestones: ROWS });
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
const stored = (page: Page, id: string) =>
  page.evaluate(
    (id) => window.appBackend!.store.milestones.find((m) => m.id === id),
    id,
  );
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

const newest = (page: Page) =>
  page.evaluate(() => window.appBackend!.store.milestones.at(-1)!);

test("lane 1: a milestone added on the page is stored with createdBy user", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Milestone list in the Mono overview");
  await editor(page).getByRole("button", { name: "Add", exact: true }).click();
  await expect(editor(page)).toHaveCount(0);
  const added = await newest(page);
  expect(added).toMatchObject({
    projectPath: ORBIT,
    title: "Milestone list in the Mono overview",
    status: "planned",
    target: null,
    order: ROWS.length,
    createdBy: "user",
  });
  await expect(
    group(page, "Planned").locator(`[data-milestone="${added.id}"]`),
  ).toContainText("Later");
});

test("lane 2: add with every field, as frame M·5 shows; the stored row matches", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Milestone list in the Mono overview");
  await editor(page)
    .getByLabel("Note")
    .fill("So a Mono sees its project’s next goal when it starts a turn.");
  await editor(page).getByRole("radio", { name: "In progress" }).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Dec 12, 2026");
  await expect(
    page
      .locator("[data-milestone-editor], [data-app-milestones] main h3")
      .first(),
  ).toHaveAttribute("data-milestone-editor");
  await shot(page, "add");
  await editor(page).getByLabel("Title").press("Enter");
  await expect(editor(page)).toHaveCount(0);
  const added = await newest(page);
  expect(added).toMatchObject({
    note: "So a Mono sees its project’s next goal when it starts a turn.",
    status: "in_progress",
    target: "2026-12-12",
    createdBy: "user",
  });
  await expect(
    group(page, "In progress").locator(`[data-milestone="${added.id}"]`),
  ).toContainText("Dec 12");
});

test("an empty project's Add milestone button opens the add card", async ({
  app,
  page,
}) => {
  await app.mount({ projects: [ORBIT], milestones: [] });
  await page.getByRole("button", { name: "Milestones", exact: true }).click();
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page).getByLabel("Title").fill("First goal");
  await editor(page).getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.locator("[data-milestone]")).toHaveText(/First goal/);
  await expect(page.getByText("1 milestone", { exact: true })).toBeVisible();
});

test("lane 10: keyboard only: open the add card, type, and add with ⌘↵", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.getByRole("button", { name: "Add milestone" }).focus();
  await page.keyboard.press("Enter");
  await expect(editor(page).getByLabel("Title")).toBeFocused();
  await page.keyboard.type("Keyboard milestone");
  await page.keyboard.press("Tab");
  await page.keyboard.type("Typed without a pointer");
  await page.keyboard.press("Meta+Enter");
  await expect(editor(page)).toHaveCount(0);
  expect(await newest(page)).toMatchObject({
    title: "Keyboard milestone",
    note: "Typed without a pointer",
    createdBy: "user",
  });
});

test("lane 3: edit title and note in place, as frame M·6 shows", async ({
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
  expect(await stored(page, HARNESS.id)).toMatchObject({
    title: "Giga Mono harness (change H, v2)",
    note: "Tool allowlist only.",
    target: "2026-11-21",
  });
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
  expect((await stored(page, VOICE.id))?.status).toBe("done");
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
  await expect(
    editor(page).getByRole("button", { name: "Keep" }),
  ).toBeFocused();
  await shot(page, "delete");
  await editor(page).getByRole("button", { name: "Keep" }).click();
  await expect(editor(page).getByLabel("Title")).toHaveValue(VOICE.title);
  expect(await writes(page)).toEqual([]);

  await editor(page).getByRole("button", { name: "Delete" }).click();
  await editor(page)
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(editor(page)).toHaveCount(0);
  await expect(row(page, VOICE.id)).toHaveCount(0);
  expect(await stored(page, VOICE.id)).toBeUndefined();
  await expect(group(page, "Idea").locator("[data-milestone]")).toHaveCount(1);
});

/** Presses Tab (Alt+Tab in WebKit, whose Tab skips buttons) until `name` has focus. */
async function tabTo(page: Page, browserName: string, name: string) {
  const key = browserName === "webkit" ? "Alt+Tab" : "Tab";
  for (let press = 0; press < 20; press++) {
    await page.keyboard.press(key);
    const focused = await page.evaluate(() =>
      document.activeElement?.tagName === "BUTTON"
        ? document.activeElement.textContent?.trim()
        : null,
    );
    if (focused === name) return;
  }
  throw new Error(`${key} never reached the ${name} button`);
}

test("lane 6: keyboard only: open a row, tab to Delete, confirm, and the row is gone", async ({
  app,
  page,
  browserName,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).focus();
  await page.keyboard.press("Enter");
  await tabTo(page, browserName, "Delete");
  await page.keyboard.press("Enter");
  await expect(
    editor(page).getByRole("button", { name: "Keep" }),
  ).toBeFocused();
  await tabTo(page, browserName, "Delete");
  await page.keyboard.press("Enter");
  await expect(editor(page)).toHaveCount(0);
  await expect(row(page, VOICE.id)).toHaveCount(0);
  expect(await stored(page, VOICE.id)).toBeUndefined();
});

test("lane 6: keyboard only: Escape in the confirm deletes nothing", async ({
  app,
  page,
  browserName,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).focus();
  await page.keyboard.press("Enter");
  await tabTo(page, browserName, "Delete");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Escape");
  await expect(editor(page)).toHaveCount(0);
  expect(await writes(page)).toEqual([]);
  await expect(row(page, VOICE.id)).toBeVisible();
});

test("lane 7: an agent edits the row the editor has open; the page keeps the agent's value", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  const agentEdits = (field: "title" | "note", value: string) =>
    page.evaluate(
      ({ id, event, project, field, value }) => {
        window.appBackend!.store.milestones.find((m) => m.id === id)![field] =
          value;
        return window.appBackend!.emit(event, { projectPath: project });
      },
      { id: TRACES.id, event: CHANGED, project: ORBIT, field, value },
    );
  await row(page, TRACES.id).click();
  await agentEdits("title", "Traces in Mono chats (agent)");
  await editor(page).getByLabel("Note").fill("Folded by default.");
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(row(page, TRACES.id)).toContainText(
    "Traces in Mono chats (agent)",
  );
  expect(await stored(page, TRACES.id)).toMatchObject({
    title: "Traces in Mono chats (agent)",
    note: "Folded by default.",
  });

  await row(page, TRACES.id).click();
  await agentEdits("note", "Agent note");
  await editor(page).getByRole("button", { name: "Cancel" }).click();
  await expect(row(page, TRACES.id)).toContainText("Agent note");
});

test("lane 7: an agent moves the open row to another group; the typed text stays", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).click();
  await editor(page).getByLabel("Title").fill("Voice notes (typed)");
  await editor(page).getByLabel("Note").fill("My unsaved note");
  await page.evaluate(
    ({ id, event, project }) => {
      window.appBackend!.store.milestones.find((m) => m.id === id)!.status =
        "planned";
      return window.appBackend!.emit(event, { projectPath: project });
    },
    { id: VOICE.id, event: CHANGED, project: ORBIT },
  );
  const moved = group(page, "Planned").locator("[data-milestone-editor]");
  await expect(moved.getByLabel("Title")).toHaveValue("Voice notes (typed)");
  await expect(moved.getByLabel("Note")).toHaveValue("My unsaved note");
  await moved.getByRole("button", { name: "Save" }).click();
  expect(await stored(page, VOICE.id)).toMatchObject({
    title: "Voice notes (typed)",
    note: "My unsaved note",
    status: "planned",
  });
});

test("lane 7: hiding and showing Done while a Done row is open keeps the typed text", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await page.evaluate(
    ({ id, event, project }) => {
      window.appBackend!.store.milestones.find((m) => m.id === id)!.status =
        "done";
      return window.appBackend!.emit(event, { projectPath: project });
    },
    { id: TRACES.id, event: CHANGED, project: ORBIT },
  );
  await group(page, "Done").locator(`[data-milestone="${TRACES.id}"]`).click();
  await editor(page).getByLabel("Note").fill("Typed then collapsed");
  await page.getByRole("button", { name: "Hide Done" }).click();
  await expect(editor(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Show Done" }).click();
  await expect(editor(page).getByLabel("Note")).toHaveValue(
    "Typed then collapsed",
  );
});

test("lane 9: an invalid date is refused in the editor with an inline error and no write", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, HARNESS.id).click();
  const date = editor(page).getByLabel("Target date");
  for (const bad of ["Feb 30, 2026", "12/11/2026", "Dec 12, 26"]) {
    await date.fill(bad);
    await editor(page).getByRole("button", { name: "Save" }).click();
    await expect(editor(page).getByRole("alert")).toHaveText(
      "Use a date like Dec 12, 2026 or 2026-12-12",
    );
    await expect(date).toHaveAttribute("aria-invalid", "true");
  }
  await shot(page, "bad-date");
  expect(await writes(page)).toEqual([]);
  expect((await stored(page, HARNESS.id))?.target).toBe("2026-11-21");
});

test("lane 10: keyboard only: open with Enter or Space, save with ⌘↵, Escape cancels", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, VOICE.id).focus();
  await page.keyboard.press("Space");
  await expect(editor(page).getByLabel("Title")).toBeFocused();
  await page.keyboard.type(" (unsaved)");
  await page.keyboard.press("Escape");
  await expect(editor(page)).toHaveCount(0);
  await expect(row(page, VOICE.id)).not.toContainText("(unsaved)");
  expect(await writes(page)).toEqual([]);

  await row(page, PROMPTFOO.id).focus();
  await page.keyboard.press("Enter");
  await expect(editor(page).getByLabel("Title")).toBeFocused();
  await page.keyboard.press("End");
  await page.keyboard.type(" (keyboard)");
  await page.keyboard.press("Meta+Enter");
  await expect(editor(page)).toHaveCount(0);
  expect((await stored(page, PROMPTFOO.id))?.title).toBe(
    "Promptfoo evals run in CI (keyboard)",
  );
  await shot(page, "keyboard");
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
  expect((await stored(page, HARNESS.id))?.target).toBeNull();

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

test("a past date saves and the row keeps M-7's saved order", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  await row(page, PROMPTFOO.id).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Jan 5, 2025");
  await editor(page).getByRole("button", { name: "Save" }).click();
  await expect(row(page, PROMPTFOO.id)).toContainText("Jan 5, 2025");
  expect((await stored(page, PROMPTFOO.id))?.target).toBe("2025-01-05");
  expect(
    await group(page, "Planned")
      .locator("[data-milestone]")
      .evaluateAll((items) =>
        items.map((item) => item.getAttribute("data-milestone")),
      ),
  ).toEqual([HARNESS.id, PROMPTFOO.id]);
});

for (const timezoneId of ["Australia/Sydney", "America/Los_Angeles"]) {
  test.describe(`in ${timezoneId}`, () => {
    test.use({ timezoneId });

    test("an entered date saves and shows as that day, not the day before", async ({
      app,
      page,
    }) => {
      await page.clock.setFixedTime(new Date("2026-10-18T14:30:00Z"));
      await openPage(app, page);
      await row(page, VOICE.id).click();
      await editor(page).getByRole("radio", { name: "Date" }).click();
      await editor(page).getByLabel("Target date").fill("December 12, 2026");
      await editor(page).getByRole("button", { name: "Save" }).click();
      expect((await stored(page, VOICE.id))?.target).toBe("2026-12-12");
      await expect(row(page, VOICE.id)).toContainText("Dec 12");
      await row(page, VOICE.id).click();
      await expect(editor(page).getByLabel("Target date")).toHaveValue(
        "Dec 12, 2026",
      );
    });
  });
}

test.describe("with locale de-DE", () => {
  test.use({ locale: "de-DE" });

  test("the date field shows English and reads English month names", async ({
    app,
    page,
  }) => {
    await openPage(app, page);
    expect(
      await page.evaluate(() => ({
        language: navigator.language,
        month: new Intl.DateTimeFormat(undefined, { month: "long" }).format(
          new Date(2026, 11, 5),
        ),
      })),
    ).toEqual({ language: "de-DE", month: "Dezember" });
    await row(page, HARNESS.id).click();
    await expect(editor(page).getByLabel("Target date")).toHaveValue(
      "Nov 21, 2026",
    );
    await editor(page).getByLabel("Target date").fill("Dezember 24, 2026");
    await editor(page).getByRole("button", { name: "Save" }).click();
    await expect(editor(page).getByLabel("Target date")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await editor(page).getByLabel("Target date").fill("december 24, 2026");
    await editor(page).getByRole("button", { name: "Save" }).click();
    await expect(row(page, HARNESS.id)).toContainText("Dec 24");
  });
});

// Real-state proof: milestones.rs replays this capture through the real
// add and update commands on a real SQLite store. RECORD_MILESTONE_WRITES=1
// rewrites it.
test("the page's writes match the committed fixture that Rust replays", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  const save = () => editor(page).getByRole("button", { name: "Save" }).click();
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Milestone list in the Mono overview");
  await editor(page).getByLabel("Note").fill("Shown when a Mono starts a turn");
  await editor(page).getByRole("radio", { name: "In progress" }).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Dec 12, 2026");
  await editor(page).getByRole("button", { name: "Add", exact: true }).click();
  await page.getByRole("button", { name: "Add milestone" }).click();
  await editor(page).getByLabel("Title").fill("Someday");
  await editor(page).getByLabel("Title").press("Enter");
  await expect(editor(page)).toHaveCount(0);
  const added = page.getByText("Milestone list in the Mono overview");
  await added.click();
  await editor(page).getByRole("radio", { name: "Later" }).click();
  await save();
  await row(page, HARNESS.id).click();
  await editor(page)
    .getByLabel("Title")
    .fill("Giga Mono harness (change H, v2)");
  await editor(page).getByLabel("Note").fill("Tool allowlist only.");
  await save();
  await row(page, HARNESS.id).click();
  await editor(page).getByRole("radio", { name: "Later" }).click();
  await save();
  await row(page, HARNESS.id).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill("Feb 29, 2028");
  await editor(page).getByRole("radio", { name: "Done" }).click();
  await save();
  await row(page, VOICE.id).click();
  await editor(page).getByRole("radio", { name: "Date" }).click();
  await editor(page).getByLabel("Target date").fill(" 2025-01-05 ");
  await save();
  await expect(row(page, VOICE.id)).toContainText("Jan 5, 2025");
  const someday = page.getByText("Someday", { exact: true });
  await someday.click();
  await editor(page).getByRole("button", { name: "Delete" }).click();
  await editor(page)
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(someday).toHaveCount(0);

  const captured = (await writes(page)).map(({ command, args, reply }) =>
    command === "milestones_add"
      ? { command, args, reply: { id: (reply as Milestone).id } }
      : { command, args },
  );
  if (process.env.RECORD_MILESTONE_WRITES)
    writeFileSync(FIXTURE, `${JSON.stringify(captured, null, 2)}\n`);
  expect(captured).toEqual(JSON.parse(readFileSync(FIXTURE, "utf8")));
});
