import { isPreparingHandoff } from "./handoff";
import { consumeOperatorCommand } from "./operatorCommand";
import type { TurnId } from "../../monos/model/monoMessages";
import type { QueuedMessage, Session } from "./session";

export function queuedHead(session: Session): QueuedMessage | undefined {
  return session.queuedMessages?.[0];
}

/** Hold auto-dispatch only while the item about to send is being edited. */
export function isEditingQueuedHead(session: Session): boolean {
  const head = queuedHead(session);
  return Boolean(head && session.editingQueuedMessageId === head.id);
}

export function dequeueQueuedMessage(
  session: Session,
  messageId: string,
): Session {
  const queuedMessages = (session.queuedMessages ?? []).filter(
    (message) => message.id !== messageId,
  );
  const remains = queuedMessages.length > 0;
  return {
    ...session,
    queuedMessages: remains ? queuedMessages : undefined,
    queueStatus: remains ? session.queueStatus : undefined,
    editingQueuedMessageId:
      session.editingQueuedMessageId === messageId
        ? undefined
        : session.editingQueuedMessageId,
  };
}

/**
 * True when the idle session can send its queued head as a new turn.
 * Busy / paused / resuming / usage-limited / preparing-handoff /
 * editing-the-head all wait.
 */
export function canDispatchQueuedHead(session: Session): boolean {
  if (session.busy) return false;
  if (session.worktreePreparing || session.worktreeRemoved) return false;
  if (session.usageLimit) return false;
  if (session.queueStatus === "paused" || session.queueStatus === "resuming") {
    return false;
  }
  const head = queuedHead(session);
  if (!head) return false;
  if (isEditingQueuedHead(session)) return false;
  if (isPreparingHandoff(session)) return false;
  return true;
}

/** A message that can only start a new turn; it never steers a running one. */
function waitsForTurnEnd(message: QueuedMessage): boolean {
  return (
    !!message.waitForIdle ||
    !!message.monoSessionCompletion ||
    message.intent === "plan" ||
    message.intent === "orchestrate" ||
    consumeOperatorCommand(message.text).matched
  );
}

/** The user typed it: not a Mono row, a CLI follow-up or a completion report. */
export function isOwnQueuedMessage(message: QueuedMessage): boolean {
  return !(
    message.monoMessageId ||
    message.appRequestId ||
    message.monoSessionCompletion
  );
}

/**
 * The messages that cannot steer the running turn: each one that waits for
 * the turn end or `missed` its turn, and every message the user typed after
 * one of their own that cannot, so the user's messages reach the agent in the
 * order typed (design 16.12.4). A Mono row or a CLI follow-up keeps its place
 * but never holds a later message.
 */
function heldMessages(
  messages: readonly QueuedMessage[],
  missed: (message: QueuedMessage) => boolean,
): Set<QueuedMessage> {
  const held = new Set<QueuedMessage>();
  let ownHeld = false;
  for (const message of messages) {
    const own = isOwnQueuedMessage(message);
    if ((own && ownHeld) || waitsForTurnEnd(message) || missed(message)) {
      held.add(message);
      ownHeld ||= own;
    }
  }
  return held;
}

/**
 * What a busy session holds until its turn ends; the bubble says "Sends when
 * this turn ends". A message typed in a turn counts as typed in the running
 * one, because `missedTurn` releases the others when their turn ends.
 */
export function heldUntilTurnEnd(session: Session): ReadonlySet<QueuedMessage> {
  return session.busy
    ? heldMessages(session.queuedMessages ?? [], () => false)
    : new Set();
}

/**
 * The running turn ended, so every message the user typed for it missed it.
 * Each one now waits for a turn end and opens its own turn, as a message typed
 * while no turn runs does (design 16.12.4).
 */
export function missedTurn(messages: QueuedMessage[]): QueuedMessage[] {
  if (!messages.some((message) => message.typedInTurn)) return messages;
  return messages.map((message) => {
    if (!message.typedInTurn) return message;
    const { typedInTurn: _missed, ...rest } = message;
    return { ...rest, waitForIdle: true };
  });
}

/**
 * A busy session takes a steer now: its turn is ready and nothing holds it
 * (a pending question, a paused queue, a provider switch or a hand-off).
 */
function canSteerNow(session: Session): boolean {
  return !(
    !session.busy ||
    session.turnReadiness !== "ready" ||
    session.worktreePreparing ||
    session.worktreeRemoved ||
    session.pendingSwitch ||
    session.usageLimit ||
    (session.pendingQuestion &&
      session.pendingQuestion.autoResolveAt == null) ||
    session.queueStatus === "paused" ||
    session.queueStatus === "resuming" ||
    isPreparingHandoff(session)
  );
}

/**
 * The message a busy, ready Mono steers next into running turn `turnId`: the
 * first one `heldMessages` does not hold. A message the user typed during an
 * earlier turn missed that turn, so it is held too (design 16.12.4).
 */
export function steerableQueuedMessage(
  session: Session,
  turnId: TurnId | undefined,
): QueuedMessage | undefined {
  if (!canSteerNow(session)) return undefined;
  const queue = session.queuedMessages ?? [];
  const held = heldMessages(
    queue,
    (entry) => entry.typedInTurn !== undefined && entry.typedInTurn !== turnId,
  );
  const message = queue.find((entry) => !held.has(entry));
  return message && session.editingQueuedMessageId !== message.id
    ? message
    : undefined;
}

/** The turn each session runs in this window, by session id (design 5.2). */
export type RunningTurns = ReadonlyMap<string, TurnId>;

/** The message a busy Mono steers next into the turn this window runs for it. */
export function queuedSteer(
  session: Session,
  turns: RunningTurns,
): QueuedMessage | undefined {
  return steerableQueuedMessage(session, turns.get(session.id));
}

/**
 * What the queue pump sends next: an idle session's head opens a turn; a busy
 * session that `steers` (a Mono whose harness can steer) steers `queuedSteer`.
 */
export function nextQueuedSend(
  session: Session,
  turns: RunningTurns,
  steers: boolean,
): QueuedMessage | undefined {
  if (canDispatchQueuedHead(session)) return queuedHead(session);
  return steers ? queuedSteer(session, turns) : undefined;
}

/** Drop the quit reason: a later pause or a resume replaces it. */
export function withoutPauseReason(
  messages: QueuedMessage[] | undefined,
): QueuedMessage[] | undefined {
  return messages?.some((message) => message.pausedBy)
    ? messages.map(({ pausedBy: _, ...message }) => message)
    : messages;
}

export const QUIT_PAUSE_NOTICE = "Paused when MonoCode quit";

/** Hold what is left of the queue. A new pause replaces the reason for an old one. */
export function pauseQueue(session: Session): Session {
  if (!session.queuedMessages?.length) return session;
  return {
    ...session,
    queueStatus: "paused",
    queuedMessages: withoutPauseReason(session.queuedMessages),
  };
}

/** A message could not be sent: keep it with its error and hold the rest. */
export function failQueuedMessage(
  session: Session,
  messageId: string,
  error: string,
): Session {
  return pauseQueue({
    ...session,
    queuedMessages: session.queuedMessages?.map((message) =>
      message.id === messageId ? { ...message, error } : message,
    ),
  });
}

/** Resume the queue: every message is sent or retried again. */
export function resumeQueuedMessages(
  messages: QueuedMessage[] | undefined,
): QueuedMessage[] | undefined {
  return withoutPauseReason(messages)?.map((message) => ({
    ...message,
    error: undefined,
  }));
}

/** Why a paused queue is not sending, when the user did not pause it. */
export function queuePauseNotice(session: Session): string | undefined {
  return session.queuedMessages?.some((message) => message.pausedBy === "quit")
    ? QUIT_PAUSE_NOTICE
    : undefined;
}

/** Resolve a queued row for auto-dispatch (head, idle) or an explicit Steer. */
export function queuedMessageForSubmit(
  session: Session,
  messageId: string,
  mode: "dispatch" | "steer",
): QueuedMessage | undefined {
  const message = session.queuedMessages?.find(
    (entry) => entry.id === messageId,
  );
  if (!message) return undefined;
  if (mode === "steer")
    return message.monoSessionCompletion || message.waitForIdle
      ? undefined
      : message;
  if (queuedHead(session)?.id !== messageId) return undefined;
  if (!canDispatchQueuedHead(session)) return undefined;
  return message;
}
