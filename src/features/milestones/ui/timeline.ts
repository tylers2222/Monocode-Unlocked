/** The row `milestones_list` returns (Rust `Milestone`, camelCase). */
export type MilestoneStatus = "idea" | "planned" | "in_progress" | "done";

export type Milestone = {
  id: string;
  title: string;
  note: string;
  status: MilestoneStatus;
  /** `YYYY-MM-DD`, or null for "Later". */
  target: string | null;
  order: number;
  /** `user`, `mono:<monoId>` or `session:<sessionId>`. */
  createdBy: string;
};

/** Past at the top, future below; the Today line sits after `done`. */
export const TIMELINE_STATUSES: readonly MilestoneStatus[] = [
  "done",
  "in_progress",
  "planned",
  "idea",
];

export const STATUS_LABELS: Record<MilestoneStatus, string> = {
  done: "Done",
  in_progress: "In progress",
  planned: "Planned",
  idea: "Idea",
};

export type MilestoneGroup = {
  status: MilestoneStatus;
  milestones: Milestone[];
};

/** Shown groups top to bottom, each in `order`; empty and hidden groups drop out. */
export function timelineGroups(
  milestones: readonly Milestone[],
  hiddenStatuses: readonly MilestoneStatus[],
): MilestoneGroup[] {
  return TIMELINE_STATUSES.filter((status) => !hiddenStatuses.includes(status))
    .map((status) => ({
      status,
      milestones: milestones
        .filter((milestone) => milestone.status === status)
        .sort((a, b) => a.order - b.order),
    }))
    .filter((group) => group.milestones.length > 0);
}

export type AgentCreator = { kind: "mono" | "session"; id: string };

/** The agent behind a row, or null for rows the user added. */
export function agentCreator(createdBy: string): AgentCreator | null {
  const [kind, id] = createdBy.split(/:(.*)/s);
  return (kind === "mono" || kind === "session") && id
    ? { kind, id }
    : null;
}

const dayMonth = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
});
const dayMonthYear = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
});

/** "Oct 15", with the year only when it is not this year. */
export function formatDay(day: Date, today: Date): string {
  return (
    day.getFullYear() === today.getFullYear() ? dayMonth : dayMonthYear
  ).format(day);
}

/** `new Date("YYYY-MM-DD")` reads UTC midnight, a day early west of UTC. */
export function formatTarget(target: string, today: Date): string {
  const [year, month, date] = target.split("-").map(Number);
  return formatDay(new Date(year, month - 1, date), today);
}
