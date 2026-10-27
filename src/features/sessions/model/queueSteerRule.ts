import { canSteerHarness } from "../../../integrations/harness/core/registry";
import {
  canDispatchQueuedHead,
  canSteerNow,
  queuedHead,
  queuedSteer,
  type RunningTurns,
} from "./messageQueue";
import type { QueuedMessage, Session } from "./session";

/**
 * The queue pump would steer an item into Mono chat `session`'s running turn
 * now (design 16.6): its harness can steer and the turn takes a steer now.
 */
export function steersQueuedNow(session: Session, mono: boolean): boolean {
  return mono && canSteerHarness(session.harness) && canSteerNow(session);
}

/**
 * What the queue pump sends next: an idle session's head opens a turn; a busy
 * session that `steersQueuedNow` steers `queuedSteer`.
 */
export function nextQueuedSend(
  session: Session,
  turns: RunningTurns,
  mono: boolean,
): QueuedMessage | undefined {
  if (canDispatchQueuedHead(session)) return queuedHead(session);
  return steersQueuedNow(session, mono)
    ? queuedSteer(session, turns)
    : undefined;
}
