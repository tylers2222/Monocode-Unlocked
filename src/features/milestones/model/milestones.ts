import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { normalizeProjectPath } from "../../projects/model/recents";

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
  /** `YYYY-MM-DD`, or null for "Later". */
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

/** Absent fields stay as they are; `target: null` clears the date. */
export type MilestonePatch = Partial<NewMilestone>;

export const MILESTONES_CHANGED = "monocode:milestones-changed";

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
    patch,
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

export function settings(project: string): Promise<MilestoneSettings> {
  return invoke("milestone_settings_get", {
    projectPath: normalizeProjectPath(project),
  });
}

/** Only the app's page changes settings, so there is no writer. */
export function setSettings(
  project: string,
  patch: Partial<MilestoneSettings>,
): Promise<MilestoneSettings> {
  return invoke("milestone_settings_set", {
    projectPath: normalizeProjectPath(project),
    patch,
  });
}

/** Deletes the project's milestones and settings, for project removal only. */
export function removeProject(project: string): Promise<void> {
  return invoke("milestones_remove_project", {
    projectPath: normalizeProjectPath(project),
  });
}

/** Calls `onChange` after any write to this project's milestones. */
export function subscribe(project: string, onChange: () => void): () => void {
  const projectPath = normalizeProjectPath(project);
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void listen<{ projectPath: string }>(MILESTONES_CHANGED, (event) => {
    if (event.payload.projectPath === projectPath) onChange();
  })
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
