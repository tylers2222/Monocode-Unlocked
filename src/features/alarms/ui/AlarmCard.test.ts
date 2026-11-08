// @vitest-environment happy-dom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  AlarmAgent,
  AlarmCardView,
  AlarmEpisodeView,
  TylerLandingView,
} from "../model/alarmViews";
import { AlarmCard } from "./AlarmCard";

const BUILDER: AlarmAgent = {
  name: "PR S1 builder",
  actor: "session:9f3c",
  role: "worker",
};
const LEAD: AlarmAgent = { name: "PR-S1", actor: "session:51aa", role: "lead" };

const EPISODE: AlarmEpisodeView = {
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
};

/** Frame 2, state A: level 1, idle with open work, in the lead's chat. */
const IDLE: AlarmCardView = {
  ...EPISODE,
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
const LANDING: TylerLandingView = {
  ...EPISODE,
  minutes: 60,
  openWork: ['ask m_tngc "Build S1 stall detector" (overdue 31 min)'],
  level: 4,
  audience: "tyler",
  sends: [
    { recipient: "PR-S1", at: "14:20", level: 1 },
    { recipient: "Fork Orchestrator", at: "14:35", level: 2 },
    { recipient: "Monocode General", at: "14:50", level: 3 },
  ],
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

function field(label: string): HTMLInputElement {
  const found = container.querySelector<HTMLInputElement>(
    `input[aria-label="${label}"]`,
  );
  if (!found) throw new Error(`no field ${label}`);
  return found;
}

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function click(label: string) {
  await act(async () => button(label).click());
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

  it("aims every act at the stalled agent, with the alarm's id", async () => {
    // Defect: a level-1 act lands on someone other than the stalled agent, or loses its alarm.
    const onAct = vi.fn();
    await render({ view: IDLE, onAct });
    await click("Read");
    await click("Nudge");
    await click("Steer");
    await type(field("Steer PR S1 builder"), "Run the tests again");
    await click("Send");
    await click("Dismiss");
    await type(field("Reason to dismiss"), "It is waiting on CI");
    await click("Send");
    const on = { kind: "alarm", alarmId: "al_7Q2", target: BUILDER };
    expect(onAct.mock.calls).toEqual([
      [{ ...on, act: "read" }],
      [{ ...on, act: "nudge" }],
      [{ ...on, act: "steer", text: "Run the tests again" }],
      [{ ...on, act: "dismiss", text: "It is waiting on CI" }],
    ]);
  });

  it("opens a one-line Steer box and a Dismiss reason, sending nothing empty", async () => {
    // Defect: Steer or Dismiss fire at once with no text for alarms.steer --text or alarms.dismiss --reason.
    const onAct = vi.fn();
    await render({ view: IDLE, onAct });
    await click("Steer");
    expect(button("Steer").getAttribute("aria-expanded")).toBe("true");
    expect(text()).toContain("Steer PR S1 builder:");
    expect(button("Send").disabled).toBe(true);
    await type(field("Steer PR S1 builder"), "   ");
    expect(button("Send").disabled).toBe(true);

    await click("Dismiss");
    expect(text()).not.toContain("Steer PR S1 builder:");
    expect(text()).toContain("Reason to dismiss:");
    await act(async () =>
      field("Reason to dismiss").form!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      ),
    );
    expect(onAct).not.toHaveBeenCalled();
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
    // Defect: Answer shows no options, sends a different one than clicked, or loses its alarm.
    const onAct = vi.fn();
    await render({ view: DIALOG, onAct });
    expect(button("Answer").disabled).toBe(false);
    expect(button("Answer").getAttribute("aria-expanded")).toBe("false");
    expect(text()).not.toContain("Answer for PR S1 reviewer:");

    await click("Answer");
    // Frame 2 B fills the open Answer button.
    expect(button("Answer").getAttribute("aria-expanded")).toBe("true");
    expect(button("Answer").className).toContain("aria-expanded:bg-selection");
    expect(text()).toContain("Answer for PR S1 reviewer:");
    await click("Deny");
    expect(onAct).toHaveBeenCalledWith({
      kind: "alarm",
      alarmId: "al_8R3",
      act: "answer",
      target: DIALOG.stalled,
      answer: { kind: "option", option: "Deny" },
    });
  });

  it("answers a question with no choices in a text box", async () => {
    // Defect: a free-text question dialog has no way to be answered from the card.
    const onAct = vi.fn();
    await render({
      view: {
        ...DIALOG,
        cause: "dialog",
        dialog: {
          kind: "question",
          minutes: 4,
          text: "Which branch should I rebase on?",
          options: [],
        },
      },
      onAct,
    });
    await click("Answer");
    await type(field("Answer for PR S1 reviewer"), "tyler/main");
    await click("Send");
    expect(onAct).toHaveBeenCalledWith(
      expect.objectContaining({
        act: "answer",
        answer: { kind: "text", text: "tyler/main" },
      }),
    );
  });

  it("starts closed again for the next episode", async () => {
    // Defect: Answer stays open from the last episode, so one stray click answers an unread dialog.
    await render({ view: DIALOG });
    await click("Answer");
    expect(text()).toContain("Answer for PR S1 reviewer:");
    await act(async () =>
      root.render(
        createElement(AlarmCard, {
          key: "al_9Z1",
          view: { ...DIALOG, id: "al_9Z1" },
        }),
      ),
    );
    expect(text()).not.toContain("Answer for PR S1 reviewer:");
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
    await click("Nudge PR-S1");
    await click("Steer PR-S1");
    await type(field("Steer PR-S1"), "Read your worker's alarm");
    await click("Send");
    expect(
      onAct.mock.calls.map(([event]) => [event.act, event.target]),
    ).toEqual([
      ["nudge", LEAD],
      ["steer", LEAD],
    ]);
  });

  it("names the lead in the acted line, since the act landed on it", async () => {
    // Defect: "nudged PR S1 builder" when the Mono nudged the lead.
    await render({
      view: {
        ...ESCALATED,
        acted: { how: "nudge", by: "Fork Orchestrator", at: "14:36" },
      },
    });
    expect(text()).toContain(
      "NudgedFork Orchestrator nudged PR-S1 at 14:36 · escalation stopped",
    );
  });

  it("names the stalled worker once Read or Answer, which land on it, acted", async () => {
    // Defect: "answered the dialog for PR-S1" when the dialog answered was PR S1 reviewer's.
    const escalatedDialog: AlarmCardView = {
      ...DIALOG,
      level: 2,
      next: { kind: "provisioner", name: "Monocode General", minutes: 15 },
      escalation: { target: LEAD, missed: "its worker's dialog", minutes: 15 },
    };
    const lines: string[] = [];
    for (const how of ["answer", "read"] as const) {
      await render({
        view: {
          ...escalatedDialog,
          acted: { how, by: "Fork Orchestrator", at: "14:36" },
        },
      });
      lines.push(text());
    }
    expect(lines[0]).toContain(
      "Fork Orchestrator answered the dialog for PR S1 reviewer at 14:36",
    );
    expect(lines[1]).toContain(
      "Fork Orchestrator read the alarm for PR S1 reviewer at 14:36",
    );
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
    await click("Nudge");
    await click("Open PR-S1");
    expect(onAct).toHaveBeenCalledWith({
      kind: "alarm",
      alarmId: "al_7Q2",
      act: "nudge",
      target: BUILDER,
    });
    expect(onOpenLead).toHaveBeenCalledWith(LEAD);
  });

  it("shows no Told row when nobody was told before Tyler", async () => {
    // Defect: a session Tyler started lands with an empty "Told, no act" row.
    await render({ view: { ...LANDING, level: 1, sends: [] } });
    expect(text()).not.toContain("Told");
    expect(text()).toContain("Alarm from the app · to you");
    expect(text()).not.toContain("level 1");
  });
});
