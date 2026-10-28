import type { Dispatch, RefObject, SetStateAction } from "react";
import { updateSessionState } from "../../../app/model/sessionState";
import type { Block, Session } from "../../sessions/model/session";
import type { ApprovalRelay } from "./monoHabits";

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
