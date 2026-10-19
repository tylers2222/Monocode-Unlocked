import { afterAll, describe, expect, it } from "vitest";
import { formatTarget } from "../ui/timeline";
import {
  checkDraft,
  dayOf,
  draftOf,
  formatDay,
  parseDay,
  patchOf,
  type Draft,
} from "./draft";
import type { Milestone } from "./milestones";

const pinned = process.env.TZ;
afterAll(() => {
  process.env.TZ = pinned;
});

const row: Milestone = {
  id: "m1",
  projectPath: "/p",
  title: "Ship it",
  note: "Soon",
  status: "planned",
  target: "2026-12-12",
  order: 0,
  createdBy: "user",
  createdAt: 0,
  updatedAt: 0,
};

const dated = (text: string): Draft => ({
  ...draftOf(),
  title: "X",
  target: { kind: "date", text },
});

// Node reads TZ again each time it changes, so local-time code runs in each zone.
for (const zone of ["Australia/Sydney", "America/Los_Angeles"]) {
  describe(`dates in ${zone}`, () => {
    it("the time zone under test is the one Date uses", () => {
      process.env.TZ = zone;
      expect(new Intl.DateTimeFormat().resolvedOptions().timeZone).toBe(zone);
    });

    // Tyler edge 3: the three accepted forms, trimmed, months in any case.
    it.each([
      ["Dec 12, 2026", "2026-12-12"],
      ["December 12, 2026", "2026-12-12"],
      ["2026-12-12", "2026-12-12"],
      ["  dec 12, 2026  ", "2026-12-12"],
      ["DECEMBER 12, 2026", "2026-12-12"],
      ["Feb 29, 2028", "2028-02-29"],
      ["Jan 1, 2020", "2020-01-01"],
    ])("parseDay accepts %j as %s", (text, day) => {
      process.env.TZ = zone;
      expect(parseDay(text)).toBe(day);
    });

    // Tyler edge 3: impossible days, a 13th month, short years and
    // day/month orders that would be a guess are refused, not repaired.
    it.each([
      "Feb 30, 2026",
      "Apr 31, 2026",
      "Feb 29, 2027",
      "2026-13-01",
      "2026-00-10",
      "Dec 12, 26",
      "26-12-12",
      "12/11/2026",
      "11/12/2026",
      "12.11.2026",
      "Decem 12, 2026",
      "Dec 0, 2026",
      "",
    ])("parseDay refuses %j", (text) => {
      process.env.TZ = zone;
      expect(parseDay(text)).toBeNull();
    });

    // Tyler edge 1: a date entered saves and shows as that day, never the
    // day before; a past date is allowed.
    it("a saved day shows as the same day and a past day is allowed", () => {
      process.env.TZ = zone;
      const checked = checkDraft(dated("Dec 12, 2026"));
      expect(checked).toMatchObject({
        ok: true,
        value: { target: "2026-12-12" },
      });
      expect(formatTarget("2026-12-12", new Date(2026, 9, 18))).toBe("Dec 12");
      expect(draftOf(row).target).toEqual({
        kind: "date",
        text: "Dec 12, 2026",
      });
      expect(checkDraft(dated("Mar 3, 2020"))).toMatchObject({
        ok: true,
        value: { target: "2020-03-03" },
      });
      expect(dayOf(new Date(2026, 11, 12, 0, 30))).toBe("2026-12-12");
    });
  });
}

// Tyler edge 3: English whatever the locale.
it("formatDay is English with the year, or without it", () => {
  expect(formatDay("2026-03-05")).toBe("Mar 5, 2026");
  expect(formatDay("2026-03-05", false)).toBe("Mar 5");
  expect(formatTarget("2027-01-09", new Date(2026, 9, 18))).toBe("Jan 9, 2027");
});

// Tyler edge 1: an empty target opens as Later; no migration.
it.each([null, ""])("a row with target %j opens as Later", (target) => {
  expect(draftOf({ ...row, target }).target).toEqual({ kind: "later" });
});

// Tyler edge 3: invalid input blocks the save; nothing partial is returned.
it("a bad date or a blank title is refused with an inline reason each", () => {
  expect(checkDraft({ ...dated("Feb 30, 2026"), title: "  " })).toEqual({
    ok: false,
    errors: {
      title: "Add a title",
      date: "Use a date like Dec 12, 2026 or 2026-12-12",
    },
  });
});

// Tyler edge 1: Later -> Date with an empty field is invalid.
it("Date with an empty field is invalid, Later saves as an empty target", () => {
  expect(checkDraft(dated(""))).toMatchObject({
    ok: false,
    errors: { date: expect.any(String) },
  });
  expect(checkDraft({ ...dated(""), target: { kind: "later" } })).toMatchObject(
    {
      ok: true,
      value: { target: "" },
    },
  );
});

// Lane 7: a field the user did not touch is not sent, so an agent's change
// to it made while the editor was open survives the save.
it("patchOf sends only the fields the user changed", () => {
  const opened = draftOf(row);
  const later = checkDraft({ ...opened, target: { kind: "later" } });
  const retitled = checkDraft({ ...opened, title: " New title " });
  if (!later.ok || !retitled.ok) throw new Error("drafts should check");
  expect(patchOf(opened, later.value)).toEqual({ target: "" });
  expect(patchOf(opened, retitled.value)).toEqual({ title: "New title" });
});
