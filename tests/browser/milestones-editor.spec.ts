import { readFileSync, writeFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import { expect, test } from "./app/fixture";

const ORBIT = "/Users/test/orbit";
const CHANGED = "monocode:milestones-changed";
/** The page's writes, replayed by milestones.rs `page_edits_replay_on_the_real_store`. */
const FIXTURE = new URL(
  "./fixtures/milestones-page-edits.json",
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
    window.appBackend!.calls.filter(
      ({ command }) => command === "milestones_update",
    ),
  );
const shot = (page: Page, name: string) =>
  page.screenshot({ path: test.info().outputPath(`${name}.png`) });

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
// update command on a real SQLite store. RECORD_MILESTONE_EDITS=1 rewrites it.
test("the page's edits match the committed fixture that Rust replays", async ({
  app,
  page,
}) => {
  await openPage(app, page);
  const save = () => editor(page).getByRole("button", { name: "Save" }).click();
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

  const captured = (await writes(page)).map(({ command, args }) => ({
    command,
    args,
  }));
  if (process.env.RECORD_MILESTONE_EDITS)
    writeFileSync(FIXTURE, `${JSON.stringify(captured, null, 2)}\n`);
  expect(captured).toEqual(JSON.parse(readFileSync(FIXTURE, "utf8")));
});
