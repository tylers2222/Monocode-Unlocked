import type { Milestone, MilestoneStatus } from "./milestones";

/** Where a moved row lands: in group `status`, before row `before`, or last in it. */
export type Drop = {
  id: string;
  status: MilestoneStatus;
  before: string | null;
};

/** A row the timeline shows, top to bottom, with the group it is in. */
type Shown = { status: MilestoneStatus; milestones: readonly Milestone[] };

/**
 * The project's full id order after `drop`, as `milestones_reorder` takes it,
 * and the new status when the row changed group. Null when nothing moves.
 */
export function moveOf(
  milestones: readonly Milestone[],
  drop: Drop,
): { ids: string[]; status: MilestoneStatus | null } | null {
  const ordered = [...milestones].sort((a, b) => a.order - b.order);
  const moved = ordered.find((row) => row.id === drop.id);
  const rest = ordered.filter((row) => row.id !== drop.id);
  if (!moved || drop.before === drop.id) return null;
  const lastInGroup = rest.reduce(
    (last, row, index) => (row.status === drop.status ? index : last),
    rest.length - 1,
  );
  const at =
    drop.before === null
      ? lastInGroup + 1
      : rest.findIndex((row) => row.id === drop.before);
  if (at < 0) return null;
  const ids = rest.map((row) => row.id);
  ids.splice(at, 0, drop.id);
  const status = moved.status === drop.status ? null : drop.status;
  const same = ids.every((id, index) => id === ordered[index].id);
  return same && status === null ? null : { ids, status };
}

/**
 * Alt+ArrowUp/Down: one place up or down in what the timeline shows. Past
 * the edge of a group the row goes into the next shown group, as a drag
 * across a group header does.
 */
export function stepOf(
  groups: readonly Shown[],
  id: string,
  delta: -1 | 1,
): Drop | null {
  const at = groups.findIndex((group) =>
    group.milestones.some((row) => row.id === id),
  );
  if (at < 0) return null;
  const { status, milestones: rows } = groups[at];
  const index = rows.findIndex((row) => row.id === id);
  if (delta < 0) {
    if (index > 0) return { id, status, before: rows[index - 1].id };
    const above = groups[at - 1];
    return above ? { id, status: above.status, before: null } : null;
  }
  if (index < rows.length - 1)
    return { id, status, before: rows[index + 2]?.id ?? null };
  const below = groups[at + 1];
  return below
    ? { id, status: below.status, before: below.milestones[0]?.id ?? null }
    : null;
}

/** Changes when a row moves, changes group, comes or goes; a title edit does not. */
export function orderKey(milestones: readonly Milestone[]): string {
  return milestones
    .map((row) => `${row.id}:${row.status}:${row.order}`)
    .sort()
    .join();
}

/** The drop for a row dragged over row `over.id`, on its upper or lower half. */
export function dropBeside(
  groups: readonly Shown[],
  id: string,
  over: { id: string; side: "before" | "after" },
): Drop | null {
  const group = groups.find((shown) =>
    shown.milestones.some((row) => row.id === over.id),
  );
  if (!group) return null;
  if (over.side === "before")
    return { id, status: group.status, before: over.id };
  const index = group.milestones.findIndex((row) => row.id === over.id);
  return {
    id,
    status: group.status,
    before: group.milestones[index + 1]?.id ?? null,
  };
}
