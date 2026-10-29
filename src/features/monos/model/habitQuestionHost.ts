import type { Dispatch, RefObject, SetStateAction } from "react";
import { updateSessionState } from "../../../app/model/sessionState";
import type { Block, Session } from "../../sessions/model/session";
import type {
  UserQuestionPrompt,
  UserQuestionReply,
} from "../../sessions/model/userQuestion";
import {
  answerHabitQuestion,
  endHabitQuestion,
  type HabitQuestionStop,
} from "./habitQuestion";
import {
  dropRelays,
  questionRoute,
  relayApproval,
  type ApprovalRelay,
  type Habit,
} from "./monoHabits";

type Reply = (
  sessionId: string,
  requestId: number,
  reply: UserQuestionReply,
) => void;

type Host = {
  sessions: RefObject<Session[]>;
  setSessions: Dispatch<SetStateAction<Session[]>>;
  persist: (session: Session | undefined) => void;
  /** Adds the block once the Mono is free, then calls `then`. */
  append: (monoId: string, block: Block, then?: () => void) => void;
};

let host: Host | undefined;

/** How a relayed card settles once its run ended, for one that lands later. */
const ended = new WeakMap<ApprovalRelay, (block: Block) => Block>();

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

/** Puts a run's question to the user in its Mono's chat. */
export function askHabitQuestion(
  monoId: string,
  habit: Habit,
  runId: string,
  runAt: number,
  prompt: UserQuestionPrompt,
): void {
  const blockId = crypto.randomUUID();
  const relay = { monoId, runId, requestId: prompt.requestId, blockId };
  const requestId = relayApproval(relay);
  host?.append(
    monoId,
    {
      id: blockId,
      role: "assistant",
      text: "",
      habitQuestion: {
        runId,
        runAt,
        // The run's own card keeps any deadline; this one only answers.
        prompt: { requestId, title: prompt.title, questions: prompt.questions },
      },
      monoHabit: { id: habit.id, name: habit.name, at: Date.now() },
    },
    // A busy Mono holds the card back for up to 10 minutes. One whose run
    // ended in the meantime settles as it lands, never open.
    () => {
      const settle = ended.get(relay);
      if (settle) settleHabitCard(relay, settle);
    },
  );
}

/**
 * Settles every card a run left open in the Mono's chat once it ends: an
 * approval is withdrawn, a question takes the run's end.
 */
export function endHabitRun(
  runId: string,
  end: HabitQuestionStop = { kind: "stopped" },
): void {
  const settle = (block: Block): Block =>
    block.approval
      ? { ...block, approval: { ...block.approval, decided: "cancelled" } }
      : endHabitQuestion(block, end);
  for (const relay of dropRelays(runId)) {
    ended.set(relay, settle);
    settleHabitCard(relay, settle);
  }
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
