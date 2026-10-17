import { canSteerHarness } from "../../../integrations/harness/core/registry";
import {
  parseMessageId,
  type Handle,
  type MessageId,
  type SessionId,
  type TurnId,
} from "../../monos/model/monoMessages";
import { canSteerNow } from "./messageQueue";
import {
  WHOLE_TURN,
  type MessageLink,
  type Session,
  type TargetLink,
} from "./session";

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
/**
 * Built on first use, so a webview without `Intl.Segmenter` (Safari before
 * 14.1) can still load every module that imports this one. There each code
 * point counts as a cluster: a ZWJ sequence may be cut, but the excerpt stays
 * within its bounds. `null` once known missing.
 */
let graphemes: Intl.Segmenter | null | undefined;

/**
 * The first words of `text` on one line (design 16.7): at most 8 words, 60
 * grapheme clusters and 200 code points, with `…` when anything was cut.
 * Plain text: `intentLine` escapes it where it writes a line.
 */
export function excerpt(text: string): string {
  const words = text
    .replace(/[\s\p{Cc}]+/gu, " ")
    .trim()
    .split(" ");
  const kept = words.slice(0, WORDS).join(" ");
  if (graphemes === undefined)
    graphemes =
      typeof Intl.Segmenter === "function"
        ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
        : null;
  const clusters = graphemes
    ? Array.from(graphemes.segment(kept), (part) => part.segment)
    : Array.from(kept);
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

function isLinkIntent(raw: unknown): raw is TargetLink["intent"] {
  return raw === "reply" || raw === "steer" || raw === "continue";
}

/**
 * A stored link that names one message (design 16.3): intent reply, steer
 * or continue, a target id of 1 to 128 characters and a quote of at most
 * 200 code points. Anything else is no link, so the message is intent `new`.
 */
export function parseTargetLink(raw: unknown): TargetLink | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const { intent, replyTo, quote, scope } = raw as Record<string, unknown>;
  if (
    scope !== undefined ||
    !isLinkIntent(intent) ||
    typeof replyTo !== "string" ||
    replyTo.length < 1 ||
    replyTo.length > 128 ||
    typeof quote !== "string" ||
    [...quote].length > CODE_POINTS
  )
    return undefined;
  return { intent, replyTo, quote };
}

/**
 * A stored block's link (design 16.3, 16.12.4): a link that names one
 * message, or exactly the whole-turn steer. A link with both, or a scope on
 * another intent, is dropped.
 */
export function parseMessageLink(raw: unknown): MessageLink | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const { intent, replyTo, quote, scope } = raw as Record<string, unknown>;
  if (scope === undefined) return parseTargetLink(raw);
  return scope === "turn" &&
    intent === "steer" &&
    replyTo === undefined &&
    quote === undefined
    ? { intent: "steer", scope: "turn" }
    : undefined;
}

/**
 * What the provider reads for a user's message (design 16.7): the line,
 * then the text. The ref is the target's message id when it is a Mono
 * message, which `get_message` takes; a plain block has none.
 */
export function linkedText(link: MessageLink | undefined, text: string) {
  if (!link) return text;
  const ref = link.scope ? undefined : parseMessageId(link.replyTo);
  return `${intentLine(link, ref)}\n${text}`;
}

/** A message whose work runs now, which a steer may name (design 16.8). */
export type RunningItem = { blockId: string; quote: string };

/**
 * The user blocks and Mono bubbles of running turn `turnId` (design 16.8):
 * the opener and every block that joined it. Empty unless a steer can go in
 * now. A queued bubble has no turn id until its work starts.
 */
export function runningItems(
  session: Session,
  turnId: TurnId | undefined,
): RunningItem[] {
  if (!turnId || !canSteerNow(session) || !canSteerHarness(session.harness))
    return [];
  return session.blocks
    .filter(
      (block) =>
        block.role === "user" &&
        !block.draft &&
        !block.internal &&
        block.turnId === turnId,
    )
    .map((block) => ({ blockId: block.id, quote: excerpt(block.text) }));
}

/** What the composer's bar shows (design 16.12.6). */
export type ComposerTarget =
  | { kind: "none" }
  | { kind: "picked"; link: TargetLink }
  /** No pick while steering: everything the session is doing. */
  | { kind: "turn" }
  /** A picked steer whose work ended: offer to send it as new. */
  | { kind: "gone"; link: TargetLink };

export function composerTarget(
  running: readonly RunningItem[],
  picked: TargetLink | null,
  steering: boolean,
): ComposerTarget {
  if (!picked) return steering ? { kind: "turn" } : { kind: "none" };
  if (
    picked.intent === "steer" &&
    !running.some((item) => item.blockId === picked.replyTo)
  )
    return { kind: "gone", link: picked };
  return { kind: "picked", link: picked };
}

/** Why a queued steer failed: its turn ended before it could join (16.8). */
export const STEER_TARGET_GONE =
  "That work finished. Edit the message or send it as new.";

/** Where a user's message goes (design 16.8, 16.12.4). */
export type UserSend =
  /** A steer whose work is no longer running: nothing is sent. */
  | { kind: "target_gone" }
  /** Starts a turn now. */
  | { kind: "open"; link?: TargetLink }
  /** Joins the running turn now; `link` is what its block records. */
  | { kind: "join"; link?: MessageLink }
  /**
   * Waits for the turn end. `waitForIdle` when the link or the user chose
   * it, so the message never steers; otherwise today's busy rule queued it.
   */
  | { kind: "queue"; link?: TargetLink; waitForIdle: boolean };

export function planUserSend(input: {
  link: TargetLink | undefined;
  /** The user cleared the steer (✕). */
  waitForIdle: boolean;
  /** The user's own text, not a Mono row handed off through the same path. */
  own: boolean;
  /** Idle, or the running turn's items and whether today's busy rule steers. */
  session: "idle" | { running: readonly RunningItem[]; steers: boolean };
}): UserSend {
  const { link, session } = input;
  if (link?.intent === "steer")
    return session !== "idle" &&
      session.running.some((item) => item.blockId === link.replyTo)
      ? { kind: "join", link }
      : { kind: "target_gone" };
  if (session === "idle") return { kind: "open", link };
  if (link || input.waitForIdle)
    return { kind: "queue", link, waitForIdle: true };
  if (!session.steers) return { kind: "queue", waitForIdle: false };
  return input.own ? { kind: "join", link: WHOLE_TURN } : { kind: "join" };
}
