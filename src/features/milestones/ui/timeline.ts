import { formatDay } from "../model/draft";
import type { Milestone, MilestoneStatus } from "../model/milestones";

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
  return (kind === "mono" || kind === "session") && id ? { kind, id } : null;
}

/** "Oct 15" in English, with the year only when it is not this year. */
export function formatTarget(target: string, today: Date): string {
  return formatDay(target, Number(target.slice(0, 4)) !== today.getFullYear());
}
