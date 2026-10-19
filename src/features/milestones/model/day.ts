const MONTHS = "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split(" ");

/**
 * "Dec 12, 2026" in English for a `YYYY-MM-DD` day, whatever the locale.
 * Read from the text, not through `Date`, so no time zone can move the day.
 */
export function formatDay(day: string, withYear: boolean): string {
  const [year, month, date] = day.split("-").map(Number);
  const label = `${MONTHS[month - 1]} ${date}`;
  return withYear ? `${label}, ${year}` : label;
}

/** The local calendar day of `date` as `YYYY-MM-DD`. */
export function dayOf(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
