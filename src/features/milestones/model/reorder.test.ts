import { expect, it } from "vitest";
import type { Milestone, MilestoneStatus } from "./milestones";
import { dropBeside, moveOf, orderKey, stepOf } from "./reorder";

const row = (
  id: string,
  status: MilestoneStatus,
  order: number,
): Milestone => ({
  id,
  projectPath: "/p",
  title: id,
  note: "",
  status,
  target: null,
  order,
  createdBy: "user",
  createdAt: 0,
  updatedAt: 0,
});

/** Saved order a0 p1 p2 i3 p4, shown as Planned p1 p2 p4, Idea i3. */
const ROWS = [
  row("a0", "in_progress", 0),
  row("p1", "planned", 1),
  row("p2", "planned", 2),
  row("i3", "idea", 3),
  row("p4", "planned", 4),
];
const group = (status: MilestoneStatus) => ({
  status,
  milestones: ROWS.filter((m) => m.status === status),
});
const SHOWN = [group("in_progress"), group("planned"), group("idea")];

it("moveOf sends every id of the project, the row placed before its target", () => {
  expect(moveOf(ROWS, { id: "p4", status: "planned", before: "p1" })).toEqual({
    ids: ["a0", "p4", "p1", "p2", "i3"],
    status: null,
  });
});

it("moveOf reads the saved order, not the order the list arrives in", () => {
  expect(
    moveOf([...ROWS].reverse(), { id: "p4", status: "planned", before: "p1" }),
  ).toEqual({ ids: ["a0", "p4", "p1", "p2", "i3"], status: null });
});

it("moveOf puts a row last in its group after that group's last row, not at the end", () => {
  expect(moveOf(ROWS, { id: "a0", status: "planned", before: null })).toEqual({
    ids: ["p1", "p2", "i3", "p4", "a0"],
    status: "planned",
  });
  expect(
    moveOf(ROWS, { id: "p1", status: "in_progress", before: null }),
  ).toEqual({
    ids: ["a0", "p1", "p2", "i3", "p4"],
    status: "in_progress",
  });
});

it("moveOf is null when nothing would change, so nothing is sent", () => {
  expect(
    moveOf(ROWS, { id: "p1", status: "planned", before: "p2" }),
  ).toBeNull();
  expect(
    moveOf(ROWS, { id: "p1", status: "planned", before: "p1" }),
  ).toBeNull();
  expect(
    moveOf(ROWS, { id: "gone", status: "planned", before: null }),
  ).toBeNull();
  expect(
    moveOf(ROWS, { id: "p1", status: "planned", before: "gone" }),
  ).toBeNull();
});

it("stepOf moves one shown place, and past a group's edge into the next group", () => {
  expect(stepOf(SHOWN, "p2", -1)).toEqual({
    id: "p2",
    status: "planned",
    before: "p1",
  });
  expect(stepOf(SHOWN, "p1", 1)).toEqual({
    id: "p1",
    status: "planned",
    before: "p4",
  });
  expect(stepOf(SHOWN, "p2", 1)).toEqual({
    id: "p2",
    status: "planned",
    before: null,
  });
  expect(stepOf(SHOWN, "p1", -1)).toEqual({
    id: "p1",
    status: "in_progress",
    before: null,
  });
  expect(stepOf(SHOWN, "p4", 1)).toEqual({
    id: "p4",
    status: "idea",
    before: "i3",
  });
  expect(stepOf(SHOWN, "a0", -1)).toBeNull();
  expect(stepOf(SHOWN, "i3", 1)).toBeNull();
});

it("dropBeside reads the upper half as before and the lower half as after", () => {
  expect(dropBeside(SHOWN, "i3", { id: "p2", side: "before" })).toEqual({
    id: "i3",
    status: "planned",
    before: "p2",
  });
  expect(dropBeside(SHOWN, "i3", { id: "p4", side: "after" })).toEqual({
    id: "i3",
    status: "planned",
    before: null,
  });
  expect(dropBeside(SHOWN, "a0", { id: "p1", side: "after" })).toEqual({
    id: "a0",
    status: "planned",
    before: "p2",
  });
});

it("orderKey changes with order, status or rows, and not with a title edit", () => {
  const key = orderKey(ROWS);
  expect(orderKey(ROWS.map((m) => ({ ...m, title: "new" })))).toBe(key);
  expect(orderKey([...ROWS].reverse())).toBe(key);
  expect(
    orderKey(ROWS.map((m) => (m.id === "p1" ? { ...m, order: 9 } : m))),
  ).not.toBe(key);
  expect(
    orderKey(ROWS.map((m) => (m.id === "p1" ? { ...m, status: "done" } : m))),
  ).not.toBe(key);
  expect(orderKey(ROWS.slice(1))).not.toBe(key);
});
