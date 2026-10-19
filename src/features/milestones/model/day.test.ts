import { afterAll, describe, expect, it } from "vitest";
import { formatTarget } from "../ui/timeline";
import { dayOf, formatDay, parseDay } from "./day";

const pinned = process.env.TZ;
afterAll(() => {
  process.env.TZ = pinned;
});

// Node reads TZ again each time it changes, so local-time code runs in each zone.
for (const zone of ["Australia/Sydney", "America/Los_Angeles"]) {
  describe(`dates in ${zone}`, () => {
    it("the time zone under test is the one Date uses", () => {
      process.env.TZ = zone;
      expect(new Intl.DateTimeFormat().resolvedOptions().timeZone).toBe(zone);
    });

    // Tyler edge 1: a stored day shows as that day, never the day before.
    it("a stored day shows as the same day, and today is the local day", () => {
      process.env.TZ = zone;
      expect(formatTarget("2026-12-12", new Date(2026, 9, 18))).toBe("Dec 12");
      expect(formatTarget("2026-01-01", new Date(2026, 9, 18))).toBe("Jan 1");
      expect(dayOf(new Date(2026, 11, 12, 0, 30))).toBe("2026-12-12");
      expect(dayOf(new Date(2026, 11, 12, 23, 30))).toBe("2026-12-12");
    });

    // Tyler edge 3: the three accepted forms, trimmed, month names in any case.
    it.each([
      ["Dec 12, 2026", "2026-12-12"],
      ["December 12, 2026", "2026-12-12"],
      ["2026-12-12", "2026-12-12"],
      ["  dec 12, 2026  ", "2026-12-12"],
      ["DECEMBER 12, 2026", "2026-12-12"],
      ["dEc 12 2026", "2026-12-12"],
      ["Feb 29, 2028", "2028-02-29"],
      ["Jan 1, 2020", "2020-01-01"],
    ])("parseDay reads %j as %s", (text, day) => {
      process.env.TZ = zone;
      expect(parseDay(text)).toBe(day);
    });

    // Tyler edge 3: impossible days, a 13th month, short years and forms
    // whose day/month order is a guess are refused, not repaired.
    it.each([
      "Feb 30, 2026",
      "Apr 31, 2026",
      "Feb 29, 2027",
      "2026-13-01",
      "2026-00-10",
      "Dec 0, 2026",
      "Dec 12, 26",
      "26-12-12",
      "Dec 12, 20260",
      "12/11/2026",
      "11/12/2026",
      "12.11.2026",
      "Decem 12, 2026",
      "Dezember 12, 2026",
      "",
    ])("parseDay refuses %j", (text) => {
      process.env.TZ = zone;
      expect(parseDay(text)).toBeNull();
    });

    // Tyler edge 1: an entered day is the stored day, read back unchanged.
    it("an entered day round-trips through its label", () => {
      process.env.TZ = zone;
      expect(parseDay(formatDay("2026-12-12", true))).toBe("2026-12-12");
      expect(parseDay(formatDay("2028-02-29", true))).toBe("2028-02-29");
    });
  });
}

// Tyler edge 3: English whatever the locale, year only when it differs.
it("formatDay is English with the year, or without it", () => {
  expect(formatDay("2026-03-05", true)).toBe("Mar 5, 2026");
  expect(formatDay("2026-12-12", true)).toBe("Dec 12, 2026");
  expect(formatDay("2026-03-05", false)).toBe("Mar 5");
  expect(formatTarget("2027-01-09", new Date(2026, 9, 18))).toBe("Jan 9, 2027");
  expect(formatTarget("2025-01-05", new Date(2026, 9, 18))).toBe("Jan 5, 2025");
});
