import {
  parseMessageId,
  type Handle,
  type MessageId,
  type SessionId,
  type TurnId,
} from "../../monos/model/monoMessages";
import {
  WHOLE_TURN,
  type Block,
  type MessageLink,
  type QueuedMessage,
  type Session,
  type TargetLink,
} from "./session";
import { isOwnQueuedMessage, type RunningTurns } from "./messageQueue";

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
 * the opener and every block that joined it, whether or not the turn can
 * take a steer this moment. A queued bubble has no turn id until its work
 * starts, and a hidden or unsent block is never a target.
 */
export function runningItems(
  session: Session,
  turnId: TurnId | undefined,
): RunningItem[] {
  if (!turnId) return [];
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

/** What a submit says about who wrote its text and what it names. */
export type SendOrigin = {
  /** What the user's message refers to (design 16.8). */
  link?: TargetLink;
  /** The user cleared the steer (✕). */
  waitForIdle?: true;
  /** Text the orchestrator, CI repair or a CLI send wrote. */
  managed?: boolean;
  ciRepair?: unknown;
  appRequestId?: string;
};

/**
 * The text is the user's own: typed in the composer, not an orchestrator
 * turn, a CI repair, a CLI send (decision 553) or a queued item the user did
 * not type (`isOwnQueuedMessage`). A Mono row's hand-off and a completion
 * report always come with their item. Only the user's own text gets the
 * user's link (design 16.12.4). The habit and automation runs pass no marker
 * and reach only an idle session, which gets no line without a link.
 */
function isOwnText(
  origin: SendOrigin | undefined,
  item: QueuedMessage | undefined,
): boolean {
  return (
    !(origin?.managed || origin?.ciRepair || origin?.appRequestId) &&
    (!item || isOwnQueuedMessage(item))
  );
}

/**
 * Where a send goes. `opens`: the session is idle and the send opens turn
 * `opens`. Otherwise turn `running` runs (its id, when this window knows it)
 * with `items`, and the send steers it `now`, or is queued for the Mono
 * pump to steer `later`, or, by today's busy rule, waits for the turn end.
 */
export type SendRoute = IdleRoute | BusyRoute;
type IdleRoute = { opens: TurnId };
type BusyRoute = {
  running: TurnId | undefined;
  items: readonly RunningItem[];
  steers: "now" | "later" | "no";
};

/** The route of a send into `session` while it is busy with the turn `turns` holds for it. */
export function busyRoute(
  session: Session,
  turns: RunningTurns,
  steers: BusyRoute["steers"],
): SendRoute {
  const running = session.busy ? turns.get(session.id) : undefined;
  return { running, items: runningItems(session, running), steers };
}

/** The fields of a user block that place it in a turn and say what it names. */
type TurnBlock = Pick<Block, "turnId" | "link">;

/** A steer whose work is no longer running: nothing is sent. */
type TargetGone = { kind: "target_gone" };
/** Opens the turn or joins the running one; the block records `block`, the provider reads `text`. */
type Sent = { kind: "open" | "join"; block: TurnBlock; text: string };
/** Waits in the queue with these fields. */
type Queued = {
  kind: "queue";
  item: Pick<QueuedMessage, "link" | "waitForIdle" | "typedInTurn">;
};
/**
 * The queued item this send delivers would only be queued again (PR I6
 * review r3 m2, n4): it keeps its place and nothing is sent, so its text is
 * never queued twice.
 */
type Stays = { kind: "stays" };

/** Where a user's message goes (design 16.8, 16.12.4). */
export type UserSend = TargetGone | Sent | Queued | Stays;

type PlanInput<R extends SendRoute> = {
  origin: SendOrigin | undefined;
  /** The queued item this send delivers. */
  item: QueuedMessage | undefined;
  /** What the provider reads, before any line. */
  text: string;
  route: R;
};

export function planUserSend(
  input: PlanInput<IdleRoute>,
): TargetGone | (Sent & { kind: "open" });
export function planUserSend(input: PlanInput<SendRoute>): UserSend;
export function planUserSend(input: PlanInput<SendRoute>): UserSend {
  const plan = planSend(input);
  return plan.kind === "queue" && input.item ? { kind: "stays" } : plan;
}

function planSend(input: PlanInput<SendRoute>): TargetGone | Sent | Queued {
  const { origin, route } = input;
  const own = isOwnText(origin, input.item);
  const link = own ? (origin?.link ?? input.item?.link) : undefined;
  const sent = (
    kind: "open" | "join",
    turnId: TurnId | undefined,
    stamp: MessageLink | undefined,
  ): Sent => ({
    kind,
    block: { ...(turnId ? { turnId } : {}), ...(stamp ? { link: stamp } : {}) },
    text: linkedText(stamp, input.text),
  });
  if ("opens" in route)
    return link?.intent === "steer"
      ? { kind: "target_gone" }
      : sent("open", route.opens, link);
  // A message queued for later may steer only the turn it was typed in.
  const later = route.running
    ? { typedInTurn: route.running }
    : { waitForIdle: true };
  if (link?.intent === "steer") {
    if (!route.items.some((item) => item.blockId === link.replyTo))
      return { kind: "target_gone" };
    return route.steers === "later"
      ? { kind: "queue", item: { link, ...later } }
      : sent("join", route.running, link);
  }
  if (link || origin?.waitForIdle)
    return {
      kind: "queue",
      item: { ...(link ? { link } : {}), waitForIdle: true },
    };
  if (route.steers === "no") return { kind: "queue", item: {} };
  if (route.steers === "later") return { kind: "queue", item: later };
  // Busy with no turn to join, as during a manual compaction: a steer would
  // reach no turn, so the text waits for the session to go idle.
  if (!route.running) return { kind: "queue", item: { waitForIdle: true } };
  return sent("join", route.running, own ? WHOLE_TURN : undefined);
}
