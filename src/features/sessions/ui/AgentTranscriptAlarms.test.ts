// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Block } from "../model/session";
import type {
  AlarmCardView,
  SleepHoldView,
} from "../../alarms/model/alarmViews";
import { createViewStore } from "../../alarms/model/viewStore";
import type { TranscriptAlarmsView } from "../../alarms/ui/TranscriptAlarms";
import { parseMonoNotice } from "../../monos/model/monoMessages";
import { AgentTranscript } from "./AgentTranscript";

vi.mock("../../monos/model/monoMessages", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../../monos/model/monoMessages")>();
  return { ...actual, parseMonoNotice: vi.fn(actual.parseMonoNotice) };
});

function alarmText(id: string): string {
  return `<mono_alarm alarm="${id}" level="1" cause="dialog">
PR S1 reviewer (session:7c21) is waiting on a dialog.
</mono_alarm>`;
}

/** A `<mono_alarm>` as the app delivers it: its outbox key as the request id. */
function delivered(id: string): Block {
  return {
    id: `block-${id}`,
    role: "user",
    text: alarmText(id),
    appRequestId: `alarm:${id}:1`,
  };
}

const BLOCKS: Block[] = [
  delivered("al_7Q2"),
  { id: "reply", role: "assistant", text: "Nudging it." },
  { id: "go", role: "user", text: "Watch CI" },
  {
    id: "watch",
    role: "tool",
    text: "gh pr checks 912 --watch",
    tool: { kind: "shell", status: "pending", callId: "toolu_912" },
  },
];

const WORKER = {
  name: "PR S1 reviewer",
  actor: "session:7c21",
  role: "worker",
} as const;

const CARD: AlarmCardView = {
  id: "al_7Q2",
  cause: "dialog",
  stalled: WORKER,
  stalledSince: "14:16",
  minutes: 4,
  openWork: [],
  sends: [],
  dialog: {
    kind: "permission",
    minutes: 4,
    text: 'Allow Bash "rm -rf target"?',
    options: ["Allow", "Deny"],
  },
  level: 1,
  audience: "provisioner",
  next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 11 },
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

function views(
  cards: AlarmCardView[] = [],
  holds: SleepHoldView[] = [],
  onAct?: TranscriptAlarmsView["onAct"],
) {
  const holdStore = createViewStore(
    new Map(holds.map((hold) => [hold.callId, hold])),
  );
  const alarms: TranscriptAlarmsView = {
    cards: createViewStore(new Map(cards.map((card) => [card.id, card]))),
    holds: holdStore,
    onAct,
  };
  return { alarms, holdStore };
}

function markup(blocks: Block[], alarms?: TranscriptAlarmsView): string {
  return renderToStaticMarkup(
    createElement(AgentTranscript, { blocks, busy: true, alarms }),
  );
}

describe("AgentTranscript alarm host spots", () => {
  it("renders as it always has with no alarm views", () => {
    // Defect: an alarm or hold surface shows before row U2 passes data.
    const plain = markup(BLOCKS);
    expect(plain).not.toContain("data-alarm");
    expect(plain).not.toContain("data-sleep-hold");
    expect(plain).toBe(markup(BLOCKS, views().alarms));
  });

  it("shows the alarm card in place of its tag, and the hold under its tool row", () => {
    // Defect: a delivered alarm stays a raw tag, or a hold card lands under the wrong row.
    const shown = markup(BLOCKS, views([CARD], [HOLD]).alarms);
    expect(shown).toContain('data-alarm="al_7Q2"');
    expect(shown).not.toContain("&lt;mono_alarm");
    expect(shown).toContain('data-sleep-hold="sh_K1"');
    expect(shown.indexOf("gh pr checks 912 --watch")).toBeLessThan(
      shown.indexOf('data-sleep-hold="sh_K1"'),
    );
  });

  it("keeps a quoted or pasted alarm tag as a plain message", () => {
    // Defect: any message holding the tag is swallowed and shown as the live, actionable card.
    const { alarms } = views([CARD]);
    for (const block of [
      { id: "pasted", role: "user", text: alarmText("al_7Q2") },
      { ...delivered("al_7Q2"), appRequestId: "alarm:al_OTHER:1" },
      { ...delivered("al_7Q2"), appRequestId: "notice-m_1-unanswered" },
    ] satisfies Block[]) {
      const shown = markup([block], alarms);
      expect(shown).not.toContain("data-alarm");
      expect(shown).toContain("&lt;mono_alarm");
    }
  });

  it("puts the hold card under a MonoCode app call too", () => {
    // Defect: a held app call renders through the MonoCode row, which had no hold host.
    const appCall: Block = {
      id: "app",
      role: "tool",
      text: "monocode app sessions.read --json '{}'",
      tool: { kind: "shell", status: "pending", callId: "toolu_app" },
    };
    const shown = markup(
      [appCall],
      views([], [{ ...HOLD, callId: "toolu_app" }]).alarms,
    );
    expect(shown).toContain("data-monocode-tool-call");
    expect(shown).toContain('data-sleep-hold="sh_K1"');
  });
});

describe("AgentTranscript alarm acts", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
  });

  async function render(blocks: Block[], alarms: TranscriptAlarmsView) {
    await act(async () =>
      root.render(createElement(AgentTranscript, { blocks, alarms })),
    );
  }

  function buttonIn(scope: string, label: string): HTMLButtonElement {
    const found = Array.from(
      container.querySelectorAll<HTMLButtonElement>(`${scope} button`),
    ).find((button) => button.textContent?.trim() === label);
    if (!found) throw new Error(`no ${label} in ${scope}`);
    return found;
  }

  it("routes each card's act with its own alarm id", async () => {
    // Defect: two cards in one chat share one callback, so Allow on the second answers the first dialog.
    const onAct = vi.fn();
    const second: AlarmCardView = { ...CARD, id: "al_8R3" };
    await render(
      [delivered("al_7Q2"), delivered("al_8R3")],
      views([CARD, second], [], onAct).alarms,
    );
    await act(async () => buttonIn('[data-alarm="al_8R3"]', "Answer").click());
    await act(async () => buttonIn('[data-alarm="al_8R3"]', "Allow").click());
    await act(async () => buttonIn('[data-alarm="al_7Q2"]', "Nudge").click());
    expect(
      onAct.mock.calls.map(([event]) => [event.alarmId, event.act]),
    ).toEqual([
      ["al_8R3", "answer"],
      ["al_7Q2", "nudge"],
    ]);
  });

  it("ticks a hold's countdown without re-rendering the transcript's messages", async () => {
    // Defect: one hold's per-second countdown re-renders every block of the chat.
    const { alarms, holdStore } = views([], [HOLD]);
    await render(BLOCKS, alarms);
    expect(container.textContent).toContain("1:42");
    const renders = vi.mocked(parseMonoNotice).mock.calls.length;
    expect(renders).toBeGreaterThan(0);

    await act(async () =>
      holdStore.set(
        new Map([[HOLD.callId, { ...HOLD, remainingMs: 101_000 }]]),
      ),
    );
    expect(container.textContent).toContain("1:41");
    expect(vi.mocked(parseMonoNotice).mock.calls.length).toBe(renders);
  });
});
