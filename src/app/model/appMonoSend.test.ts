import { describe, expect, it } from "vitest";
import type { WhenBusy } from "../../features/agent-app/model/agentApp";
import {
  acknowledgeMonoMessage,
  enqueueMonoMessage,
  monoMessageDeliveries,
} from "../../features/monos/model/monoMessaging";
import { sanitizeSessionForPersist } from "../../features/sessions/data/sessionStore";
import {
  canDispatchQueuedHead,
  steerableQueuedMessage,
} from "../../features/sessions/model/messageQueue";
import {
  newSession,
  type QueuedMessage,
  type Session,
} from "../../features/sessions/model/session";
import { appendUser } from "../../integrations/harness/core/apply";
import {
  enqueueAppMonoMessage,
  MONO_BUSY_ERROR,
  MONO_CANNOT_STEER_ERROR,
  NO_MONO_CONVERSATION_ERROR,
  planAppMonoSend,
  type AppMonoSendPlan,
} from "./appMonoSend";

const text =
  '<mono_message from="MonoCrab" monoId="crab">\nReview the parser\n</mono_message>';
const requestId = "app-crab-chat-request-1";

function idle(): Session {
  return newSession("codex", "/home");
}

/** The provider has accepted the running turn, so it can take a steer. */
function busy(): Session {
  return { ...appendUser(idle(), "Fix the parser"), turnReady: true };
}

function plan(
  session: Session | undefined,
  whenBusy?: WhenBusy,
  options: { text?: string; steerable?: boolean } = {},
) {
  return planAppMonoSend({
    session,
    text: options.text ?? text,
    requestId,
    whenBusy,
    steerable: options.steerable ?? true,
  });
}

function enqueue(session: Session, whenBusy?: WhenBusy) {
  const planned = plan(session, whenBusy);
  expect(planned.kind).toBe("enqueue");
  const decision = planned as Extract<AppMonoSendPlan, { kind: "enqueue" }>;
  return { ...decision, next: enqueueAppMonoMessage(session, decision) };
}

describe("planAppMonoSend", () => {
  it("needs the Mono's conversation", () => {
    expect(plan(undefined)).toEqual({
      kind: "error",
      error: NO_MONO_CONVERSATION_ERROR,
    });
  });

  it.each([undefined, "steer", "followUp"] as const)(
    "starts a new turn in an idle Mono with an empty queue (whenBusy: %s)",
    (whenBusy) => {
      const session = idle();
      expect(plan(session, whenBusy)).toEqual({
        kind: "submit",
        sessionId: session.id,
      });
    },
  );

  it("refuses a busy Mono when the request has no whenBusy", () => {
    expect(plan(busy())).toEqual({ kind: "error", error: MONO_BUSY_ERROR });
  });

  it("queues a steer that the queue delivers once the provider is ready", () => {
    const session = busy();
    const { sessionId, message, delivery, next } = enqueue(session, "steer");
    expect(sessionId).toBe(session.id);
    expect(delivery).toBe("steered");
    expect(message).toEqual({
      id: expect.any(String),
      text,
      attachments: [],
      appRequestId: requestId,
    });
    expect(next.queuedMessages?.map((queued) => queued.id)).toEqual([
      message.id,
    ]);
    expect(steerableQueuedMessage(next)?.id).toBe(message.id);
    expect(
      steerableQueuedMessage({ ...next, turnReady: false }),
    ).toBeUndefined();
  });

  it("refuses a steer the busy Mono's provider cannot take, but not a follow-up", () => {
    expect(plan(busy(), "steer", { steerable: false })).toEqual({
      kind: "error",
      error: MONO_CANNOT_STEER_ERROR,
    });
    expect(plan(busy(), "followUp", { steerable: false })).toMatchObject({
      kind: "enqueue",
      delivery: "queued",
      message: { waitForIdle: true },
    });
  });

  it("holds a follow-up until the turn ends, also after a save", () => {
    const { message, delivery, next } = enqueue(busy(), "followUp");
    expect(delivery).toBe("queued");
    expect(message.waitForIdle).toBe(true);
    expect(steerableQueuedMessage(next)).toBeUndefined();
    expect(canDispatchQueuedHead({ ...next, busy: false })).toBe(true);
    const saved = sanitizeSessionForPersist(next).queuedMessages?.[0];
    expect(saved).toMatchObject({ id: message.id, waitForIdle: true });
  });

  it("lets the user's own message steer past a follow-up that waits for the turn end", () => {
    const { message: waiting, next } = enqueue(busy(), "followUp");
    const user: QueuedMessage = {
      id: "user",
      text: "Use TypeScript",
      attachments: [],
    };
    const both = enqueueMonoMessage(next, user);
    expect(steerableQueuedMessage(both)?.id).toBe("user");
    expect(both.queuedMessages?.map((queued) => queued.id)).toEqual([
      waiting.id,
      "user",
    ]);
    expect(monoMessageDeliveries(both).get(waiting.id)).toEqual({
      status: "waiting",
    });
  });

  it.each([
    [undefined, undefined],
    ["steer", undefined],
    ["followUp", true],
  ] as const)(
    "queues behind the waiting messages of an idle Mono (whenBusy: %s)",
    (whenBusy, waitForIdle) => {
      const waiting: QueuedMessage = {
        id: "waiting",
        text: "Check the logs",
        attachments: [],
      };
      const session: Session = {
        ...idle(),
        queuedMessages: [waiting],
        queueStatus: "paused",
      };
      const { message, delivery, next } = enqueue(session, whenBusy);
      expect(delivery).toBe("queued");
      expect(message.waitForIdle).toBe(waitForIdle);
      expect(next.queuedMessages?.map((queued) => queued.id)).toEqual([
        "waiting",
        message.id,
      ]);
    },
  );

  it("answers a retried steer with the first delivery, also after the steer", () => {
    const { message, next } = enqueue(busy(), "steer");
    const first = {
      kind: "prior",
      result: {
        alreadySubmitted: true,
        delivery: "steered",
        queuedMessageId: message.id,
      },
    };
    expect(plan(next, "steer")).toEqual(first);
    const steered = acknowledgeMonoMessage(next, next.queuedMessages![0], {
      mode: "follow-up",
    });
    expect(steered.queuedMessages).toBeUndefined();
    expect(plan(steered, "steer")).toEqual(first);
  });

  it("answers a retried follow-up with the first delivery", () => {
    const { message, next } = enqueue(busy(), "followUp");
    expect(plan(next, "followUp")).toEqual({
      kind: "prior",
      result: {
        alreadySubmitted: true,
        delivery: "queued",
        queuedMessageId: message.id,
      },
    });
  });

  it("answers a retried new turn as sent while that turn runs", () => {
    const sent = appendUser(idle(), text, [], { appRequestId: requestId });
    expect(plan(sent)).toEqual({
      kind: "prior",
      result: { alreadySubmitted: true, delivery: "sent" },
    });
  });

  it("refuses a request ID reused with another prompt", () => {
    const error = {
      kind: "error",
      error: "Request ID was already used with another prompt",
    };
    const { next } = enqueue(busy(), "steer");
    expect(plan(next, "steer", { text: "Another prompt" })).toEqual(error);
    const sent = appendUser(idle(), text, [], { appRequestId: requestId });
    expect(plan(sent, undefined, { text: "Another prompt" })).toEqual(error);
  });
});
