import { isPreparingHandoff } from "./handoff";
import { consumeOperatorCommand } from "./operatorCommand";
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
export function waitsForTurnEnd(message: QueuedMessage): boolean {
  return (
    !!message.waitForIdle ||
    !!message.monoSessionCompletion ||
    message.intent === "plan" ||
    message.intent === "orchestrate" ||
    consumeOperatorCommand(message.text).matched
  );
}

/**
 * The message a busy, ready Mono steers next: the first one that can steer.
 * Messages that wait for the turn to end keep their place but never block it.
 */
export function steerableQueuedMessage(
  session: Session,
): QueuedMessage | undefined {
  if (
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
  )
    return undefined;
  const message = session.queuedMessages?.find(
    (entry) => !waitsForTurnEnd(entry),
  );
  return message && session.editingQueuedMessageId !== message.id
    ? message
    : undefined;
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
