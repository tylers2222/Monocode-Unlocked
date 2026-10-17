import { describe, expect, it, vi } from "vitest";
import { TurnNotReadyError } from "../../../integrations/harness/core/types";
import {
  applyHarnessEvent,
  stopStreaming,
  appendUser,
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
import { planUserSend, type SendOrigin } from "./messageIntent";
import {
  dequeueQueuedMessage,
  failQueuedMessage,
  missedTurn,
  nextQueuedSend,
  resumeQueuedMessages,
  runningTurnMap,
  userTextHeld,
  type RunningTurnMap,
  type RunningTurns,
} from "./messageQueue";
import {
  deliverQueuedFollowUp,
  planQueuedSteer,
  typedMonoMessage,
} from "./queuedFollowUp";
import {
  newSession,
  WHOLE_TURN,
  type QueuedMessage,
  type Session,
  type TargetLink,
} from "./session";
import { sanitizeSessionForPersist } from "../data/sessionStore";

registerHarness({ id: "codex", live: true } as unknown as HarnessAdapter);

const T1 = "turn-1" as TurnId;
const T2 = "turn-2" as TurnId;
const T3 = "turn-3" as TurnId;
const T4 = "turn-4" as TurnId;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
}

function delivery() {
  let session: Session = {
    ...newSession("codex", "/tmp"),
    busy: true,
    turnReadiness: "ready",
    queueStatus: "active",
    queuedMessages: [
      { id: "first", text: "Use TypeScript", attachments: [], typedInTurn: T1 },
      {
        id: "second",
        text: "Also add a README",
        attachments: [],
        typedInTurn: T1,
      },
    ],
  };
  const turns = runningTurnMap([[session.id, T1]]);
  let currentTurn = true;
  const message = session.queuedMessages![0];
  const options = {
    message,
    joined: { block: { turnId: T1 }, text: message.text },
    turns,
    current: () => session,
    isCurrentTurn: () => currentTurn,
    prepare: vi.fn(async (text: string) => text),
    steer: vi.fn(async (_text: string) => {}),
    delivered: vi.fn((_block: { turnId?: TurnId }) => {
      session = dequeueQueuedMessage(session, message.id);
    }),
    deferred: vi.fn(() => {
      session = { ...session, turnReadiness: "deferred" };
    }),
    failed: vi.fn((error: string) => {
      session = {
        ...session,
        queueStatus: "paused",
        queuedMessages: session.queuedMessages?.map((entry) =>
          entry.id === message.id ? { ...entry, error } : entry,
        ),
      };
    }),
  };
  return {
    options,
    turns,
    session: () => session,
    patch: (patch: Partial<Session>) => {
      session = { ...session, ...patch };
    },
    stop: () => {
      currentTurn = false;
      session = { ...session, busy: false };
    },
  };
}

/** A Mono chat running turn T1, opened by the user's "Build it". */
function t1Running(turnReadiness: Session["turnReadiness"] = "ready"): Session {
  return {
    ...appendUser(newSession("codex", "/tmp"), "Build it", [], { turnId: T1 }),
    turnReadiness,
  };
}

const base = (id: string, text = id) => ({ id, text, attachments: [] });

/** The item text the user types into a Mono chat becomes, as App.tsx enqueues it. */
function typed(
  chat: Session,
  turns: RunningTurns,
  id: string,
  text: string,
  origin: SendOrigin = {},
): QueuedMessage {
  const item = typedMonoMessage({
    session: chat,
    turns,
    origin,
    message: base(id, text),
  });
  if (!item) throw new Error("the Mono enqueue refused the message");
  return item;
}

const row = (id: string): QueuedMessage => ({
  ...base(id, "<mono_message>please review PR 12</mono_message>"),
  waitForIdle: true,
  monoMessageId: id as MessageId,
});

describe("pending Mono follow-up delivery", () => {
  it("waits for cold startup and never lets a later message overtake the head", async () => {
    const d = delivery();
    d.patch({ turnReadiness: "starting" });
    expect(await deliverQueuedFollowUp(d.options)).toBe(false);
    expect(d.options.prepare).not.toHaveBeenCalled();
    d.patch({ turnReadiness: "ready" });
    expect(
      await deliverQueuedFollowUp({
        ...d.options,
        message: d.session().queuedMessages![1],
      }),
    ).toBe(false);
    expect(d.options.steer).not.toHaveBeenCalled();
    expect(await deliverQueuedFollowUp(d.options)).toBe(true);
    expect(d.options.steer).toHaveBeenCalledWith("Use TypeScript");
    expect(d.session().queuedMessages?.map((message) => message.id)).toEqual([
      "second",
    ]);
  });

  it("steers the user's message past a Mono message that waits for the turn end", async () => {
    const waiting = row("m_waitingwaiti");
    const user = { ...base("user", "Use TypeScript"), typedInTurn: T1 };
    let session: Session = {
      ...newSession("codex", "/tmp"),
      busy: true,
      turnReadiness: "ready",
      queueStatus: "active",
      queuedMessages: [waiting, user],
    };
    const steer = vi.fn(async (_text: string) => {});
    const delivered = await deliverQueuedFollowUp({
      message: user,
      joined: { block: { turnId: T1 }, text: user.text },
      turns: runningTurnMap([[session.id, T1]]),
      current: () => session,
      isCurrentTurn: () => true,
      prepare: async (text) => text,
      steer,
      delivered: () => {
        session = dequeueQueuedMessage(session, user.id);
      },
      deferred: vi.fn(),
      failed: vi.fn(),
    });
    expect(delivered).toBe(true);
    expect(steer).toHaveBeenCalledWith("Use TypeScript");
    expect(session.queuedMessages).toEqual([waiting]);
  });

  it("keeps the message pending until the provider acknowledges delivery", async () => {
    const d = delivery();
    const accepted = deferred<void>();
    d.options.steer.mockImplementation(() => accepted.promise);
    const sending = deliverQueuedFollowUp(d.options);
    await Promise.resolve();
    expect(d.session().queuedMessages?.[0]).toBe(d.options.message);
    expect(d.options.delivered).not.toHaveBeenCalled();
    accepted.resolve();
    expect(await sending).toBe(true);
    expect(d.session().busy).toBe(true);
  });

  it.each(["edit", "delete", "stop", "question", "switch"])(
    "honors a %s while attachments or prompt context are being prepared",
    async (action) => {
      const d = delivery();
      const prepared = deferred<string>();
      d.options.prepare.mockImplementation(() => prepared.promise);
      const sending = deliverQueuedFollowUp(d.options);
      if (action === "edit")
        d.patch({ queuedMessages: [{ ...d.options.message, text: "Edited" }] });
      if (action === "delete") d.patch({ queuedMessages: [] });
      if (action === "stop") d.stop();
      if (action === "question")
        d.patch({ pendingQuestion: { requestId: 1, questions: [] } });
      if (action === "switch")
        d.patch({
          pendingSwitch: {
            from: "codex",
            fromModel: "model",
            fromSettings: {},
          },
        });
      prepared.resolve("prepared");
      expect(await sending).toBe(false);
      expect(d.options.steer).not.toHaveBeenCalled();
      expect(d.options.delivered).not.toHaveBeenCalled();
    },
  );

  // PR I5 review r2 n3. Defect: the re-check after preparation reuses the
  // turn it checked first, so a message typed in a turn that another turn
  // replaced during preparation steers the new turn.
  it("refuses the message when another turn replaced its turn during preparation", async () => {
    const d = delivery();
    const prepared = deferred<string>();
    d.options.prepare.mockImplementation(() => prepared.promise);
    const sending = deliverQueuedFollowUp(d.options);
    d.turns.set(d.session().id, T2);
    prepared.resolve("prepared");
    expect(await sending).toBe(false);
    expect(d.options.steer).not.toHaveBeenCalled();
    expect(d.options.delivered).not.toHaveBeenCalled();
  });

  it.each(["preparation", "provider"])(
    "retains a failed %s without settling the original turn",
    async (failure) => {
      const d = delivery();
      (failure === "preparation"
        ? d.options.prepare
        : d.options.steer
      ).mockRejectedValueOnce(new Error("Connection lost"));
      expect(await deliverQueuedFollowUp(d.options)).toBe(false);
      expect(d.session()).toMatchObject({ busy: true, queueStatus: "paused" });
      expect(d.session().queuedMessages?.[0]).toMatchObject({
        id: "first",
        error: "Connection lost",
      });
      expect(d.options.delivered).not.toHaveBeenCalled();
    },
  );

  it("waits for a new turn when the provider finished during delivery", async () => {
    const d = delivery();
    d.options.steer.mockRejectedValueOnce(new TurnNotReadyError("Turn ended"));
    expect(await deliverQueuedFollowUp(d.options)).toBe(false);
    expect(d.session()).toMatchObject({
      turnReadiness: "deferred",
      queueStatus: "active",
    });
    expect(d.session().queuedMessages?.[0]).toBe(d.options.message);
    expect(d.options.failed).not.toHaveBeenCalled();
  });
});

describe("a message the user types into a Mono chat", () => {
  // PR I5 review r2 m6 (the M1 forward). Defects: the item loses the turn it
  // was typed in, so the pump steers it into a turn another sender opened; or
  // an idle chat whose ended turn is still known gives the item that turn.
  it("records the running turn while the chat is busy, and waits for a turn end while it is idle", () => {
    const busy = t1Running();
    const turns = runningTurnMap([[busy.id, T1]]);
    expect(
      typedMonoMessage({
        session: busy,
        turns,
        origin: {},
        message: base("a"),
      }),
    ).toEqual({ ...base("a"), typedInTurn: T1 });
    expect(
      typedMonoMessage({
        session: { ...busy, busy: false },
        turns,
        origin: {},
        message: base("b"),
      }),
    ).toEqual({ ...base("b"), waitForIdle: true, missed: true });
  });

  // PR I5 review r2 m4, m6. Defects: every message typed into a busy Mono
  // chat is refused at delivery; or it reaches the provider without its
  // whole-turn line, or its bubble misses the link or the turn it joined.
  it("is steered into its running turn with the whole-turn line, and its bubble records the link and the turn", async () => {
    let chat = t1Running();
    const turns = runningTurnMap([[chat.id, T1]]);
    chat = enqueueMonoMessage(chat, typed(chat, turns, "a", "use staging"));
    const message = nextQueuedSend(chat, turns, true)!;
    expect(message.id).toBe("a");
    const joined = planQueuedSteer({
      session: chat,
      turns,
      message,
      origin: {},
      text: message.text,
    });
    if (joined?.kind !== "join") throw new Error(`planned ${joined?.kind}`);
    const provider: string[] = [];
    const delivered = await deliverQueuedFollowUp({
      message,
      joined,
      turns,
      current: () => chat,
      isCurrentTurn: () => true,
      prepare: async (text) => text,
      steer: async (text) => {
        provider.push(text);
      },
      delivered: (block) => {
        chat = acknowledgeMonoMessage(chat, message, {
          mode: "follow-up",
          cards: block,
        });
      },
      deferred: vi.fn(),
      failed: vi.fn(),
    });
    expect(delivered).toBe(true);
    expect(provider).toEqual(["[steer → whole turn]\nuse staging"]);
    expect(chat.blocks.find((block) => block.id === "a")).toMatchObject({
      text: "use staging",
      link: WHOLE_TURN,
      turnId: T1,
    });
    expect(chat.queuedMessages).toBeUndefined();
  });

  // PR I5 review r2 m5. Defect: the user's newer message is steered into a
  // turn before their older one that missed its turn, which then runs after
  // it, so "don't deploy" reaches the agent before "deploy"; and the older
  // message shows no "Sends when this turn ends" while it waits.
  it("keeps the user's messages in typed order after one missed its turn, and says each waits", () => {
    let chat = enqueueMonoMessage(t1Running("starting"), row("m_wwwwwwwwwwww"));
    const turns = runningTurnMap([[chat.id, T1]]);
    chat = enqueueMonoMessage(
      chat,
      typed(chat, turns, "a", "deploy to staging"),
    );

    // T1 ends before it can take the steer; the Mono row opens T2.
    chat = stopStreaming(chat);
    turns.delete(chat.id);
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("m_wwwwwwwwwwww");
    chat = acknowledgeMonoMessage(chat, chat.queuedMessages![0], {
      mode: "new-turn",
      cards: { turnId: T2 },
    });
    chat = { ...chat, turnReadiness: "ready" };
    turns.set(chat.id, T2);
    chat = enqueueMonoMessage(
      chat,
      typed(chat, turns, "b", "actually, don't deploy"),
    );
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
    expect(monoMessageDeliveries(chat).get("a")).toEqual({ status: "waiting" });
    expect(monoMessageDeliveries(chat).get("b")).toEqual({ status: "waiting" });

    // T2 ends; the older message opens T3 first, with no line.
    chat = stopStreaming(chat);
    turns.delete(chat.id);
    const older = nextQueuedSend(chat, turns, true)!;
    expect(
      planUserSend({
        origin: {},
        item: older,
        text: older.text,
        route: { opens: T3 },
      }),
    ).toEqual({
      kind: "open",
      block: { turnId: T3 },
      text: "deploy to staging",
    });
    chat = acknowledgeMonoMessage(chat, older, {
      mode: "new-turn",
      cards: { turnId: T3 },
    });
    chat = { ...chat, turnReadiness: "ready" };
    turns.set(chat.id, T3);
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
    expect(monoMessageDeliveries(chat).get("b")).toEqual({ status: "waiting" });

    // T3 ends; the correction opens T4 after the message it corrects.
    chat = stopStreaming(chat);
    turns.delete(chat.id);
    const newer = nextQueuedSend(chat, turns, true)!;
    expect(
      planUserSend({
        origin: {},
        item: newer,
        text: newer.text,
        route: { opens: T4 },
      }),
    ).toEqual({
      kind: "open",
      block: { turnId: T4 },
      text: "actually, don't deploy",
    });
  });
});

describe("a queued item's own link", () => {
  // PR I5 review r2 m4. Defect: a queued picked steer joins as a whole-turn
  // steer, because the plan reads only the submit's link.
  it("steers a queued picked steer with its own link and line", () => {
    const running = t1Running();
    const link: TargetLink = {
      intent: "steer",
      replyTo: running.blocks[0].id,
      quote: "Build it",
    };
    const turns = runningTurnMap([[running.id, T1]]);
    const item = typedMonoMessage({
      session: running,
      turns,
      origin: { link },
      message: base("a", "use the staging DB"),
    });
    expect(item).toEqual({
      ...base("a", "use the staging DB"),
      link,
      typedInTurn: T1,
    });
    const chat = enqueueMonoMessage(running, item!);
    const message = nextQueuedSend(chat, turns, true)!;
    expect(
      planQueuedSteer({
        session: chat,
        turns,
        message,
        origin: {},
        text: message.text,
      }),
    ).toEqual({
      kind: "join",
      block: { turnId: T1, link },
      text: '[steer → "Build it"]\nuse the staging DB',
    });
  });

  // PR I5 review r2 m4. Defect: a queued reply opens its turn with no line.
  it("opens a turn with a queued reply's link and line", () => {
    const link: TargetLink = {
      intent: "reply",
      replyTo: "m_aaaaaaaaaaaa",
      quote: "Which DB?",
    };
    const item = { ...base("r", "staging"), link, waitForIdle: true };
    expect(
      planUserSend({ origin: {}, item, text: item.text, route: { opens: T2 } }),
    ).toEqual({
      kind: "open",
      block: { turnId: T2, link },
      text: '[reply → m_aaaaaaaaaaaa · "Which DB?"]\nstaging',
    });
  });
});

/** T1 ran; the user typed "deploy" in it; T1 ended; a Mono row opened T2. */
function missedInT1(): { chat: Session; turns: RunningTurnMap } {
  let chat = enqueueMonoMessage(t1Running("starting"), row("m_wwwwwwwwwwww"));
  const turns = runningTurnMap([[chat.id, T1]]);
  chat = enqueueMonoMessage(chat, typed(chat, turns, "a", "deploy"));
  chat = stopStreaming(chat);
  turns.delete(chat.id);
  chat = acknowledgeMonoMessage(chat, chat.queuedMessages![0], {
    mode: "new-turn",
    cards: { turnId: T2 },
  });
  chat = { ...chat, turnReadiness: "ready" };
  turns.set(chat.id, T2);
  return { chat, turns };
}

describe("the user's message after one that waits for the turn end", () => {
  // PR I5b review r1 M1. Defect: one reply, continue or cleared steer (✕)
  // queued for the turn end holds every later message of that turn, so
  // "stop, wrong branch" reaches the agent only after the work it was typed
  // to stop, and the composer says "Steering" for text the queue holds.
  it.each<[string, (chat: Session) => SendOrigin]>([
    ["a cleared steer (✕)", () => ({ waitForIdle: true })],
    [
      "a picked reply",
      (chat) => ({
        link: { intent: "reply", replyTo: chat.blocks[0].id, quote: "Build it" },
      }),
    ],
  ])(
    "after %s, the user's next message still steers the running turn and only the first one waits",
    (_kind, origin) => {
      let chat = t1Running();
      const turns = runningTurnMap([[chat.id, T1]]);
      chat = enqueueMonoMessage(
        chat,
        typed(chat, turns, "x", "after this, write docs", origin(chat)),
      );
      expect(userTextHeld(chat)).toBe(false);
      chat = enqueueMonoMessage(
        chat,
        typed(chat, turns, "s", "stop, wrong branch"),
      );
      expect(nextQueuedSend(chat, turns, true)?.id).toBe("s");
      expect(monoMessageDeliveries(chat).get("x")).toEqual({
        status: "waiting",
      });
      expect(monoMessageDeliveries(chat).get("s")).toEqual({
        status: "pending",
      });
    },
  );

  // PR I5b review r1 M1, idle case, and review r2 m1. Defect: a ✕ or an
  // /operator command typed while no turn runs is taken as a message that
  // missed its turn, so it holds the user's text typed in the next turn.
  it.each<[string, string, SendOrigin]>([
    ["a cleared steer (✕)", "after this, write docs", { waitForIdle: true }],
    ["an /operator command", "/operator list sessions", {}],
  ])("lets the user's text steer past %s typed while no turn ran", (_kind, text, origin) => {
    const turns = runningTurnMap();
    let chat: Session = {
      ...newSession("codex", "/tmp"),
      queuedMessages: [row("m_wwwwwwwwwwww")],
      queueStatus: "paused",
    };
    chat = enqueueMonoMessage(chat, typed(chat, turns, "x", text, origin));
    expect(userTextHeld(chat)).toBe(false);
    chat = acknowledgeMonoMessage(
      { ...chat, queueStatus: "active" },
      chat.queuedMessages![0],
      { mode: "new-turn", cards: { turnId: T2 } },
    );
    chat = { ...chat, turnReadiness: "ready" };
    turns.set(chat.id, T2);
    chat = enqueueMonoMessage(chat, typed(chat, turns, "s", "stop"));
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("s");
  });

  // PR I5b review r2 m1. Defect: an /operator command typed in T1 waits for
  // the turn end by choice, but T1's end marks it missed, so it holds the
  // user's text typed in T2 (decision 585).
  it("lets the user's text in the next turn steer past an /operator command typed in T1", () => {
    let chat = enqueueMonoMessage(t1Running(), row("m_wwwwwwwwwwww"));
    const turns = runningTurnMap([[chat.id, T1]]);
    chat = enqueueMonoMessage(
      chat,
      typed(chat, turns, "x", "/operator list sessions"),
    );
    chat = stopStreaming(chat);
    turns.delete(chat.id);
    expect(userTextHeld(chat)).toBe(false);
    chat = acknowledgeMonoMessage(chat, chat.queuedMessages![0], {
      mode: "new-turn",
      cards: { turnId: T2 },
    });
    chat = { ...chat, turnReadiness: "ready" };
    turns.set(chat.id, T2);
    chat = enqueueMonoMessage(
      chat,
      typed(chat, turns, "s", "stop, wrong branch"),
    );
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("s");
    expect(monoMessageDeliveries(chat).get("x")).toEqual({ status: "waiting" });
  });

  // PR I6 review r1 M2. Defect: the composer says "Steering" for text the
  // pump holds behind the user's message that missed its turn, or says it
  // waits while the text would steer.
  it("userTextHeld says the user's next message waits exactly while one of theirs missed its turn", () => {
    const running = enqueueMonoMessage(t1Running(), row("m_wwwwwwwwwwww"));
    const t1 = runningTurnMap([[running.id, T1]]);
    expect(
      userTextHeld(
        enqueueMonoMessage(running, typed(running, t1, "a", "deploy")),
      ),
    ).toBe(false);

    const { turns, ...missed } = missedInT1();
    let chat = missed.chat;
    expect(userTextHeld(chat)).toBe(true);
    chat = enqueueMonoMessage(chat, typed(chat, turns, "b", "don't deploy"));
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
    expect(monoMessageDeliveries(chat).get("b")).toEqual({ status: "waiting" });
  });

  // PR I5b review r2 m2. Defect: while a Mono chat compacts (busy, no turn
  // in this window) userTextHeld says false, so the composer says
  // "Steering" for text that waits for the compaction to end. PR I5b review
  // r3 n3: or it says held while a turn starts or is deferred, though the
  // text typed then is steered into that turn.
  it.each(["starting", "deferred"] as const)(
    "userTextHeld says the user's next message waits while the chat compacts, and not while a turn is %s",
    (turnReadiness) => {
      const turns = runningTurnMap();
      const compacting = applyHarnessEvent(
        { ...stopStreaming(t1Running()), busy: true },
        { type: "status", text: "Compacting context…" },
      );
      expect(typed(compacting, turns, "b", "what changed?")).toMatchObject({
        missed: true,
      });
      expect(userTextHeld(compacting)).toBe(true);

      const opening = t1Running(turnReadiness);
      const t1 = runningTurnMap([[opening.id, T1]]);
      expect(typed(opening, t1, "b", "what changed?")).toMatchObject({
        typedInTurn: T1,
      });
      expect(userTextHeld(opening)).toBe(false);
    },
  );

  // PR I5b review r1 n2. Defect: the steer plan joins a message the pump
  // would not steer now into the running turn: a message that missed its
  // turn, or a ✕ message, joins it as a whole-turn steer.
  it("plans no steer for a message the pump does not steer now", () => {
    const { chat, turns } = missedInT1();
    const missed = chat.queuedMessages!.find((message) => message.id === "a")!;
    expect(
      planQueuedSteer({
        session: chat,
        turns,
        message: missed,
        origin: {},
        text: missed.text,
      }),
    ).toBeUndefined();

    let running = t1Running();
    const t1 = runningTurnMap([[running.id, T1]]);
    running = enqueueMonoMessage(
      running,
      typed(running, t1, "x", "after this, write docs", { waitForIdle: true }),
    );
    const cleared = running.queuedMessages![0];
    expect(
      planQueuedSteer({
        session: running,
        turns: t1,
        message: cleared,
        origin: {},
        text: cleared.text,
      }),
    ).toBeUndefined();
  });
});

describe("typed order against the other queue items", () => {
  // PR I5b review r1, section 2. Defects: a Mono steer row or a CLI follow-up
  // waits behind the user's held message; or a held message lets the user's
  // later text pass.
  it("lets a Mono steer row and a CLI follow-up pass the user's held messages, and holds the user's text", () => {
    const { turns, ...missed } = missedInT1();
    let chat = missed.chat;
    chat = enqueueMonoMessage(chat, typed(chat, turns, "b", "don't deploy"));
    chat = {
      ...chat,
      queuedMessages: [
        ...chat.queuedMessages!,
        {
          ...base("m_ssssssssssss", "<mono_message>stop</mono_message>"),
          monoMessageId: "m_ssssssssssss" as MessageId,
        },
        { ...base("c", "from the CLI"), appRequestId: "req-c" },
      ],
    };
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("m_ssssssssssss");
    chat = dequeueQueuedMessage(chat, "m_ssssssssssss");
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("c");
    chat = dequeueQueuedMessage(chat, "c");
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
  });

  // Defect: a Mono notice or a completion report waiting for the turn end
  // holds the user's text typed in the running turn.
  it("steers the user's text past a Mono notice and a completion report", () => {
    let chat = t1Running();
    const turns = runningTurnMap([[chat.id, T1]]);
    chat = {
      ...chat,
      queuedMessages: [
        row("m_nnnnnnnnnnnn"),
        {
          ...base("r", "report"),
          monoSessionCompletion: {
            sessionId: "worker",
            title: "Worker",
            status: "completed",
          },
        },
      ],
    };
    chat = enqueueMonoMessage(chat, typed(chat, turns, "b", "use staging"));
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("b");
    expect(monoMessageDeliveries(chat).get("b")).toEqual({ status: "pending" });
  });

  // PR I5b review r1 (decision 578's idle case). Defect: a message typed
  // while no turn runs is overtaken by the user's text typed in the turn
  // another message opens.
  it("keeps a message typed while no turn ran ahead of the user's text typed in the next turn", () => {
    const turns = runningTurnMap();
    let chat: Session = {
      ...newSession("codex", "/tmp"),
      queuedMessages: [row("m_wwwwwwwwwwww")],
    };
    chat = failQueuedMessage(chat, "m_wwwwwwwwwwww", "Offline");
    chat = enqueueMonoMessage(
      chat,
      typed(chat, turns, "b", "also check the docs"),
    );
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
    chat = {
      ...chat,
      queueStatus: "active",
      queuedMessages: resumeQueuedMessages(chat.queuedMessages),
    };
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("m_wwwwwwwwwwww");
    chat = acknowledgeMonoMessage(chat, chat.queuedMessages![0], {
      mode: "new-turn",
      cards: { turnId: T2 },
    });
    chat = { ...chat, turnReadiness: "ready" };
    turns.set(chat.id, T2);
    chat = enqueueMonoMessage(chat, typed(chat, turns, "c", "skip the docs"));
    expect(nextQueuedSend(chat, turns, true)).toBeUndefined();
    expect(monoMessageDeliveries(chat).get("b")).toEqual({ status: "waiting" });
    expect(monoMessageDeliveries(chat).get("c")).toEqual({ status: "waiting" });
    chat = stopStreaming(chat);
    turns.delete(chat.id);
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("b");
  });

  // Defect: a message typed while the chat is busy with no turn id (manual
  // compaction) is stuck, or steers a turn.
  it("lets a message typed during a compaction open the next turn", () => {
    const turns = runningTurnMap();
    let chat: Session = { ...newSession("codex", "/tmp"), busy: true };
    chat = enqueueMonoMessage(chat, typed(chat, turns, "b", "what changed?"));
    expect(chat.queuedMessages![0]).toMatchObject({ waitForIdle: true });
    chat = { ...chat, busy: false };
    expect(nextQueuedSend(chat, turns, true)?.id).toBe("b");
  });
});

// PR I5b review r1, section 2: seeded walks over the real queue functions,
// one App.tsx action per step. Defects they catch: a message held forever;
// the user's messages reach the agent out of typed order (only a reply, a
// continue or a ✕ may be passed); a message joins a turn it was not typed in
// or loses its line; a Mono steer row is blocked; a bubble's label disagrees
// with the pump; userTextHeld disagrees with the pump.
describe("the Mono queue over seeded walks", () => {
  type Sent = { id: string; how: "open" | "join"; turn: TurnId; text: string };
  const steerRow = (id: string): QueuedMessage => ({
    ...base(id, "<mono_message>[steer → whole turn] stop</mono_message>"),
    monoMessageId: id as MessageId,
  });

  function rng(seed: number) {
    let s = seed >>> 0;
    return () => {
      s = (s * 1664525 + 1013904223) >>> 0;
      return s / 2 ** 32;
    };
  }

  /** What the pump steers into the running turn if nothing else happens. */
  function steersThisTurn(chat: Session, turns: RunningTurns): Set<string> {
    const ids = new Set<string>();
    for (
      let message = nextQueuedSend(chat, turns, true);
      message;
      message = nextQueuedSend(chat, turns, true)
    ) {
      ids.add(message.id);
      chat = dequeueQueuedMessage(chat, message.id);
    }
    return ids;
  }

  function walk(seed: number, steps: number) {
    const rand = rng(seed);
    const turns = runningTurnMap();
    let chat: Session = { ...newSession("codex", "/tmp"), id: `mono-${seed}` };
    let turnNo = 0;
    let n = 0;
    const typedOrder: string[] = [];
    const typedTurn = new Map<string, TurnId | undefined>();
    const waits = new Set<string>();
    const sent: Sent[] = [];

    const open = (message: QueuedMessage | undefined, text: string, id: string) => {
      const turn = `turn-${++turnNo}` as TurnId;
      const plan = planUserSend({
        origin: {},
        item: message,
        text,
        route: { opens: turn },
      });
      if (plan.kind !== "open") throw new Error(`open planned ${plan.kind}`);
      chat = message
        ? acknowledgeMonoMessage(chat, message, {
            mode: "new-turn",
            cards: plan.block,
          })
        : appendUser(chat, text, [], plan.block);
      chat = {
        ...chat,
        busy: true,
        turnReadiness: rand() < 0.7 ? "ready" : "starting",
      };
      turns.set(chat.id, turn);
      sent.push({ id, how: "open", turn, text: plan.text });
    };

    const pump = () => {
      const next = nextQueuedSend(chat, turns, true);
      if (!next) return false;
      if (!chat.busy) {
        open(next, next.text, next.id);
        return true;
      }
      const joined = planQueuedSteer({
        session: chat,
        turns,
        message: next,
        origin: {},
        text: next.text,
      });
      if (joined?.kind !== "join")
        throw new Error(`steer planned ${joined?.kind}`);
      chat = acknowledgeMonoMessage(chat, next, {
        mode: "follow-up",
        cards: joined.block,
      });
      sent.push({
        id: next.id,
        how: "join",
        turn: turns.get(chat.id)!,
        text: joined.text,
      });
      return true;
    };

    const typeOwn = (clear: boolean) => {
      const id = `u${++n}`;
      typedOrder.push(id);
      if (!chat.busy && !chat.queuedMessages?.length) {
        typedTurn.set(id, undefined);
        open(undefined, id, id);
        return;
      }
      if (clear) waits.add(id);
      typedTurn.set(id, chat.busy ? turns.get(chat.id) : undefined);
      chat = enqueueMonoMessage(
        chat,
        typed(chat, turns, id, id, clear ? { waitForIdle: true } : {}),
      );
    };

    const check = () => {
      const queue = chat.queuedMessages ?? [];
      const ready =
        chat.busy &&
        chat.turnReadiness === "ready" &&
        chat.queueStatus !== "paused";
      if (!ready) return;
      if (queue.some((message) => message.monoMessageId && !message.waitForIdle))
        expect(
          nextQueuedSend(chat, turns, true),
          `seed ${seed}: a Mono steer row is blocked`,
        ).toBeDefined();
      const steers = steersThisTurn(chat, turns);
      const labels = monoMessageDeliveries(chat);
      for (const message of queue)
        if (typedTurn.has(message.id))
          expect(labels.get(message.id), `seed ${seed}: label of ${message.id}`).toEqual({
            status: steers.has(message.id) ? "pending" : "waiting",
          });
      const probe = typed(chat, turns, "probe", "probe");
      expect(
        steersThisTurn(enqueueMonoMessage(chat, probe), turns).has("probe"),
        `seed ${seed}: userTextHeld`,
      ).toBe(!userTextHeld(chat));
    };

    for (let i = 0; i < steps; i++) {
      const r = rand();
      if (r < 0.25) typeOwn(false);
      else if (r < 0.32) typeOwn(true);
      else if (r < 0.38)
        chat = enqueueMonoMessage(chat, steerRow(`m_s${String(++n).padStart(10, "0")}`));
      else if (r < 0.44)
        chat = enqueueMonoMessage(chat, row(`m_n${String(++n).padStart(10, "0")}`));
      else if (r < 0.48)
        chat = {
          ...chat,
          queuedMessages: [
            ...(chat.queuedMessages ?? []),
            { ...base(`c${++n}`, "from the CLI"), appRequestId: `req-${n}` },
          ],
        };
      else if (r < 0.7) pump();
      else if (r < 0.8) {
        if (chat.busy) chat = { ...chat, turnReadiness: "ready" };
      } else if (r < 0.95) {
        if (chat.busy) {
          chat = stopStreaming(chat);
          turns.delete(chat.id);
        }
      } else {
        // A reload, as recordToSession loads the chat; then the user resumes.
        const saved = sanitizeSessionForPersist(chat);
        chat = {
          ...saved,
          busy: false,
          turnReadiness: undefined,
          queuedMessages: resumeQueuedMessages(
            missedTurn(saved.queuedMessages ?? []),
          ),
          queueStatus: "active",
        };
        turns.clear();
      }
      check();
    }

    for (let guard = 0; guard < 500; guard++) {
      if (!chat.busy && !chat.queuedMessages?.length) break;
      if (chat.busy) chat = { ...chat, turnReadiness: "ready" };
      check();
      if (!pump() && chat.busy) {
        chat = stopStreaming(chat);
        turns.delete(chat.id);
      }
    }
    return { chat, sent, typedOrder, typedTurn, waits };
  }

  it.each(Array.from({ length: 400 }, (_, i) => i + 1))(
    "seed %i drains its queue, keeps typed order, and agrees with the pump",
    (seed) => {
      const { chat, sent, typedOrder, typedTurn, waits } = walk(seed, 60);
      expect(chat.queuedMessages ?? [], "held forever").toEqual([]);
      const own = sent.filter((entry) => typedTurn.has(entry.id));
      const position = new Map(own.map((entry, index) => [entry.id, index]));
      typedOrder.forEach((earlier, i) => {
        for (const later of typedOrder.slice(i + 1))
          if (!waits.has(earlier))
            expect(
              position.get(earlier)! < position.get(later)!,
              `${later} reached the agent before ${earlier}`,
            ).toBe(true);
      });
      for (const entry of own)
        if (entry.how === "join") {
          expect(entry.turn, `${entry.id} joined another turn`).toBe(
            typedTurn.get(entry.id),
          );
          expect(entry.text).toBe(`[steer → whole turn]\n${entry.id}`);
        } else expect(entry.text).toBe(entry.id);
    },
  );
});
