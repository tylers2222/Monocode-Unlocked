import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { normalizeProjectPath } from "../../projects/model/recents";

/** Rust emits it after every write, page or agent, to every window. */
export const MILESTONES_CHANGED = "monocode:milestones-changed";

export type MilestoneStatus = "idea" | "planned" | "in_progress" | "done";

export const MILESTONE_STATUSES: readonly MilestoneStatus[] = [
  "idea",
  "planned",
  "in_progress",
  "done",
];

/**
 * Who writes: the app's own page, or the agent a Rust grant identifies.
 * Rust refuses agent writes while a project's milestones are off.
 */
export type Writer = "user" | `mono:${string}` | `session:${string}`;

/** One row of `milestones_list` (Rust `Milestone`, camelCase). */
export type Milestone = {
  id: string;
  projectPath: string;
  title: string;
  note: string;
  status: MilestoneStatus;
  /** `YYYY-MM-DD`, or null for "Later"; the page also shows `""` as Later. */
  target: string | null;
  order: number;
  createdBy: Writer;
  /** Unix milliseconds. */
  createdAt: number;
  updatedAt: number;
};

export type MilestoneSettings = {
  enabled: boolean;
  hiddenStatuses: MilestoneStatus[];
};

export type ProjectMilestones = {
  settings: MilestoneSettings;
  milestones: Milestone[];
};

export type NewMilestone = {
  title: string;
  note?: string;
  status?: MilestoneStatus;
  target?: string | null;
};

/** Absent fields stay as they are; `target: ""` or null clears the date. */
export type MilestonePatch = Partial<NewMilestone>;

export function list(project: string): Promise<ProjectMilestones> {
  return invoke("milestones_list", {
    projectPath: normalizeProjectPath(project),
  });
}

export function add(
  project: string,
  milestone: NewMilestone,
  by: Writer,
): Promise<Milestone> {
  return invoke("milestones_add", {
    projectPath: normalizeProjectPath(project),
    milestone: { ...milestone, createdBy: by },
  });
}

export function update(
  project: string,
  id: string,
  patch: MilestonePatch,
  by: Writer,
): Promise<Milestone> {
  return invoke("milestones_update", {
    projectPath: normalizeProjectPath(project),
    id,
    // Rust stores Later as NULL only, so "" never reaches it.
    patch: patch.target === "" ? { ...patch, target: null } : patch,
    by,
  });
}

export function remove(project: string, id: string, by: Writer): Promise<void> {
  return invoke("milestones_remove", {
    projectPath: normalizeProjectPath(project),
    id,
    by,
  });
}

/** `ids` is every id of the project, in the new order. */
export function reorder(
  project: string,
  ids: string[],
  by: Writer,
): Promise<Milestone[]> {
  return invoke("milestones_reorder", {
    projectPath: normalizeProjectPath(project),
    ids,
    by,
  });
}

/** Deletes the project's milestones and settings, for project removal only. */
export function removeProject(project: string): Promise<void> {
  return invoke("milestones_remove_project", {
    projectPath: normalizeProjectPath(project),
  });
}

/** Calls `onChange` with the normalized path of each project that changed. */
export function subscribe(onChange: (projectPath: string) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void listen<{ projectPath: string }>(MILESTONES_CHANGED, (event) =>
    onChange(event.payload.projectPath),
  )
    .then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    })
    .catch(() => undefined);
  return () => {
    disposed = true;
    unlisten?.();
  };
}
