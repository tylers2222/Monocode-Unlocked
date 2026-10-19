import type {
  Milestone,
  MilestonePatch,
  MilestoneStatus,
  NewMilestone,
} from "./milestones";

const MONTHS =
  "january february march april may june july august september october november december".split(
    " ",
  );

const pad = (value: number) => String(value).padStart(2, "0");

/** Days in a month of a year, from the calendar alone (no time zone). */
function daysIn(year: number, month: number): number {
  return new Date(Date.UTC(year, month, 0)).getUTCDate();
}

/**
 * `YYYY-MM-DD` for "Dec 12, 2026", "December 12, 2026" or "2026-12-12",
 * else null. A 4-digit year and a real calendar day are required; a form
 * whose day and month order is a guess, such as 12/11/2026, is refused.
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
      MONTHS.findIndex(
        (full) => full === name || (name.length === 3 && full.startsWith(name)),
      );
    [day, year] = [Number(named[2]), Number(named[3])];
  } else return null;
  if (month < 1 || month > 12 || day < 1 || day > daysIn(year, month))
    return null;
  return `${year}-${pad(month)}-${pad(day)}`;
}

/** "Dec 12, 2026" in English for a `YYYY-MM-DD` day, whatever the locale. */
export function formatDay(day: string, withYear = true): string {
  const [year, month, date] = day.split("-").map(Number);
  const name = MONTHS[month - 1].slice(0, 3);
  const label = `${name[0].toUpperCase()}${name.slice(1)} ${date}`;
  return withYear ? `${label}, ${year}` : label;
}

/** The local calendar day of `date` as `YYYY-MM-DD`. */
export function dayOf(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** What the editor holds: the date text stays as typed until it is saved. */
export type Draft = {
  title: string;
  note: string;
  status: MilestoneStatus;
  target: { kind: "later" } | { kind: "date"; text: string };
};

export function draftOf(milestone?: Milestone): Draft {
  return {
    title: milestone?.title ?? "",
    note: milestone?.note ?? "",
    status: milestone?.status ?? "planned",
    target: milestone?.target
      ? { kind: "date", text: formatDay(milestone.target) }
      : { kind: "later" },
  };
}

/** A saveable milestone; `target` is `""` for Later. */
export type Checked = Required<NewMilestone> & { target: string };

export type DraftErrors = { title?: string; date?: string };

export function checkDraft(
  draft: Draft,
): { ok: true; value: Checked } | { ok: false; errors: DraftErrors } {
  const title = draft.title.trim();
  const target =
    draft.target.kind === "later" ? "" : parseDay(draft.target.text);
  const errors: DraftErrors = {
    ...(title ? {} : { title: "Add a title" }),
    ...(target === null
      ? { date: "Use a date like Dec 12, 2026 or 2026-12-12" }
      : {}),
  };
  if (target === null || !title) return { ok: false, errors };
  return {
    ok: true,
    value: { title, note: draft.note.trim(), status: draft.status, target },
  };
}

/**
 * Only what the user changed since the editor opened, so a field an agent
 * changed meanwhile keeps the agent's value.
 */
export function patchOf(opened: Draft, value: Checked): MilestonePatch {
  const before = checkDraft(opened);
  const was = before.ok ? before.value : undefined;
  return Object.fromEntries(
    (["title", "note", "status", "target"] as const)
      .filter((field) => was?.[field] !== value[field])
      .map((field) => [field, value[field]]),
  );
}
