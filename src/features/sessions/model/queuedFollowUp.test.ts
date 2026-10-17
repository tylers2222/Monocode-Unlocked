import { describe, expect, it, vi } from "vitest";
import { TurnNotReadyError } from "../../../integrations/harness/core/types";
import {
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
import { planUserSend } from "./messageIntent";
import { dequeueQueuedMessage, nextQueuedSend } from "./messageQueue";
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
  const turns = new Map([[session.id, T1]]);
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
  turns: Map<string, TurnId>,
  id: string,
  text: string,
): QueuedMessage {
  const item = typedMonoMessage({
    session: chat,
    turns,
    origin: {},
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
      turns: new Map([[session.id, T1]]),
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
    const turns = new Map([[busy.id, T1]]);
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
    ).toEqual({ ...base("b"), waitForIdle: true });
  });

  // PR I5 review r2 m4, m6. Defects: every message typed into a busy Mono
  // chat is refused at delivery; or it reaches the provider without its
  // whole-turn line, or its bubble misses the link or the turn it joined.
  it("is steered into its running turn with the whole-turn line, and its bubble records the link and the turn", async () => {
    let chat = t1Running();
    const turns = new Map([[chat.id, T1]]);
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
    const turns = new Map([[chat.id, T1]]);
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
    const turns = new Map([[running.id, T1]]);
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
