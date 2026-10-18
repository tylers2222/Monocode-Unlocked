export type MilestoneStatus = "idea" | "planned" | "in_progress" | "done";

/** One row of `milestones_list` (Rust `Milestone`, camelCase). */
export type Milestone = {
  id: string;
  projectPath: string;
  title: string;
  note: string;
  status: MilestoneStatus;
  /** `YYYY-MM-DD`, or null for "Later". */
  target: string | null;
  order: number;
  /** `user`, `mono:<monoId>` or `session:<sessionId>`. */
  createdBy: string;
  /** Unix milliseconds. */
  createdAt: number;
  updatedAt: number;
};
