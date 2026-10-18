import type { Block, Session } from "../../sessions/model/session";
import { operatorUserPrompt } from "../../sessions/model/operatorCommand";
import { AppActionError } from "./appActionError";

type Exchange = { user: Block; assistants: Block[]; work: Block[] };

export type SessionReadOptions = {
  before?: string;
  limit?: number;
  maxChars?: number;
  /** Also list each turn's reasoning and tool rows, in order. */
  work?: boolean;
};

function capped(text: string, maxChars: number) {
  const trimmed = text.trim();
  return {
    text: trimmed.slice(0, maxChars),
    truncated: trimmed.length > maxChars,
  };
}

function workRow(block: Block, maxChars: number) {
  if (block.role === "reasoning")
    return { kind: "reasoning" as const, ...capped(block.text, maxChars) };
  return {
    kind: "tool" as const,
    tool: block.tool?.title ?? block.tool?.kind ?? null,
    status: block.tool?.status ?? null,
    input: capped(block.text, maxChars),
    result: block.tool?.detail ? capped(block.tool.detail, maxChars) : null,
  };
}

/**
 * Show conversation prose, plus reasoning and tool rows when `work` is set,
 * with a stable cursor for older exchanges.
 */
export function sessionConversationPage(
  session: Session,
  options: SessionReadOptions = {},
) {
  const limit = options.limit ?? 3;
  if (!Number.isInteger(limit) || limit < 1 || limit > 3)
    throw new AppActionError(
      "invalid_input",
      "limit must be an integer from 1 to 3",
    );
  const maxChars = options.maxChars ?? 1200;
  if (!Number.isInteger(maxChars) || maxChars < 200 || maxChars > 6000)
    throw new AppActionError(
      "invalid_input",
      "maxChars must be an integer from 200 to 6000",
    );
  if (options.work !== undefined && typeof options.work !== "boolean")
    throw new AppActionError("invalid_input", "work must be true or false");

  const exchanges: Exchange[] = [];
  for (const block of session.blocks) {
    if (
      block.role === "user" &&
      (!block.internal || block.monoSessionCompletion) &&
      !block.draft
    ) {
      exchanges.push({ user: block, assistants: [], work: [] });
    } else if (
      block.role === "assistant" &&
      !block.internal &&
      block.text.trim() &&
      exchanges.length > 0
    ) {
      exchanges[exchanges.length - 1].assistants.push(block);
    } else if (
      options.work &&
      (block.role === "reasoning" || block.role === "tool") &&
      !block.internal &&
      exchanges.length > 0
    ) {
      exchanges[exchanges.length - 1].work.push(block);
    }
  }

  const end = options.before
    ? exchanges.findIndex((exchange) => exchange.user.id === options.before)
    : exchanges.length;
  if (end < 0)
    throw new AppActionError(
      "invalid_input",
      "before is not a turn ID in this session",
    );
  const start = Math.max(0, end - limit);
  const selected = exchanges.slice(start, end);
  return {
    sessionId: session.id,
    title: session.title,
    busy: !!session.busy,
    hasDraft: session.blocks.some(
      (block) => block.role === "user" && block.draft,
    ),
    turns: selected.map(({ user, assistants, work }) => ({
      turnId: user.id,
      user: capped(
        user.monoSessionCompletion
          ? user.monoSessionCompletion.sessionCount
            ? `MonoCode: results from ${user.monoSessionCompletion.sessionCount} sessions`
            : `MonoCode: session ${user.monoSessionCompletion.status}: ${user.monoSessionCompletion.title}`
          : operatorUserPrompt(user),
        maxChars,
      ),
      assistant: assistants.length
        ? capped(assistants[assistants.length - 1].text, maxChars)
        : null,
      earlierAssistantMessages: Math.max(0, assistants.length - 1),
      ...(options.work
        ? { work: work.map((block) => workRow(block, maxChars)) }
        : {}),
    })),
    nextBefore: start > 0 ? selected[0]?.user.id : null,
  };
}
