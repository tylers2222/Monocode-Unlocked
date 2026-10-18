// @vitest-environment happy-dom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MilestoneTimeline } from "./MilestoneTimeline";
import type { Milestone } from "../model/milestones";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const TODAY = new Date(2026, 9, 18);

function milestone(fields: Partial<Milestone> & Pick<Milestone, "id">): Milestone {
  return {
    projectPath: "/work/app",
    title: fields.id,
    note: "",
    status: "idea",
    target: null,
    order: 0,
    createdBy: "user",
    createdAt: 0,
    updatedAt: 0,
    ...fields,
  };
}

async function render(props: ComponentProps<typeof MilestoneTimeline>) {
  await act(async () =>
    root.render(createElement(MilestoneTimeline, { today: TODAY, ...props })),
  );
}

function row(id: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`[data-milestone="${id}"]`);
  if (!found) throw new Error(`no row ${id}`);
  return found;
}

/** Group names and row ids top to bottom, with the Today line as "today". */
function layout(): string[] {
  return [
    ...container.querySelectorAll<HTMLElement>(
      "section, [data-today], [data-milestone]",
    ),
  ].map((element) =>
    element.dataset.milestone ??
    (element.dataset.today === undefined
      ? `# ${element.getAttribute("aria-label")}`
      : "today"),
  );
}

it("groups rows Done, Today, In progress, Planned, Idea, each in sort order", async () => {
  await render({
    milestones: [
      milestone({ id: "idea-b", order: 7 }),
      milestone({ id: "planned", status: "planned", order: 4 }),
      milestone({ id: "done-b", status: "done", order: 3 }),
      milestone({ id: "idea-a", order: 2 }),
      milestone({ id: "doing", status: "in_progress", order: 5 }),
      milestone({ id: "done-a", status: "done", order: 1 }),
    ],
  });
  expect(layout()).toEqual([
    "# Done",
    "done-a",
    "done-b",
    "today",
    "# In progress",
    "doing",
    "# Planned",
    "planned",
    "# Idea",
    "idea-a",
    "idea-b",
  ]);
});

it("leaves hidden statuses out and keeps the Today line above the future", async () => {
  await render({
    milestones: [
      milestone({ id: "planned", status: "planned" }),
      milestone({ id: "idea" }),
    ],
    hiddenStatuses: ["idea"],
  });
  expect(layout()).toEqual(["today", "# Planned", "planned"]);
  expect(container.querySelector("[data-today]")?.textContent).toBe(
    `Today · ${TODAY.toLocaleDateString(undefined, { month: "short", day: "numeric" })}`,
  );
});

it("draws no Today line while Done is hidden, as approved frame M·8 shows", async () => {
  await render({
    milestones: [
      milestone({ id: "done", status: "done" }),
      milestone({ id: "planned", status: "planned" }),
    ],
    hiddenStatuses: ["done"],
  });
  expect(layout()).toEqual(["# Planned", "planned"]);
});

it("shows the local target date, with the year only when it is not this year, or Later", async () => {
  await render({
    milestones: [
      milestone({ id: "dated", target: "2026-11-07" }),
      milestone({ id: "next-year", target: "2027-01-05", order: 1 }),
      milestone({ id: "undated", order: 2 }),
    ],
  });
  const day = { month: "short", day: "numeric" } as const;
  // vitest.config.ts pins TZ west of UTC, where a UTC parse reads Nov 6.
  expect(row("dated").querySelector(".tabular-nums")?.textContent).toBe(
    new Date(2026, 10, 7).toLocaleDateString(undefined, day),
  );
  expect(row("dated").textContent).not.toContain("Later");
  expect(row("next-year").querySelector(".tabular-nums")?.textContent).toBe(
    new Date(2027, 0, 5).toLocaleDateString(undefined, {
      ...day,
      year: "numeric",
    }),
  );
  expect(row("undated").textContent).toContain("Later");
});

it("marks rows a Mono or a session added, and not rows the user added", async () => {
  await render({
    milestones: [
      milestone({ id: "by-mono", createdBy: "mono:m1" }),
      milestone({ id: "by-session", createdBy: "session:s1", order: 1 }),
      milestone({ id: "by-gone-mono", createdBy: "mono:gone", order: 2 }),
      milestone({ id: "by-gone-session", createdBy: "session:gone", order: 3 }),
      milestone({ id: "by-user", order: 4 }),
    ],
    lookUpCreator: ({ kind, id }) =>
      kind === "mono" && id === "m1"
        ? { name: "General", mascot: { name: "ghost", color: "#7FA6F0" } }
        : kind === "session" && id === "s1"
          ? { name: "Execute plan M tasks" }
          : undefined,
  });
  expect(row("by-mono").textContent).toContain("added by General");
  expect(row("by-session").textContent).toContain(
    "added by session · Execute plan M tasks",
  );
  expect(row("by-gone-mono").textContent).toContain("added by a Mono");
  expect(row("by-gone-session").textContent).toBe(
    "by-gone-sessionadded by sessionLater",
  );
  expect(row("by-user").textContent).not.toContain("added by");
});

it("highlights only the rows named in highlightIds, with a New label", async () => {
  await render({
    milestones: [
      milestone({ id: "fresh", createdBy: "mono:m1" }),
      milestone({ id: "old", createdBy: "mono:m1", order: 1 }),
    ],
    highlightIds: new Set(["fresh"]),
  });
  expect(row("fresh").textContent).toContain("New");
  expect(row("fresh").className).toContain("bg-accent/8");
  expect(row("old").textContent).not.toContain("New");
  expect(row("old").className).not.toContain("bg-accent/8");
});
