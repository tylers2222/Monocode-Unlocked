// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { steerBlockReason } from "../../../app/model/appSessionSend";
import {
  appendSteerUser,
  appendUser,
  applyHarnessEvent,
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
import {
  busyRoute,
  planUserSend,
  runningItems,
  type RunningItem,
} from "../model/messageIntent";
import { appendPreparingHandoff, planComposerSwitch } from "../model/handoff";
import {
  dequeueQueuedMessage,
  nextQueuedSend,
  pauseQueue,
  resumeQueuedMessages,
  runningTurnMap,
} from "../model/messageQueue";
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
  newTextWaits,
  linkTo,
  NO_CHOICE,
  quoteText,
  runningTurnOf,
  steersNow,
  steersTyped,
  type BarChoice,
} from "./composerIntent";
import { ComposerIntentBar } from "./ComposerIntentBar";
import { MessageQuote } from "./MessageQuote";
import { MessageTargetsContext, PickTargetActions } from "./MessageTargets";

registerHarness({ id: "codex", live: true } as unknown as HarnessAdapter);
registerHarness({ id: "claude", live: true } as unknown as HarnessAdapter);
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
    const bar = composerBar(items, { kind: "default" }, true);
    expect(bar).toEqual({ kind: "turn" });
    expect(barSend(bar, true)).toEqual({});
  }
});

// Design 16.12.4, 16.12.6 (whole-turn review r1 M2). Defect: a cleared steer
// sends like `turn`, so the message the user made new joins the running turn.
it("makes ✕ a new message that waits for the turn end while the session would steer", () => {
  const bar = composerBar(running, { kind: "cleared" }, true);
  expect(bar).toEqual({ kind: "none" });
  expect(barSend(bar, true)).toEqual({ waitForIdle: true });
  expect(barSend(bar, false)).toEqual({});
});

// Design 16.8. Defect: a steer whose work ended is sent as a guess.
it("blocks a picked steer whose block left the running turn and offers it as new", () => {
  const bar = composerBar([], { kind: "picked", link: steer }, false);
  expect(bar).toEqual({ kind: "gone", link: steer });
  expect(barSend(bar, false)).toBeNull();
  expect(barSend({ kind: "none" }, false)).toEqual({});
  const live = composerBar(running, { kind: "picked", link: steer }, true);
  expect(barSend(live, true)).toEqual({ link: steer });
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
// queues the text, or stays silent where it steers; or, PR I5b review r2 m2,
// it says Steering while the session is busy with no turn, when typed text
// waits until the session is idle; or, PR I6 review r2 m2, while a Mono
// chat's queue is paused, when the pump holds every steer.
it("steers typed text only where the busy rule would", () => {
  const busy: Session = {
    ...newSession("codex", "/tmp"),
    busy: true,
    turnReadiness: "ready",
  };
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
  expect(steersTyped({ ...busy, turnReadiness: undefined }, true, "steer")).toBe(
    false,
  );
  for (const queueStatus of ["paused", "resuming"] as const) {
    expect(steersTyped({ ...busy, queueStatus }, true, "steer")).toBe(false);
    // A project session's busy send steers without the queue.
    expect(steersTyped({ ...busy, queueStatus }, false, "steer")).toBe(true);
  }
  expect(steersTyped({ ...busy, queueStatus: "active" }, true, "steer")).toBe(
    true,
  );
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
        busy: false,
        steering: false,
        newWaits: false,
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
  const newWaits = () => newTextWaits(chat, true);
  // SessionPane gives the bar the running items only while a steer reaches them.
  const running = () =>
    steersNow(chat, true) ? runningItems(chat, runningTurnOf(chat)) : [];
  const bar = (choice: BarChoice = NO_CHOICE) =>
    composerBar(running(), choice, steering());
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
            busy: !!chat.busy,
            steering: steering(),
            newWaits: newWaits(),
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
        steering() || choice.kind === "cleared" || newWaits(),
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
    steerTargets: () => new Set(running().map((item) => item.blockId)),
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

// PR I5b review r2 m2. Defect: while a Mono chat is busy with no turn, the
// bar says Steering, or the hover offers Steer on the ended turn's messages,
// though text typed then waits until the chat is idle.
it("shows no Steering bar and no hover Steer while a Mono chat is busy with no turn", () => {
  const mono = monoChat("ready");
  expect(mono.barText()).toContain("Steering");
  mono.update((chat) => ({ ...stopStreaming(chat), busy: true }));
  expect(runningItems(mono.chat, runningTurnOf(mono.chat))).not.toEqual([]);
  expect(mono.barText()).not.toContain("Steering");
  expect(mono.steerTargets()).toEqual(new Set());
});

// PR I6 review r3 m1. Defect: text typed while a Mono chat's queue is paused
// shows no bar but is sent as a plain message, so if the user resumes the
// queue before the turn ends, the pump steers it into that turn as a
// whole-turn steer.
it("lets text typed while the queue is paused wait for the turn end, even if the queue resumes first", () => {
  const mono = monoChat("ready");
  mono.update((chat) =>
    pauseQueue(
      enqueueMonoMessage(chat, {
        id: "failed",
        text: "failed earlier",
        attachments: [],
        waitForIdle: true,
        error: "provider error",
      }),
    ),
  );
  expect(mono.bar()).toEqual({ kind: "none" });
  const typed = mono.type("rename it to foo");
  // App's onResumeQueue sets a Mono queue active while the turn runs.
  mono.update((chat) => ({ ...chat, queueStatus: "active" }));
  expect(mono.next()).toBeUndefined();
  expect(monoMessageDeliveries(mono.chat).get(typed.id)).toEqual({
    status: "waiting",
  });
});

// PR I6 review r3 M1. Defect: while a provider switch is armed or the
// outgoing agent writes its hand-off brief, the bar says Steering and the
// hover offers Steer, though App's send then cancels the running turn and
// opens a hand-off turn (switch) or refuses the text (hand-off), and the CLI
// refuses a steer.
it("offers no steer while the session switches providers or prepares a hand-off", () => {
  const running: Session = {
    ...appendUser(newSession("claude", "/tmp"), "Build it", [], {
      turnId: turn1,
    }),
    busy: true,
    turnReadiness: "ready",
  };
  // As App's onModelChange arms a switch from the composer's model picker.
  const plan = planComposerSwitch(running, "codex");
  if (plan.kind !== "arm") throw new Error(`expected arm, got ${plan.kind}`);
  const switching: Session = {
    ...running,
    harness: "codex",
    pendingSwitch: plan.pending,
  };
  // As App reveals a hand-off turn while the outgoing agent writes its brief.
  const handingOff: Session = {
    ...appendUser(
      {
        ...appendPreparingHandoff(stopStreaming(running), "claude", "codex"),
        harness: "codex",
      },
      "next",
      [],
      { turnId: turn2 },
    ),
    busy: true,
  };
  for (const mono of [false, true]) {
    expect(steersNow(running, mono)).toBe(true);
    for (const session of [switching, handingOff]) {
      expect(steersNow(session, mono)).toBe(false);
      expect(steersTyped(session, mono, "steer")).toBe(false);
      expect(steerBlockReason(session, "stop")).toMatch(/switching providers/);
    }
  }
});

// PR I6 review r3 n2. Defect: a picked steer that cannot reach a turn that
// still runs says "Not running" and "That work finished", in the chip or in
// its tooltip (PR I6 review r4 n2).
it("says a picked steer cannot steer now while the session is busy, and not running once idle", () => {
  const markup = (busy: boolean) =>
    renderToStaticMarkup(
      createElement(ComposerIntentBar, {
        bar: { kind: "gone", link: steer },
        intent: {
          running: [],
          busy,
          steering: false,
          newWaits: false,
          choice: { kind: "picked", link: steer },
          onChoiceChange: () => {},
        },
        typing: true,
        onSendAsNew: () => {},
      }),
    ).replaceAll("&#x27;", "'");
  const tooltip = /class="composer-intent-chip" title="([^"]*)"/;
  const [busy, idle] = [markup(true), markup(false)];
  expect(busy.replace(/<[^>]+>/g, "")).toContain("Can't steer now");
  expect(busy.replace(/<[^>]+>/g, "")).not.toContain("Not running");
  expect(tooltip.exec(busy)?.[1]).toBe(
    "A steer cannot reach this work now. Pick Reply, or send it as new.",
  );
  expect(idle.replace(/<[^>]+>/g, "")).toContain("Not running");
  expect(tooltip.exec(idle)?.[1]).toBe(
    "That work finished. Edit the message or send it as new.",
  );
});

// PR I5b review r3 m1. Defect: in a chat that is not a Mono's, compacting
// by hand (busy, no turn) with the follow-up setting `steer`, the bar says
// no Steering while the send steers the text into no turn.
it("agrees with the send while a chat compacts: no Steering, and the text waits", () => {
  const ran = appendUser(newSession("codex", "/tmp"), "Build it", [], {
    turnId: "turn-1" as TurnId,
  });
  // As App.tsx builds a manual compaction.
  const compacting = applyHarnessEvent(
    { ...stopStreaming(ran), busy: true },
    { type: "status", text: "Compacting context…" },
  );
  expect(steersTyped(compacting, false, "steer")).toBe(false);
  const plan = planUserSend({
    origin: {},
    item: undefined,
    text: "also fix the tests",
    route: busyRoute(compacting, runningTurnMap(), "now"),
  });
  expect(plan).toEqual({ kind: "queue", item: { waitForIdle: true } });
});

// PR I6 review r4 M1 (probe P8). Defect: while a worktree-mode session
// creates its worktree for the first turn, the hover offers Steer and a
// picked steer is sent, though the provider has no turn yet and refuses it
// ("No active turn to steer"), so the agent never reads the text.
it("offers no steer while the first turn's worktree is prepared", () => {
  const preparing: Session = {
    ...appendUser(newSession("claude", "/tmp"), "Build it", [], {
      turnId: turn1,
    }),
    busy: true,
    turnReadiness: "starting",
    worktreePreparing: true,
  };
  const picked: BarChoice = {
    kind: "picked",
    link: linkTo("steer", preparing.blocks[0]),
  };
  for (const mono of [false, true]) {
    expect(steersNow({ ...preparing, worktreePreparing: false }, mono)).toBe(
      true,
    );
    // SessionPane gives the hover and the bar the running items only while a
    // steer reaches them.
    const reached = steersNow(preparing, mono)
      ? runningItems(preparing, turn1)
      : [];
    expect(reached).toEqual([]);
    const bar = composerBar(
      reached,
      picked,
      steersTyped(preparing, mono, "steer"),
    );
    expect(bar).toEqual({ kind: "gone", link: picked.link });
    expect(barSend(bar, false)).toBeNull();
  }
  expect(steerBlockReason(preparing, "use tabs")).toMatch(
    /preparing its worktree/,
  );
});

/** The ids an idle chat opens its queued messages with, one turn each. */
function idleOrder(chat: Session): string[] {
  const order: string[] = [];
  let idle = stopStreaming(chat);
  for (
    let next = nextQueuedSend(idle, runningTurnMap(), true);
    next;
    next = nextQueuedSend(idle, runningTurnMap(), true)
  ) {
    order.push(next.id);
    idle = dequeueQueuedMessage(idle, next.id);
  }
  return order;
}

// PR I6 review r4 m1. Defect: text typed while every steer is held waits
// by choice, which holds nothing behind it, so once the user resumes the
// queue or reverts the switch before the turn ends, their later text steers
// the running turn and reaches the agent before the earlier text.
it.each<[string, (chat: Session) => Session, (chat: Session) => Session]>([
  [
    "the queue is paused and resumed",
    (chat) =>
      pauseQueue(
        enqueueMonoMessage(chat, {
          id: "failed",
          text: "failed earlier",
          attachments: [],
          waitForIdle: true,
          error: "provider error",
        }),
      ),
    // As App's onResumeQueue resumes a Mono queue while the turn runs.
    (chat) => ({
      ...chat,
      queueStatus: "active",
      queuedMessages: resumeQueuedMessages(chat.queuedMessages),
    }),
  ],
  [
    "a provider switch is armed and reverted",
    (chat) => {
      const plan = planComposerSwitch(chat, "claude");
      if (plan.kind !== "arm") throw new Error(`expected arm, got ${plan.kind}`);
      return { ...chat, harness: "claude", pendingSwitch: plan.pending };
    },
    (chat) => ({ ...chat, harness: "codex", pendingSwitch: undefined }),
  ],
])(
  "keeps the user's typed order when %s before the turn ends",
  (_hold, hold, lift) => {
    const mono = monoChat("ready");
    mono.update(hold);
    expect(mono.bar()).toEqual({ kind: "none" });
    mono.type("rename it to foo");
    mono.update(lift);
    expect(mono.barText()).not.toContain("Steering");
    mono.type("and the tests");
    expect(mono.next()).toBeUndefined();
    expect(idleOrder(mono.chat).filter((id) => id !== "failed")).toEqual([
      "rename it to foo",
      "and the tests",
    ]);
  },
);

// Decision 658, PR I6 review r4 n2. Defect: text typed behind the user's
// missed message is sent as a chosen wait, which holds nothing behind it, so
// after the missed message opens the next turn, text typed in that turn
// steers ahead of it.
it("keeps typed order for text held behind a missed message into the next turn", () => {
  const mono = monoChat("ready");
  mono.update((chat) =>
    enqueueMonoMessage(chat, {
      id: "use the staging DB",
      text: "use the staging DB",
      attachments: [],
      waitForIdle: true,
      missed: true,
    }),
  );
  mono.type("and seed it");
  // Turn 1 ends; the missed message opens turn 2.
  mono.update((chat) => ({
    ...appendUser(
      dequeueQueuedMessage(stopStreaming(chat), "use the staging DB"),
      "use the staging DB",
      [],
      { turnId: turn2 },
    ),
    busy: true,
    turnReadiness: "ready",
  }));
  expect(mono.barText()).not.toContain("Steering");
  mono.type("then run the tests");
  expect(mono.next()).toBeUndefined();
  expect(idleOrder(mono.chat)).toEqual(["and seed it", "then run the tests"]);
});
