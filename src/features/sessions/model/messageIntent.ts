import type {
  Handle,
  MessageId,
  SessionId,
  TurnId,
} from "../../monos/model/monoMessages";

/**
 * What a line needs of the message a Mono row refers to (design 16.7). Rust
 * reads it in the transaction that hands the row off.
 */
export type ReplyTarget = {
  messageId: MessageId;
  body: string;
  /** Null once the chat that handled it was reset, archived or deleted. */
  handledBySession: SessionId | null;
  /** Its handle and the turn that holds it; null when it holds none. */
  slot: { turnId: TurnId; handle: Handle } | null;
};

/** What a line says: a quoted target, or the whole running turn (16.12.5). */
export type LineLink =
  | { intent: "reply" | "steer" | "continue"; quote: string }
  | { intent: "steer"; scope: "turn" };

const WORDS = 8;
const CLUSTERS = 60;
/** `parseMessageLink` keeps a stored quote of at most this many, `…` included. */
const CODE_POINTS = 200;
const graphemes = new Intl.Segmenter(undefined, { granularity: "grapheme" });

/**
 * The first words of `text` on one line (design 16.7): at most 8 words, 60
 * grapheme clusters and 200 code points, with `…` when anything was cut.
 * Plain text: `intentLine` escapes it where it writes a line.
 */
export function excerpt(text: string): string {
  const words = text.replace(/[\s\p{Cc}]+/gu, " ").trim().split(" ");
  const kept = words.slice(0, WORDS).join(" ");
  const clusters = Array.from(graphemes.segment(kept), (part) => part.segment);
  if (
    words.length <= WORDS &&
    clusters.length <= CLUSTERS &&
    [...kept].length <= CODE_POINTS
  )
    return kept;
  let cut = "";
  let points = 0;
  for (const cluster of clusters.slice(0, CLUSTERS)) {
    const size = [...cluster].length;
    if (points + size > CODE_POINTS - 1) break;
    cut += cluster;
    points += size;
  }
  return `${cut}…`;
}

/**
 * The one line before a message that names what it refers to (design 16.7,
 * 16.12.5). `ref` is a value `get_message` takes; a whole-turn steer has none.
 */
export function intentLine(link: LineLink, ref?: string): string {
  if ("scope" in link) return "[steer → whole turn]";
  const head = link.intent === "continue" ? "re" : `${link.intent} →`;
  const quote = link.quote
    ? `"${link.quote.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`
    : "(no text)";
  return `[${head} ${ref ? `${ref} · ` : ""}${quote}]`;
}

/**
 * How the reader names a Mono row's target: its handle when the target holds
 * one in the reader's own session and turn, else its message id. Handles
 * restart in each turn, so any other `#n` would name another message.
 */
export function targetRef(
  target: ReplyTarget,
  reader: { sessionId: SessionId; turnId: TurnId },
): string {
  return target.slot &&
    target.handledBySession === reader.sessionId &&
    target.slot.turnId === reader.turnId
    ? target.slot.handle
    : target.messageId;
}
