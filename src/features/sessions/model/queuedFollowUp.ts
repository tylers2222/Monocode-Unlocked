import { TurnNotReadyError } from "../../../integrations/harness/core/types";
import {
  busyRoute,
  planUserSend,
  type SendOrigin,
  type UserSend,
} from "./messageIntent";
import { queuedSteer, type RunningTurns } from "./messageQueue";
import type { Block, QueuedMessage, Session } from "./session";

/** What a send records on its block and gives the provider. */
type JoinedTurn = { block: Pick<Block, "turnId" | "link">; text: string };

/**
 * The queue item a message the user sends into a busy Mono chat (or one whose
 * queue has not started) becomes: the pump steers it later into the turn it
 * was typed in, and a reply, a continue or a cleared steer waits for the turn
 * end (design 16.8, 16.12.4). Undefined for a picked steer whose work ended.
 */
export function typedMonoMessage(input: {
  session: Session;
  turns: RunningTurns;
  origin: SendOrigin | undefined;
  message: Omit<QueuedMessage, "link" | "waitForIdle" | "typedInTurn">;
}): QueuedMessage | undefined {
  const queued = planUserSend({
    origin: input.origin,
    item: undefined,
    text: input.message.text,
    route: busyRoute(input.session, input.turns, "later"),
  });
  return queued.kind === "queue"
    ? { ...input.message, ...queued.item }
    : undefined;
}

/**
 * How queued `message` steers busy Mono chat `session` now, with the
 * provider text `text`. Undefined when the pump may not steer it now.
 */
export function planQueuedSteer(input: {
  session: Session;
  turns: RunningTurns;
  message: QueuedMessage;
  origin: SendOrigin | undefined;
  text: string;
}): UserSend | undefined {
  if (queuedSteer(input.session, input.turns) !== input.message)
    return undefined;
  return planUserSend({
    origin: input.origin,
    item: input.message,
    text: input.text,
    route: busyRoute(input.session, input.turns, "now"),
  });
}

/**
 * Steer `message` into the running turn as `joined` plans it. The message stays
 * pending until the provider accepts it; preparation is part of delivery. Each
 * check reads the turn `turns` holds now, so a new turn refuses the message.
 */
export async function deliverQueuedFollowUp<T>(options: {
  message: QueuedMessage;
  joined: JoinedTurn;
  turns: RunningTurns;
  current: () => Session | undefined;
  isCurrentTurn: () => boolean;
  /** Prepares the provider text `joined` plans. */
  prepare: (text: string) => Promise<T>;
  steer: (prepared: T) => Promise<void>;
  /** The provider took it; the bubble records `block`. */
  delivered: (block: JoinedTurn["block"]) => void;
  deferred: () => void;
  failed: (error: string) => void;
}): Promise<boolean> {
  const canDeliver = () => {
    const session = options.current();
    return (
      options.isCurrentTurn() &&
      !!session &&
      queuedSteer(session, options.turns) === options.message
    );
  };
  if (!canDeliver()) return false;
  try {
    const prepared = await options.prepare(options.joined.text);
    // An edit, cancellation, provider switch, stop or new turn during preparation must win.
    if (!canDeliver()) return false;
    await options.steer(prepared);
  } catch (error) {
    if (!options.isCurrentTurn()) return false;
    if (error instanceof TurnNotReadyError) options.deferred();
    else options.failed(error instanceof Error ? error.message : String(error));
    return false;
  }
  options.delivered(options.joined.block);
  return true;
}
