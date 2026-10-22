import type { Dispatch, RefObject, SetStateAction } from "react";
import { updateSessionState } from "../../../app/model/sessionState";
import type { Block, Session } from "../../sessions/model/session";
import type { UserQuestionReply } from "../../sessions/model/userQuestion";
import { answerHabitQuestion } from "./habitQuestion";
import { questionRoute, type ApprovalRelay } from "./monoHabits";

type Reply = (
  sessionId: string,
  requestId: number,
  reply: UserQuestionReply,
) => void;

type Host = {
  sessions: RefObject<Session[]>;
  setSessions: Dispatch<SetStateAction<Session[]>>;
  persist: (session: Session | undefined) => void;
  append: (monoId: string, block: Block) => void;
};

let host: Host | undefined;

/**
 * The app's session store, bound on every render as the turn guard's host
 * is, so the app wires these functions by name and keeps no logic of its own.
 */
export function setHabitQuestionHost(
  sessions: Host["sessions"],
  setSessions: Host["setSessions"],
  persist: Host["persist"],
  append: Host["append"],
): void {
  host = { sessions, setSessions, persist, append };
}

const routed = new WeakMap<Reply, Reply>();

/**
 * The app's one answer path, with the habit's card in it: either card's
 * answer reaches the run once, and the Mono's card settles with it. The same
 * path in gives the same function out, so a pane's props stay stable.
 */
export function habitQuestionReply(reply: Reply): Reply {
  const known = routed.get(reply);
  if (known) return known;
  const route: Reply = (sessionId, requestId, answer) => {
    const target = questionRoute(sessionId, requestId);
    const live = host?.sessions.current.some(
      (session) => session.id === target.sessionId,
    );
    if (target.card && live)
      settleHabitCard(target.card, (block) =>
        answerHabitQuestion(block, answer),
      );
    reply(target.sessionId, target.requestId, answer);
  };
  routed.set(reply, route);
  return route;
}

/** Applies `update` to the habit card a relay points at and saves the Mono's
 * chat. */
export function settleHabitCard(
  relay: ApprovalRelay,
  update: (block: Block) => Block,
): void {
  if (!host) return;
  const mono = host.sessions.current.find(
    (session) => session.id === relay.monoId,
  );
  if (!mono?.blocks.some((entry) => entry.id === relay.blockId)) return;
  const sessions = updateSessionState(host.sessions, host.setSessions, (all) =>
    all.map((session) =>
      session.id === relay.monoId
        ? {
            ...session,
            blocks: session.blocks.map((entry) =>
              entry.id === relay.blockId ? update(entry) : entry,
            ),
          }
        : session,
    ),
  );
  host.persist(sessions.find((session) => session.id === relay.monoId));
}
