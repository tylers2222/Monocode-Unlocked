import type {
  AppSendResult,
  WhenBusy,
} from "../../features/agent-app/model/agentApp";
import { enqueueMonoMessage } from "../../features/monos/model/monoMessaging";
import type {
  Block,
  QueuedMessage,
  Session,
} from "../../features/sessions/model/session";

export const UNKNOWN_MONO_ERROR = "Unknown Mono; run monos.list for IDs";
export const NO_MONO_CONVERSATION_ERROR =
  "That Mono has no conversation yet; open it in MonoCode once";
export const MONO_BUSY_ERROR =
  'That Mono is busy; pass whenBusy "steer" or "followUp"';
export const MONO_CANNOT_STEER_ERROR =
  'That Mono\'s provider cannot steer a running turn; use whenBusy "followUp"';
export const MONO_REJECTED_ERROR = "That Mono could not accept the message";

export type AppMonoSendPlan =
  | { kind: "error"; error: string }
  | { kind: "prior"; result: AppSendResult }
  | { kind: "submit"; sessionId: string }
  | {
      kind: "enqueue";
      sessionId: string;
      message: QueuedMessage;
      delivery: "steered" | "queued";
    };

/** Decide how `monos.send` delivers a message to another Mono's chat. */
export function planAppMonoSend(input: {
  /** The target Mono's chat; undefined until it has one. */
  session: Session | undefined;
  /** Already wrapped as a message from the sending Mono. */
  text: string;
  /** Already namespaced by the sending session. */
  requestId: string;
  whenBusy?: WhenBusy;
  /** canSteerHarness(session.harness), from the caller. */
  steerable: boolean;
}): AppMonoSendPlan {
  const { session, text, requestId, whenBusy } = input;
  if (!session) return { kind: "error", error: NO_MONO_CONVERSATION_ERROR };
  const prior = priorAppMonoSend(session, requestId, text);
  if (prior) return prior;
  if (!session.busy && !session.queuedMessages?.length)
    return { kind: "submit", sessionId: session.id };
  if (session.busy && whenBusy === undefined)
    return { kind: "error", error: MONO_BUSY_ERROR };
  if (session.busy && whenBusy === "steer" && !input.steerable)
    return { kind: "error", error: MONO_CANNOT_STEER_ERROR };
  return {
    kind: "enqueue",
    sessionId: session.id,
    message: {
      id: crypto.randomUUID(),
      text,
      attachments: [],
      appRequestId: requestId,
      ...(whenBusy === "followUp" ? { waitForIdle: true } : {}),
    },
    // As with a message the user types, the queue steers it into the running
    // turn once the provider is ready. A queue that is already waiting keeps
    // its order.
    delivery: session.busy && whenBusy === "steer" ? "steered" : "queued",
  };
}

/** The first delivery of a retried request, or the error for reusing its ID
 *  with another prompt. Nothing can edit a Mono's waiting message. */
function priorAppMonoSend(
  session: Pick<Session, "blocks" | "queuedMessages">,
  requestId: string,
  text: string,
): AppMonoSendPlan | undefined {
  const queued = session.queuedMessages?.find(
    (message) => message.appRequestId === requestId,
  );
  const bubble = session.blocks.find(
    (block) => block.appRequestId === requestId,
  );
  const previous = queued ?? bubble;
  if (!previous) return undefined;
  if (previous.text !== text)
    return {
      kind: "error",
      error: "Request ID was already used with another prompt",
    };
  const prior = (
    delivery: AppSendResult["delivery"],
    queuedMessageId?: string,
  ): AppMonoSendPlan => ({
    kind: "prior",
    result: {
      alreadySubmitted: true,
      delivery,
      ...(queuedMessageId ? { queuedMessageId } : {}),
    },
  });
  const delivery = bubble?.appDelivery;
  // A queued message's bubble has the message's ID.
  if (bubble && delivery?.via === "steered") return prior("steered", bubble.id);
  if (queued) return prior("queued", queued.id);
  if (delivery?.via === "queued")
    return prior("queued", delivery.queuedMessageId);
  return prior("sent");
}

/** Add a planned message to the Mono's queue and chat. Its bubble keeps the
 *  request and the delivery, so a retry after delivery reports the same one. */
export function enqueueAppMonoMessage(
  session: Session,
  plan: Extract<AppMonoSendPlan, { kind: "enqueue" }>,
): Session {
  return enqueueMonoMessage(session, plan.message, {
    appRequestId: plan.message.appRequestId,
    appDelivery:
      plan.delivery === "steered"
        ? { via: "steered" }
        : { via: "queued", queuedMessageId: plan.message.id },
  });
}

/** Epoch ms of the newest message, finished turn or habit post in a chat. */
export function chatUpdatedAt(blocks: readonly Block[]): number | undefined {
  let latest = 0;
  for (const block of blocks) {
    const ended = block.startedAt && block.startedAt + (block.durationMs ?? 0);
    latest = Math.max(
      latest,
      ended ?? 0,
      block.sentAt ?? 0,
      block.monoHabit?.at ?? 0,
    );
  }
  return latest || undefined;
}
