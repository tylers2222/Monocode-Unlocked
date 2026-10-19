const MONTHS =
  "January February March April May June July August September October November December".split(
    " ",
  );

/**
 * "Dec 12, 2026" in English for a `YYYY-MM-DD` day, whatever the locale.
 * Read from the text, not through `Date`, so no time zone can move the day.
 */
export function formatDay(day: string, withYear: boolean): string {
  const [year, month, date] = day.split("-").map(Number);
  const label = `${MONTHS[month - 1].slice(0, 3)} ${date}`;
  return withYear ? `${label}, ${year}` : label;
}

/** The local calendar day of `date` as `YYYY-MM-DD`. */
export function dayOf(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/**
 * `YYYY-MM-DD` for "Dec 12, 2026", "December 12, 2026" or "2026-12-12",
 * trimmed and in any case, else null. A 4-digit year and a real calendar
 * day are required; a form whose day and month order is a guess, such as
 * 12/11/2026, is refused.
 */
export function parseDay(text: string): string | null {
  const value = text.trim();
  const iso = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  const named = /^([a-z]+)\s+(\d{1,2}),?\s+(\d{4})$/i.exec(value);
  let year: number, month: number, day: number;
  if (iso) [year, month, day] = iso.slice(1).map(Number);
  else if (named) {
    const name = named[1].toLowerCase();
    month =
      1 +
      MONTHS.findIndex((full) => {
        const lower = full.toLowerCase();
        return lower === name || lower.slice(0, 3) === name;
      });
    [day, year] = [Number(named[2]), Number(named[3])];
  } else return null;
  // Day 0 of the next month is the last day of this one, from the calendar alone.
  const last = new Date(Date.UTC(year, month, 0)).getUTCDate();
  if (month < 1 || month > 12 || day < 1 || day > last) return null;
  return `${year}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
}
