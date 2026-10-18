import type { Block, Session } from "../../sessions/model/session";
import { operatorUserPrompt } from "../../sessions/model/operatorCommand";
import { redactSecrets } from "../../monos/model/monoMemory";
import { AppActionError } from "./appActionError";

type Exchange = { user: Block; assistants: Block[]; work?: Block[] };
type Excerpt = { text: string; truncated: boolean };

/** One reasoning or tool row of a turn, as `work: true` returns it. */
export type WorkRow =
  | ({ kind: "reasoning" } & Excerpt)
  | {
      kind: "tool";
      tool: string | null;
      status: string | null;
      input: Excerpt;
      result: Excerpt | null;
    };

/** JSON bytes of work rows per turn: 3 turns stay under the CLI's 4 MB reply cap. */
export const WORK_BYTES_MAX = 1_000_000;
const utf8 = new TextEncoder();
const WORK_ROLES: Partial<Record<Block["role"], true>> = {
  reasoning: true,
  tool: true,
};

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

function workRow(block: Block, maxChars: number): WorkRow {
  const excerpt = (text: string) => capped(redactSecrets(text), maxChars);
  if (block.role === "reasoning")
    return { kind: "reasoning", ...excerpt(block.text) };
  return {
    kind: "tool",
    tool: block.tool?.kind ?? null,
    status: block.tool?.status ?? null,
    input: excerpt(block.text),
    result: block.tool?.detail ? excerpt(block.tool.detail) : null,
  };
}

/** The turn's newest rows that fit WORK_BYTES_MAX, in order, and how many older rows were left out. */
function workPage(work: Block[], maxChars: number) {
  const rows: WorkRow[] = [];
  let bytes = 0;
  for (let i = work.length - 1; i >= 0; i -= 1) {
    const row = workRow(work[i], maxChars);
    bytes += utf8.encode(JSON.stringify(row)).length + 1;
    if (bytes > WORK_BYTES_MAX) break;
    rows.push(row);
  }
  return { work: rows.reverse(), omittedWork: work.length - rows.length };
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
      exchanges.push({
        user: block,
        assistants: [],
        ...(options.work ? { work: [] } : {}),
      });
    } else if (
      block.role === "assistant" &&
      !block.internal &&
      block.text.trim() &&
      exchanges.length > 0
    ) {
      exchanges[exchanges.length - 1].assistants.push(block);
    } else if (
      options.work &&
      WORK_ROLES[block.role] &&
      !block.internal &&
      exchanges.length > 0
    ) {
      exchanges[exchanges.length - 1].work?.push(block);
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
      ...(work && workPage(work, maxChars)),
    })),
    nextBefore: start > 0 ? selected[0]?.user.id : null,
  };
}
