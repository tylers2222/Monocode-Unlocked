import type { Block, Session } from "../../sessions/model/session";
import type { ApprovalRelay } from "./monoHabits";

type Host = {
  sessions: { current: Session[] };
  setSessions: (sessions: Session[]) => void;
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
  const block = mono?.blocks.find((entry) => entry.id === relay.blockId);
  const next = block && update(block);
  if (!mono || !next) return;
  const settled: Session = {
    ...mono,
    blocks: mono.blocks.map((entry) => (entry === block ? next : entry)),
  };
  host.sessions.current = host.sessions.current.map((session) =>
    session.id === settled.id ? settled : session,
  );
  host.setSessions(host.sessions.current);
  host.persist(settled);
}
