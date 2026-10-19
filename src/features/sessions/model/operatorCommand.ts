import type { BuiltinSkill } from "../../skills/model/skills";
import type { Block } from "./session";

export const OPERATOR_COMMAND: BuiltinSkill = {
  kind: "builtin",
  name: "operator",
  invocation: "operator",
  description:
    "Give this thread access to MonoCode sessions, folders, and notes.",
  scope: "builtin",
  source: "monocode",
};

/** Activate app access with a leading composer command. */
export function consumeOperatorCommand(text: string): {
  text: string;
  matched: boolean;
} {
  // Keep the old spellings as unlisted aliases for existing drafts and threads.
  const match = text.match(/^\s*\/(?:operator|mono|monocode)(?=\s|$)\s*/i);
  if (!match) return { text, matched: false };
  return { text: text.slice(match[0].length), matched: true };
}

const LEGACY_COMMAND = /^\s*\/(?:mono|monocode)(?=\s|$)\s*/i;

/** A submitted /operator turn keeps CLI access available in later turns. */
export function isOperatorUserTurn(block: Block): boolean {
  return (
    block.role === "user" &&
    !block.draft &&
    !block.internal &&
    // This persisted field keeps its original name for saved session compatibility.
    (block.monocode === true || LEGACY_COMMAND.test(block.text))
  );
}

/** Old command messages remain enabled and render without their old prefix. */
export function operatorUserPrompt(block: Block): string {
  const legacy = block.text.match(LEGACY_COMMAND);
  if (!legacy) return block.text;
  return (
    block.text.slice(legacy[0].length).trim() ||
    "Explain what you can do in MonoCode with the app CLI."
  );
}

export function operatorEnabledInThread(blocks: Block[]): boolean {
  return blocks.some(isOperatorUserTurn);
}

/** How a thread reached app access (design 13.4). */
export type AppAccess = "operator" | "started";

/**
 * `operator` once the thread has a submitted /operator turn; else `started`
 * when `sessions.start` made the session, which stamps its first user block;
 * else none.
 */
export function appAccess(blocks: Block[]): AppAccess | null {
  if (operatorEnabledInThread(blocks)) return "operator";
  const first = blocks.find((block) => block.role === "user" && !block.draft);
  return first?.startedBy ? "started" : null;
}

/**
 * The access of the turn a submit starts: `operator` for a typed /operator,
 * else the thread's, else `started` when this submit is the first turn of a
 * session that `sessions.start` (`startedBy`) made, whose block it stamps.
 */
export function submitAccess(
  blocks: Block[],
  typedOperator: boolean,
  startedBy: string | undefined,
): AppAccess | null {
  if (typedOperator) return "operator";
  return appAccess(blocks) ?? (startedBy ? "started" : null);
}

/**
 * What a turn gets: app access for any access, and `controlsAgents` (codex
 * opens its sandbox network so the CLI reaches the app's loopback) only for
 * access the user granted, a Mono's chat or /operator, or for an active
 * orchestration run, whose lead keeps its own control access. A session
 * that `sessions.start` made keeps its runtime mode's network. Each harness
 * call of a turn spreads this, so no call site picks either field itself.
 */
export function turnAccess(
  access: AppAccess | null,
  mono: boolean,
  orchestrating: boolean,
): { appAccess: boolean; controlsAgents: boolean } {
  return {
    appAccess: mono || access !== null,
    controlsAgents: mono || access === "operator" || orchestrating,
  };
}
