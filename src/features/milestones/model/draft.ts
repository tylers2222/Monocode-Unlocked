import { formatDay, parseDay } from "./day";
import type { Milestone, MilestonePatch, MilestoneStatus } from "./milestones";

/** What the editor holds: the date text stays as typed until it is saved. */
export type Draft = {
  title: string;
  note: string;
  status: MilestoneStatus;
  target: { kind: "later" } | { kind: "date"; text: string };
};

/** A row's draft, or a new milestone's: Planned and Later, as frame M·5 opens. */
export function draftOf(milestone: Milestone | null): Draft {
  return {
    title: milestone?.title ?? "",
    note: milestone?.note ?? "",
    status: milestone?.status ?? "planned",
    target: milestone?.target
      ? { kind: "date", text: formatDay(milestone.target, true) }
      : { kind: "later" },
  };
}

/** A saveable milestone; `target` is null for Later, as `Milestone` has it. */
type Checked = Pick<Milestone, "title" | "note" | "status" | "target">;

type DraftErrors = { title?: string; date?: string };

export function checkDraft(
  draft: Draft,
): { ok: true; value: Checked } | { ok: false; errors: DraftErrors } {
  const title = draft.title.trim();
  const target =
    draft.target.kind === "later" ? null : parseDay(draft.target.text);
  const badDate = draft.target.kind === "date" && target === null;
  if (!badDate && title)
    return {
      ok: true,
      value: { title, note: draft.note.trim(), status: draft.status, target },
    };
  return {
    ok: false,
    errors: {
      ...(title ? {} : { title: "Add a title" }),
      ...(badDate
        ? { date: "Use a date like Dec 12, 2026 or 2026-12-12" }
        : {}),
    },
  };
}

/**
 * Only what the user changed since the editor opened, so a field an agent
 * changed meanwhile keeps the agent's value.
 */
export function patchOf(opened: Milestone, value: Checked): MilestonePatch {
  const before = checkDraft(draftOf(opened));
  const was = before.ok ? before.value : undefined;
  return Object.fromEntries(
    (["title", "note", "status", "target"] as const)
      .filter((field) => was?.[field] !== value[field])
      .map((field) => [field, value[field]]),
  );
}
