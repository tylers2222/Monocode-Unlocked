import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Block } from "../model/session";
import type {
  AlarmCardView,
  SleepHoldView,
} from "../../alarms/model/alarmViews";
import type { TranscriptAlarmsView } from "../../alarms/ui/TranscriptAlarms";
import { AgentTranscript } from "./AgentTranscript";

const ALARM_TEXT = `<mono_alarm alarm="al_7Q2" level="1" cause="idle_open_work">
PR S1 builder (session:9f3c) is stalled since 14:05 (17 min, no turn).
</mono_alarm>`;

const BLOCKS: Block[] = [
  { id: "alarm", role: "user", text: ALARM_TEXT },
  { id: "reply", role: "assistant", text: "Nudging it." },
  { id: "go", role: "user", text: "Watch CI" },
  {
    id: "watch",
    role: "tool",
    text: "gh pr checks 912 --watch",
    tool: { kind: "shell", status: "pending", callId: "toolu_912" },
  },
];

const CARD: AlarmCardView = {
  id: "al_7Q2",
  cause: "idle_open_work",
  stalled: { name: "PR S1 builder", actor: "session:9f3c", role: "worker" },
  stalledSince: "14:05",
  minutes: 17,
  openWork: [],
  sends: [],
  level: 1,
  audience: "provisioner",
  next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 15 },
};

const HOLD: SleepHoldView = {
  id: "sh_K1",
  callId: "toolu_912",
  agent: { name: "Watch CI for #912", actor: "session:4d10", role: "session" },
  command: "gh pr checks 912 --watch",
  howLong: "until checks finish (no bound)",
  remainingMs: 102_000,
  windowMs: 120_000,
};

function render(alarms?: TranscriptAlarmsView) {
  return renderToStaticMarkup(
    createElement(AgentTranscript, { blocks: BLOCKS, busy: true, alarms }),
  );
}

describe("AgentTranscript alarm host spots", () => {
  it("renders as it always has with no alarm views", () => {
    // Defect: an alarm or hold surface shows before row U2 passes data.
    const markup = render();
    expect(markup).not.toContain("data-alarm");
    expect(markup).not.toContain("data-sleep-hold");
    expect(markup).toBe(render({ cards: new Map(), holds: new Map() }));
  });

  it("shows the alarm card in place of its tag, and the hold under its tool row", () => {
    // Defect: a delivered alarm stays a raw tag, or a hold card lands under the wrong row.
    const markup = render({
      cards: new Map([[CARD.id, CARD]]),
      holds: new Map([[HOLD.callId, HOLD]]),
    });
    expect(markup).toContain('data-alarm="al_7Q2"');
    expect(markup).not.toContain("&lt;mono_alarm");
    expect(markup).toContain('data-sleep-hold="sh_K1"');
    expect(markup.indexOf("gh pr checks 912 --watch")).toBeLessThan(
      markup.indexOf('data-sleep-hold="sh_K1"'),
    );
  });
});
