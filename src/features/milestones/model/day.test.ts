import { afterAll, describe, expect, it } from "vitest";
import { formatTarget } from "../ui/timeline";
import { dayOf, formatDay } from "./day";

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
