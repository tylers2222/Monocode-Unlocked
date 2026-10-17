// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import {
  appendSteerUser,
  appendUser,
  stopStreaming,
} from "../../../integrations/harness/core/apply";
import {
  registerHarness,
  type HarnessAdapter,
} from "../../../integrations/harness/core/registry";
import type { MessageId, TurnId } from "../../monos/model/monoMessages";
import {
  acknowledgeMonoMessage,
  enqueueMonoMessage,
  monoMessageDeliveries,
} from "../../monos/model/monoMessaging";
import { runningItems, type RunningItem } from "../model/messageIntent";
import { nextQueuedSend, runningTurnMap } from "../model/messageQueue";
import { typedMonoMessage } from "../model/queuedFollowUp";
import {
  newSession,
  WHOLE_TURN,
  type Session,
  type TargetLink,
} from "../model/session";
import {
  barSend,
  composerBar,
  NO_CHOICE,
  quoteText,
  runningTurnOf,
  steersTyped,
  steerTargets,
  type BarChoice,
} from "./composerIntent";
import { ComposerIntentBar } from "./ComposerIntentBar";
import { MessageQuote } from "./MessageQuote";
import { MessageTargetsContext, PickTargetActions } from "./MessageTargets";

registerHarness({ id: "codex", live: true } as unknown as HarnessAdapter);
registerHarness({
  id: "fx",
  live: true,
  canSteer: false,
} as unknown as HarnessAdapter);

const turn1 = "turn-1" as TurnId;
const turn2 = "turn-2" as TurnId;
const turn3 = "turn-3" as TurnId;
const running: RunningItem[] = [{ blockId: "opener", quote: "Build it" }];
const steer: TargetLink = {
  intent: "steer",
  replyTo: "opener",
  quote: "Build it",
};

afterEach(() => {
  vi.useRealTimers();
});

// Design 16.12.6. Defects: no pick while steering guesses a target, or
// blocks Send because several items run.
it("steers the whole turn with no pick, for one running item or several, and passes no link", () => {
  for (const items of [
    running,
    [...running, { blockId: "steer-2", quote: "Also" }],
  ]) {
    const bar = composerBar(items, { kind: "default" }, true, false);
    expect(bar).toEqual({ kind: "turn" });
    expect(barSend(bar, true)).toEqual({});
  }
});

// Design 16.12.4, 16.12.6 (whole-turn review r1 M2). Defect: a cleared steer
// sends like `turn`, so the message the user made new joins the running turn.
it("makes ✕ a new message that waits for the turn end while the session would steer", () => {
  const bar = composerBar(running, { kind: "cleared" }, true, true);
  expect(bar).toEqual({ kind: "none" });
  expect(barSend(bar, true)).toEqual({ waitForIdle: true });
  expect(barSend(bar, false)).toEqual({});
});

// Design 16.8. Defect: a steer whose work ended is sent as a guess.
it("blocks a picked steer whose block left the running turn and offers it as new", () => {
  const bar = composerBar([], { kind: "picked", link: steer }, false, false);
  expect(bar).toEqual({ kind: "gone", link: steer });
  expect(barSend(bar, false)).toBeNull();
  expect(barSend({ kind: "none" }, false)).toEqual({});
  const live = composerBar(
    running,
    { kind: "picked", link: steer },
    true,
    false,
  );
  expect(barSend(live, true)).toEqual({ link: steer });
});

// Doop E10, decision 536. Defect: text typed as a whole-turn steer silently
// opens a new turn when the turn ends before Send.
it("blocks Send once the turn the bar named has ended, until the user sends it as new or clears it", () => {
  const ended = composerBar([], { kind: "default" }, false, true);
  expect(ended).toEqual({ kind: "ended" });
  expect(barSend(ended, false)).toBeNull();
  expect(composerBar([], { kind: "default" }, false, false)).toEqual({
    kind: "none",
  });
  expect(composerBar([], { kind: "cleared" }, false, true)).toEqual({
    kind: "none",
  });
});

// Design 16.8 (intents review r1). Defect: position stands in for the
// running turn, so a bubble queued in turn 1 that opens turn 2 is read as
// turn 1's work and its Steer action names the wrong turn.
it("reads the running turn from the opener that started last, not from block order", () => {
  vi.useFakeTimers();
  vi.setSystemTime(1_000);
  let session = appendUser(newSession("codex", "/tmp"), "Build it", [], {
    turnId: turn1,
  });
  session = enqueueMonoMessage(session, {
    id: "later",
    text: "Then the signup page",
    attachments: [],
  });
  session = appendSteerUser(session, "Use blue", [], { turnId: turn1 });
  expect(runningTurnOf(session)).toBe(turn1);
  vi.setSystemTime(2_000);
  session = acknowledgeMonoMessage(
    stopStreaming(session),
    session.queuedMessages![0],
    { mode: "new-turn", cards: { turnId: turn2 } },
  );
  expect(session.blocks.at(-1)!.turnId).toBe(turn1);
  expect(runningTurnOf(session)).toBe(turn2);
  // An opener placed after turn 2's in the blocks, which started before it.
  vi.setSystemTime(1_500);
  const earlier = appendUser(session, "Old opener", [], { turnId: turn3 });
  expect(earlier.blocks.at(-1)!.turnId).toBe(turn3);
  expect(runningTurnOf(earlier)).toBe(turn2);
  expect(runningTurnOf(stopStreaming(session))).toBeUndefined();
});

// Design 16.8 `steering`. Defect: the bar says Steering where the busy rule
// queues the text, or stays silent where it steers.
it("steers typed text only where the busy rule would", () => {
  const busy = { ...newSession("codex", "/tmp"), busy: true };
  expect(steersTyped(busy, true, "queue")).toBe(true);
  expect(steersTyped(busy, false, "steer")).toBe(true);
  expect(steersTyped(busy, false, "queue")).toBe(false);
  expect(
    steersTyped({ ...busy, backgroundTasks: ["dev"] }, false, "queue"),
  ).toBe(true);
  expect(
    steersTyped({ ...busy, worktreePreparing: true }, false, "steer"),
  ).toBe(false);
  expect(steersTyped({ ...busy, busy: false }, true, "steer")).toBe(false);
  expect(steersTyped({ ...busy, harness: "fx" }, true, "steer")).toBe(false);
});

// PR I4 review n2, PR I6 review r1 n1. Defect: an override or isolate in the
// target's text reorders the quote, above the bubble or in the bar, so the
// user reads the words in another order.
it("drops bidirectional controls from a quote and isolates its direction", () => {
  const quote = "Pay \u202Eexe.txt\u202C and \u2067this\u2069\u200F now";
  const bidi = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/;
  expect(quoteText(quote)).toBe("Pay exe.txt and this now");
  const html = renderToStaticMarkup(
    createElement(MessageQuote, {
      link: { intent: "reply", replyTo: "m", quote },
    }),
  );
  expect(html).toContain('dir="auto"');
  expect(html).not.toMatch(bidi);
  const link: TargetLink = { intent: "reply", replyTo: "m", quote };
  const bar = renderToStaticMarkup(
    createElement(ComposerIntentBar, {
      bar: { kind: "picked", link },
      intent: {
        running: [],
        steering: false,
        choice: { kind: "picked", link },
        onChoiceChange: () => {},
      },
      typing: true,
      onSendAsNew: () => {},
    }),
  );
  expect(bar).toContain("Pay exe.txt and this now");
  expect(bar).not.toMatch(bidi);
});

// Doop E9, design 16.12.6. Defect: a whole-turn steer shows a target it does
// not have, or no label.
it("labels a whole-turn steer with no quote line", () => {
  const html = renderToStaticMarkup(
    createElement(MessageQuote, { link: WHOLE_TURN }),
  );
  expect(html).toContain("Steer · whole turn");
  expect(html).not.toContain("message-quote-text");
});

// Design 16.8 hover actions. Defect: Steer is offered on a message whose
// work ended, or picks a link that names another block.
it("offers Steer only on a running message and picks a link to it", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const pick = vi.fn();
  const container = document.createElement("div");
  const root = createRoot(container);
  const render = (id: string) =>
    act(() =>
      root.render(
        createElement(
          MessageTargetsContext.Provider,
          { value: { running: new Set(["opener"]), pick } },
          createElement(PickTargetActions, {
            block: { id, text: "Build it now" },
          }),
        ),
      ),
    );
  render("done");
  expect(container.querySelector('[data-pick-intent="steer"]')).toBeNull();
  render("opener");
  act(() =>
    container
      .querySelector<HTMLButtonElement>('[data-pick-intent="steer"]')!
      .click(),
  );
  expect(pick).toHaveBeenCalledWith({
    intent: "steer",
    replyTo: "opener",
    quote: "Build it now",
  });
  act(() => root.unmount());
  vi.unstubAllGlobals();
});

/**
 * A busy Mono chat in turn 1, its composer as SessionPane wires it, and the
 * queue pump that takes what the composer sends.
 */
function monoChat(turnReadiness: Session["turnReadiness"]) {
  let chat: Session = {
    ...appendUser(newSession("codex", "/tmp"), "Build it", [], {
      turnId: turn1,
    }),
    busy: true,
    turnReadiness,
  };
  const turns = () => runningTurnMap([[chat.id, runningTurnOf(chat)!]]);
  const steering = () => steersTyped(chat, true, "steer");
  const running = () => runningItems(chat, runningTurnOf(chat));
  const bar = (choice: BarChoice = NO_CHOICE) =>
    composerBar(running(), choice, steering(), false);
  return {
    get chat() {
      return chat;
    },
    update(change: (chat: Session) => Session) {
      chat = change(chat);
    },
    bar,
    /** What the user reads above the field while typing. */
    barText(choice: BarChoice = NO_CHOICE) {
      return renderToStaticMarkup(
        createElement(ComposerIntentBar, {
          bar: bar(choice),
          intent: {
            running: running(),
            steering: steering(),
            choice,
            onChoiceChange: () => {},
            monoName: "Ada",
          },
          typing: true,
          onSendAsNew: () => {},
        }),
      ).replace(/<[^>]+>/g, "");
    },
    /** Send `text` as the composer does (`useComposerBar` `send`). */
    type(text: string, choice: BarChoice = NO_CHOICE) {
      const origin = barSend(
        bar(choice),
        steering() || choice.kind === "cleared",
      );
      expect(origin).not.toBeNull();
      const item = typedMonoMessage({
        session: chat,
        turns: turns(),
        origin: origin!,
        message: { id: text, text, attachments: [] },
      })!;
      chat = enqueueMonoMessage(chat, item);
      return item;
    },
    /** What the pump steers into the running turn now. */
    next: () => nextQueuedSend(chat, turns(), true),
    steerTargets: () => steerTargets(chat, running()),
  };
}

// PR I6 review r1 M2, with PR #19. Defect: the bar or the hover Steer is
// hidden after ✕, so text the pump would steer reads as waiting.
it("steers text typed after ✕, and says Steering for it", () => {
  const mono = monoChat("ready");
  mono.type("after this, write docs", { kind: "cleared" });
  expect(mono.bar()).toEqual({ kind: "turn" });
  expect(mono.barText()).toContain("Steering");
  expect(mono.steerTargets()).toEqual(new Set([mono.chat.blocks[0].id]));
  mono.type("stop, wrong branch");
  expect(mono.next()?.text).toBe("stop, wrong branch");
});

// PR I6 review r1 M2, with PR #19. Defect: the bar says Steering, or the
// hover offers Steer, for text the pump holds behind the user's own message
// that missed its turn.
it("holds text typed behind a missed message, with no Steering bar and no hover Steer", () => {
  vi.useFakeTimers();
  vi.setSystemTime(1_000);
  const mono = monoChat("starting");
  const row = {
    id: "m_rowrowrowrow",
    text: "<mono_message>please review PR 12</mono_message>",
    attachments: [],
    waitForIdle: true as const,
    monoMessageId: "m_rowrowrowrow" as MessageId,
  };
  mono.update((chat) => enqueueMonoMessage(chat, row));
  mono.type("use the staging DB");
  // Turn 1 ends before it takes the steer; the Mono row opens turn 2.
  vi.setSystemTime(2_000);
  mono.update((chat) =>
    acknowledgeMonoMessage(
      { ...stopStreaming(chat), queueStatus: "active" },
      row,
      { mode: "new-turn", cards: { turnId: turn2 } },
    ),
  );
  mono.update((chat) => ({ ...chat, busy: true, turnReadiness: "ready" }));
  expect(runningTurnOf(mono.chat)).toBe(turn2);
  expect(mono.chat.queuedMessages?.[0]).toMatchObject({ missed: true });

  expect(mono.bar()).toEqual({ kind: "none" });
  expect(mono.barText()).not.toContain("Steering");
  expect(runningItems(mono.chat, turn2)).not.toEqual([]);
  expect(mono.steerTargets()).toEqual(new Set());
  const held = mono.type("stop, wrong branch");
  expect(mono.next()).toBeUndefined();
  expect(monoMessageDeliveries(mono.chat).get(held.id)).toEqual({
    status: "waiting",
  });
});
