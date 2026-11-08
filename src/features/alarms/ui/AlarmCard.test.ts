// @vitest-environment happy-dom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AlarmAgent, AlarmCardView } from "../model/alarmViews";
import { AlarmCard } from "./AlarmCard";

const BUILDER: AlarmAgent = {
  name: "PR S1 builder",
  actor: "session:9f3c",
  role: "worker",
};
const LEAD: AlarmAgent = { name: "PR-S1", actor: "session:51aa", role: "lead" };

/** Frame 2, state A: level 1, idle with open work, in the lead's chat. */
const IDLE: AlarmCardView = {
  id: "al_7Q2",
  cause: "idle_open_work",
  stalled: BUILDER,
  stalledSince: "14:05",
  minutes: 17,
  openWork: ['ask m_tngc "Build S1 stall detector" (due in 12 min)'],
  lastActivity: {
    at: "13:48",
    tool: "Bash",
    command: "cargo test -p monocode alarms",
    state: "completed",
  },
  sends: [{ recipient: "PR-S1", at: "14:20", level: 1 }],
  level: 1,
  audience: "provisioner",
  next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 15 },
};

/** Frame 2, state B: level 1 with a permission dialog open. */
const DIALOG: AlarmCardView = {
  ...IDLE,
  id: "al_8R3",
  cause: "dialog",
  stalled: { name: "PR S1 reviewer", actor: "session:7c21", role: "worker" },
  stalledSince: "14:16",
  minutes: 4,
  openWork: ['ask m_r8kd "Review PR S1 at exact head"'],
  lastActivity: {
    at: "14:16",
    tool: "Bash",
    command: "rm -rf target",
    state: "waiting for permission",
  },
  dialog: {
    kind: "permission",
    minutes: 4,
    text: 'Allow Bash "rm -rf target"?',
    options: ["Allow", "Deny"],
  },
  next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 11 },
};

/** Frame 2, state C: level 2 in Fork Orchestrator's Mono chat. */
const ESCALATED: AlarmCardView = {
  ...IDLE,
  minutes: 32,
  openWork: ['ask m_tngc "Build S1 stall detector" (overdue 3 min)'],
  level: 2,
  next: { kind: "provisioner", name: "Monocode General", minutes: 15 },
  escalation: { target: LEAD, missed: "its worker's stall", minutes: 15 },
};

/** Frame 4: Tyler's landing in the stalled session, last level. */
const LANDING: AlarmCardView = {
  ...IDLE,
  minutes: 60,
  openWork: ['ask m_tngc "Build S1 stall detector" (overdue 31 min)'],
  level: 4,
  audience: "tyler",
  next: { kind: "last" },
  sends: [
    { recipient: "PR-S1", at: "14:20", level: 1 },
    { recipient: "Fork Orchestrator", at: "14:35", level: 2 },
    { recipient: "Monocode General", at: "14:50", level: 3 },
  ],
  escalation: { target: LEAD, missed: "its worker's stall", minutes: 15 },
  lead: LEAD,
};

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

async function render(props: ComponentProps<typeof AlarmCard>) {
  await act(async () => root.render(createElement(AlarmCard, props)));
}

function text(): string {
  return (container.textContent ?? "").replace(/\s+/g, " ");
}

function button(label: string): HTMLButtonElement {
  const found = Array.from(container.querySelectorAll("button")).find(
    (node) => node.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no button ${label}`);
  return found;
}

function buttonLabels(): string[] {
  return Array.from(container.querySelectorAll("button"), (node) =>
    (node.textContent ?? "").trim(),
  );
}

describe("AlarmCard, frame 2 state A (level 1, idle)", () => {
  it("reads the frame's lines", async () => {
    // Defect: the card drops a line of the episode, or words it unlike the approved frame.
    await render({ view: IDLE });
    expect(text()).toContain("Alarm from the app · level 1");
    expect(text()).toContain("Goes to Fork Orchestrator in 15 min");
    expect(text()).toContain(
      "Stalled · PR S1 builder (session:9f3c) is stalled since 14:05 (17 min, no turn).",
    );
    expect(text()).toContain(
      'Open workask m_tngc "Build S1 stall detector" (due in 12 min)',
    );
    expect(text()).toContain(
      "Last activity13:48 Bash cargo test -p monocode alarms (completed)",
    );
    expect(text()).toContain("Dialognone");
  });

  it("offers the five acts with Answer off, since there is no dialog", async () => {
    // Defect: Answer can be clicked with no dialog to answer, or an act is missing.
    await render({ view: IDLE });
    expect(buttonLabels()).toEqual([
      "Read",
      "Answer",
      "Steer",
      "Nudge",
      "Dismiss",
    ]);
    expect(button("Answer").disabled).toBe(true);
  });

  it("aims every act at the stalled agent", async () => {
    // Defect: a level-1 act lands on someone other than the stalled agent.
    const onAct = vi.fn();
    await render({ view: IDLE, onAct });
    for (const label of ["Read", "Steer", "Nudge", "Dismiss"]) {
      await act(async () => button(label).click());
    }
    expect(onAct.mock.calls).toEqual([
      ["read", BUILDER],
      ["steer", BUILDER],
      ["nudge", BUILDER],
      ["dismiss", BUILDER],
    ]);
  });

  it("swaps the countdown for the act once someone acted", async () => {
    // Defect: an acted-on card still counts down to a level that will not come.
    await render({
      view: { ...IDLE, acted: { how: "nudge", by: "PR-S1", at: "14:21" } },
    });
    expect(text()).not.toContain("Goes to Fork Orchestrator");
    expect(text()).toContain(
      "NudgedPR-S1 nudged PR S1 builder at 14:21 · escalation stopped",
    );
  });

  it("says when the next level is Tyler", async () => {
    // Defect: the last provisioner is told a Giga comes next when it is Tyler.
    await render({ view: { ...IDLE, next: { kind: "tyler", minutes: 15 } } });
    expect(text()).toContain("Notifies Tyler in 15 min");
  });
});

describe("AlarmCard, frame 2 state B (level 1, dialog)", () => {
  it("names the dialog and leads with Waiting on dialog", async () => {
    // Defect: a dialog stall reads as plain Stalled, or hides the dialog text.
    await render({ view: DIALOG });
    expect(text()).toContain(
      "Waiting on dialog · PR S1 reviewer (session:7c21) is stalled since 14:16 (4 min, dialog open).",
    );
    expect(text()).toContain(
      'DialogDialog (permission, 4 min): Allow Bash "rm -rf target"?',
    );
    expect(text()).toContain("Goes to Fork Orchestrator in 11 min");
  });

  it("opens the dialog's own options under Answer and sends the one picked", async () => {
    // Defect: Answer shows no options, or sends a different one than clicked.
    const onAnswer = vi.fn();
    await render({ view: DIALOG, onAnswer });
    expect(button("Answer").disabled).toBe(false);
    expect(text()).not.toContain("Answer for PR S1 reviewer:");

    await act(async () => button("Answer").click());
    expect(text()).toContain("Answer for PR S1 reviewer:");
    await act(async () => button("Deny").click());
    expect(onAnswer).toHaveBeenCalledWith("Deny");
  });
});

describe("AlarmCard, frame 2 state C (level 2, escalated)", () => {
  it("says the lead did not act, with a red level chip", async () => {
    // Defect: an escalated card reads like level 1, so the Mono cannot tell the lead ignored it.
    await render({ view: ESCALATED });
    expect(text()).toContain("Alarm from the applevel 2");
    expect(text()).not.toContain("· level 2");
    expect(text()).toContain("Goes to Monocode General in 15 min");
    expect(text()).toContain(
      "Not acted on · Lead PR-S1 (session:51aa) did not act on its worker's stall for 15 min.",
    );
    expect(text()).toContain(
      "WorkerPR S1 builder (session:9f3c), stalled since 14:05 (32 min, no turn)",
    );
    expect(text()).toContain("ToldPR-S1 at 14:20 (level 1)");
  });

  it("aims Steer and Nudge at the lead, never the worker", async () => {
    // Defect: a Mono's act skips its lead and steers the worker, breaking the hierarchy.
    const onAct = vi.fn();
    await render({ view: ESCALATED, onAct });
    expect(buttonLabels()).toEqual([
      "Read",
      "Answer",
      "Steer PR-S1",
      "Nudge PR-S1",
      "Dismiss",
    ]);
    await act(async () => button("Steer PR-S1").click());
    await act(async () => button("Nudge PR-S1").click());
    await act(async () => button("Dismiss").click());
    expect(onAct.mock.calls).toEqual([
      ["steer", LEAD],
      ["nudge", LEAD],
      ["dismiss", LEAD],
    ]);
  });
});

describe("AlarmCard, frame 4 (Tyler's landing)", () => {
  it("reads as the last level, to you, with who was told", async () => {
    // Defect: Tyler's landing hides who was told, or promises another level.
    await render({ view: LANDING });
    expect(text()).toContain("Alarm from the app · to youlevel 4");
    expect(text()).toContain("Last level · it does not escalate further");
    expect(text()).toContain(
      "Stalled · PR S1 builder (session:9f3c) is stalled since 14:05 (60 min, no turn).",
    );
    expect(text()).toContain(
      "Told, no actPR-S1 14:20 · Fork Orchestrator 14:35 · Monocode General 14:50",
    );
  });

  it("drops Read, aims acts at the stalled agent, and opens the lead", async () => {
    // Defect: Tyler's acts go one level down instead of to the stalled agent.
    const onAct = vi.fn();
    const onOpenLead = vi.fn();
    await render({ view: LANDING, onAct, onOpenLead });
    expect(buttonLabels()).toEqual([
      "Answer",
      "Steer",
      "Nudge",
      "Dismiss",
      "Open PR-S1",
    ]);
    await act(async () => button("Steer").click());
    await act(async () => button("Open PR-S1").click());
    expect(onAct).toHaveBeenCalledWith("steer", BUILDER);
    expect(onOpenLead).toHaveBeenCalledWith(LEAD);
  });
});
