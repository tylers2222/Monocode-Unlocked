import { describe, expect, it } from "vitest";
import { applyHarnessEvent } from "../../../integrations/harness/core/apply";
import {
  registerHarness,
  type HarnessAdapter,
} from "../../../integrations/harness/core/registry";
import type { MessageId, TurnId } from "../../monos/model/monoMessages";
import { appendPreparingHandoff } from "./handoff";
import {
  canDispatchQueuedHead,
  dequeueQueuedMessage,
  isEditingQueuedHead,
  nextQueuedSend,
  queuedHead,
  queuedMessageForSubmit,
  runningTurnMap,
  steerableQueuedMessage,
  failQueuedMessage,
  pauseQueue,
  queuePauseNotice,
  wakesQueue,
} from "./messageQueue";
import { newSession, type QueuedMessage, type Session } from "./session";

const T1 = "turn-1" as TurnId;
const T2 = "turn-2" as TurnId;

registerHarness({ id: "claude", live: true } as unknown as HarnessAdapter);

function queued(id: string, text = id): QueuedMessage {
  return { id, text, attachments: [] };
}

function chat(patch: Partial<Session> = {}): Session {
  return {
    ...newSession("claude", "/tmp/project"),
    queuedMessages: [queued("a", "first"), queued("b", "second")],
    queueStatus: "active",
    ...patch,
  };
}

describe("queuedHead", () => {
  it("returns the first queued follow-up", () => {
    expect(queuedHead(chat())?.id).toBe("a");
    expect(queuedHead(chat({ queuedMessages: undefined }))).toBeUndefined();
  });
});

describe("isEditingQueuedHead", () => {
  it("is true only when the head row is the one being edited", () => {
    expect(isEditingQueuedHead(chat())).toBe(false);
    expect(isEditingQueuedHead(chat({ editingQueuedMessageId: "a" }))).toBe(
      true,
    );
    expect(isEditingQueuedHead(chat({ editingQueuedMessageId: "b" }))).toBe(
      false,
    );
  });
});

describe("canDispatchQueuedHead", () => {
  it("dispatches an idle session with a queued head", () => {
    expect(canDispatchQueuedHead(chat())).toBe(true);
  });

  it("holds while the session is busy, paused, or resuming", () => {
    expect(canDispatchQueuedHead(chat({ busy: true }))).toBe(false);
    expect(canDispatchQueuedHead(chat({ queueStatus: "paused" }))).toBe(false);
    expect(canDispatchQueuedHead(chat({ queueStatus: "resuming" }))).toBe(
      false,
    );
  });

  it("holds while the last turn is stopped at a usage limit", () => {
    expect(
      canDispatchQueuedHead(chat({ usageLimit: { resetsAt: 1_000 } })),
    ).toBe(false);
  });

  it("holds only when the head item is being edited", () => {
    expect(canDispatchQueuedHead(chat({ editingQueuedMessageId: "a" }))).toBe(
      false,
    );
    expect(canDispatchQueuedHead(chat({ editingQueuedMessageId: "b" }))).toBe(
      true,
    );
  });

  it("does not dispatch during a preparing handoff", () => {
    const preparing = appendPreparingHandoff(
      chat({ queuedMessages: [queued("a")] }),
      "claude",
      "cursor",
    );
    expect(canDispatchQueuedHead(preparing)).toBe(false);
  });

  it("does not dispatch an empty queue", () => {
    expect(canDispatchQueuedHead(chat({ queuedMessages: undefined }))).toBe(
      false,
    );
  });
});

describe("steerableQueuedMessage", () => {
  const ready = (queuedMessages?: QueuedMessage[]) =>
    chat({
      busy: true,
      turnReadiness: "ready",
      ...(queuedMessages ? { queuedMessages } : {}),
    });
  const row = (id: string): QueuedMessage => ({
    ...queued(id),
    monoMessageId: id as MessageId,
  });

  it("still takes follow-ups while an optional asynchronous question is open", () => {
    expect(
      steerableQueuedMessage(
        chat({
          busy: true,
          turnReadiness: "ready",
          pendingQuestion: { requestId: 1, questions: [], autoResolveAt: 1000 },
        }),
        T1,
      )?.id,
    ).toBe("a");
  });
  it("holds during startup and allows delivery once the busy turn is ready", () => {
    expect(steerableQueuedMessage(chat({ busy: true }), T1)).toBeUndefined();
    expect(steerableQueuedMessage(ready(), T1)?.id).toBe("a");
    expect(
      steerableQueuedMessage(chat({ turnReadiness: "ready" }), T1),
    ).toBeUndefined();
  });

  it.each([
    { queueStatus: "paused" as const },
    { queueStatus: "resuming" as const },
    { editingQueuedMessageId: "a" },
    { worktreePreparing: true },
    { worktreeRemoved: true },
    { usageLimit: { resetsAt: 1000 } },
    { pendingQuestion: { requestId: 1, questions: [] } },
  ])("holds an unavailable session: %j", (patch) => {
    expect(
      steerableQueuedMessage(
        chat({ busy: true, turnReadiness: "ready", ...patch }),
        T1,
      ),
    ).toBeUndefined();
  });

  // PR I5 review r2 m4. Defect: an item with no typed turn (a Mono row, a CLI
  // follow-up, an item saved before typedInTurn) never steers a running turn.
  it.each<[string, QueuedMessage]>([
    ["a Mono row", row("m_rowrowrowrow")],
    ["a CLI follow-up", { ...queued("cli"), appRequestId: "request-1" }],
    ["an item saved before typedInTurn", queued("legacy")],
  ])("steers %s into whichever turn runs", (_kind, message) => {
    expect(steerableQueuedMessage(ready([message]), T1)).toBe(message);
  });

  it("steers a message the user typed only into the turn it was typed in", () => {
    const typed = { ...queued("typed"), typedInTurn: T1 };
    expect(steerableQueuedMessage(ready([typed]), T1)).toBe(typed);
    expect(steerableQueuedMessage(ready([typed]), T2)).toBeUndefined();
    expect(steerableQueuedMessage(ready([typed]), undefined)).toBeUndefined();
  });

  it.each<[string, QueuedMessage]>([
    ["a Mono row", { ...row("a"), waitForIdle: true }],
    [
      "a completion report",
      {
        ...queued("a"),
        monoSessionCompletion: {
          sessionId: "worker",
          title: "Worker",
          status: "completed",
        },
      },
    ],
  ])(
    "steers the user's message past %s that waits for the turn end",
    (_kind, head) => {
      const session = ready([head, queued("b")]);
      expect(steerableQueuedMessage(session, T1)?.id).toBe("b");
      expect(session.queuedMessages?.map((message) => message.id)).toEqual([
        "a",
        "b",
      ]);
    },
  );

  // PR I5 review r2 m5. Defect: the user's newer text reaches the agent
  // before their older message, which then runs after it; a correction such
  // as "don't deploy" lands before the message it corrects.
  it.each<Partial<QueuedMessage>>([
    { typedInTurn: T1 },
    { waitForIdle: true, missed: true },
  ])(
    "keeps the user's later messages behind their own message that missed its turn, but lets a Mono row pass: %j",
    (patch) => {
      const session = ready([
        { ...queued("a"), ...patch },
        { ...queued("b", "actually, don't deploy"), typedInTurn: T2 },
        queued("c"),
        row("m_rowrowrowrow"),
      ]);
      expect(steerableQueuedMessage(session, T2)?.id).toBe("m_rowrowrowrow");
    },
  );

  // PR I5b review r1 M1. Defect: one reply, continue or cleared steer (✕)
  // queued for the turn end holds every later message, so "stop, wrong
  // branch" reaches the agent only after the work it was typed to stop.
  it.each<Partial<QueuedMessage>>([
    { waitForIdle: true },
    {
      waitForIdle: true,
      link: { intent: "reply", replyTo: "m_aaaaaaaaaaaa", quote: "Which DB?" },
    },
    { intent: "plan" },
    { intent: "orchestrate" },
    { text: "/operator list sessions" },
  ])(
    "steers the user's later message past their own message that only waits for the turn end: %j",
    (patch) => {
      const session = ready([
        { ...queued("a"), ...patch },
        { ...queued("b", "stop, wrong branch"), typedInTurn: T1 },
      ]);
      expect(steerableQueuedMessage(session, T1)?.id).toBe("b");
    },
  );

  it("keeps the order among messages that can steer", () => {
    const session = ready([
      { ...row("a"), waitForIdle: true },
      queued("b"),
      queued("c"),
    ]);
    expect(steerableQueuedMessage(session, T1)?.id).toBe("b");
    expect(
      steerableQueuedMessage(dequeueQueuedMessage(session, "b"), T1)?.id,
    ).toBe("c");
  });
});

describe("nextQueuedSend", () => {
  it("opens an idle session's head, and steers a busy Mono's message into the turn this window runs for it", () => {
    const typed = { ...queued("typed"), typedInTurn: T1 };
    const idle = chat({ queuedMessages: [typed] });
    expect(nextQueuedSend(idle, runningTurnMap(), false)).toBe(typed);
    const busy = chat({
      busy: true,
      turnReadiness: "ready",
      queuedMessages: [typed],
    });
    expect(nextQueuedSend(busy, runningTurnMap([[busy.id, T1]]), true)).toBe(typed);
    expect(
      nextQueuedSend(busy, runningTurnMap([[busy.id, T1]]), false),
    ).toBeUndefined();
    expect(
      nextQueuedSend(busy, runningTurnMap([["other", T1]]), true),
    ).toBeUndefined();
  });
});

describe("queue pause reason", () => {
  const restarted = chat({
    queuedMessages: [{ ...queued("a"), pausedBy: "quit" }],
    queueStatus: "paused",
  });

  it("is shown for a queue that MonoCode paused when it quit", () => {
    expect(queuePauseNotice(restarted)).toBe("Paused when MonoCode quit");
  });

  it("goes away with the last message of that queue", () => {
    expect(queuePauseNotice(dequeueQueuedMessage(restarted, "a"))).toBe(
      undefined,
    );
  });

  it("is not shown when the user stops the turn", () => {
    const paused = pauseQueue({ ...restarted, busy: true });
    expect(paused.queueStatus).toBe("paused");
    expect(queuePauseNotice(paused)).toBeUndefined();
  });

  it("is not shown once a message fails to send", () => {
    expect(
      queuePauseNotice(failQueuedMessage(restarted, "a", "Offline")),
    ).toBeUndefined();
  });
});

describe("wakesQueue", () => {
  it("wakes a Mono's queue that the quit paused, never a project session's", () => {
    const restarted = chat({
      queuedMessages: [{ ...queued("a"), pausedBy: "quit" }],
      queueStatus: "paused",
    });
    expect(wakesQueue(restarted, true)).toBe(true);
    expect(wakesQueue(restarted, false)).toBe(false);
  });

  it("wakes a Mono after a failed turn, but not one stopped at a usage limit", () => {
    const [failed, limited] = [
      "Provider returned 500",
      "You have hit your usage limit.",
    ].map((message) =>
      applyHarnessEvent(
        { ...chat(), busy: true },
        { type: "session.error", message },
      ),
    );
    expect(wakesQueue(failed, true)).toBe(true);
    expect(limited).toMatchObject({
      busy: false,
      queueStatus: "paused",
      queuePause: "error",
    });
    expect(wakesQueue(limited, true)).toBe(false);
  });
});

describe("dequeueQueuedMessage", () => {
  it("drops the id and clears queue state when the last item goes", () => {
    const one = chat({ queuedMessages: [queued("a")], queueStatus: "active" });
    expect(dequeueQueuedMessage(one, "a")).toMatchObject({
      queuedMessages: undefined,
      queueStatus: undefined,
    });
  });

  it("keeps editing another row after the head is sent", () => {
    const next = dequeueQueuedMessage(
      chat({ editingQueuedMessageId: "b" }),
      "a",
    );
    expect(next.queuedMessages?.map((message) => message.id)).toEqual(["b"]);
    expect(next.editingQueuedMessageId).toBe("b");
    expect(next.queueStatus).toBe("active");
  });
});

describe("queuedMessageForSubmit", () => {
  it("only auto-dispatches the idle head", () => {
    expect(queuedMessageForSubmit(chat(), "a", "dispatch")?.id).toBe("a");
    expect(queuedMessageForSubmit(chat(), "b", "dispatch")).toBeUndefined();
    expect(
      queuedMessageForSubmit(chat({ busy: true }), "a", "dispatch"),
    ).toBeUndefined();
  });

  it("lets Steer target any remaining row, including while a turn runs or the queue is paused", () => {
    expect(
      queuedMessageForSubmit(
        chat({ busy: true, turnReadiness: "ready" }),
        "b",
        "steer",
      )?.id,
    ).toBe("b");
    expect(
      queuedMessageForSubmit(chat({ queueStatus: "paused" }), "a", "steer")?.id,
    ).toBe("a");
    expect(queuedMessageForSubmit(chat(), "missing", "steer")).toBeUndefined();
  });

  it("never resolves a row that waits for an idle turn for Steer", () => {
    const session = chat({
      queuedMessages: [{ ...queued("a"), waitForIdle: true }, queued("b")],
    });
    expect(
      queuedMessageForSubmit({ ...session, busy: true }, "a", "steer"),
    ).toBeUndefined();
    expect(queuedMessageForSubmit(session, "a", "dispatch")?.id).toBe("a");
  });
});
