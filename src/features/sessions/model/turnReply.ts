import {
  opensTurn,
  turnOpener,
  type Block,
  type BlockRole,
  type Session,
} from "./session";

/** Blocks the app or the user writes; every other block is the agent's. */
const NOT_AGENT_OUTPUT: ReadonlySet<BlockRole> = new Set([
  "user",
  "system",
  "handoff",
]);

/**
 * The turn-bounded reply rule: the first block the agent wrote after
 * `prompt` and before the turn ends, at the next block that opens a turn or
 * the interrupt note. It is the proof that the agent took `prompt`; a later
 * turn's reply is not. mono_completions.rs `settle` reads a saved Mono chat
 * by the same rule.
 */
export function agentReplyTo(
  blocks: readonly Block[],
  prompt: Block,
): Block | undefined {
  for (const block of blocks.slice(blocks.indexOf(prompt) + 1)) {
    if (opensTurn(block) || block.notice === "interrupt") return undefined;
    if (!NOT_AGENT_OUTPUT.has(block.role)) return block;
  }
  return undefined;
}

function turnReplyId(blocks: readonly Block[]): string | undefined {
  const opener = turnOpener(blocks);
  return opener && agentReplyTo(blocks, opener)?.id;
}

/**
 * The agent wrote its first block of the running turn since `previous`. A
 * busy chat is saved then, so a Mono chat save shows that the agent took the
 * notice that opened the turn (ADE-89).
 */
export function turnReplyBegan(
  previous: Session | undefined,
  session: Session,
): boolean {
  if (!session.busy) return false;
  const reply = turnReplyId(session.blocks);
  return !!reply && reply !== (previous && turnReplyId(previous.blocks));
}
