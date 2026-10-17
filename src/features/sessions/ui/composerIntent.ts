import { canSteerHarness } from "../../../integrations/harness/core/registry";
import type { FollowUpBehavior } from "../../settings/model/settings";
import {
  composerTarget,
  excerpt,
  type ComposerTarget,
  type RunningItem,
} from "../model/messageIntent";
import type {
  Block,
  ComposerTurnOptions,
  Session,
  TargetLink,
} from "../model/session";
import type { TurnId } from "../../monos/model/monoMessages";

/** What the user did with the bar for the message being typed (design 16.8). */
export type BarChoice =
  | { kind: "default" }
  /** Picked by hovering a message and clicking Reply or Steer. */
  | { kind: "picked"; link: TargetLink }
  /** ✕: the message is `new` and never steers. */
  | { kind: "cleared" };

export const NO_CHOICE: BarChoice = { kind: "default" };

/**
 * What the bar shows (design 16.12.6), plus `ended`: the bar showed `turn`
 * for this draft and the turn ended before Send (doop E10, decision 536);
 * and `held`: a picked steer while the composer's mode (`mode`, the pill's
 * label) sends the text to the queue, so the mode and the steer cannot both
 * hold.
 */
export type ComposerBar =
  | ComposerTarget
  | { kind: "ended" }
  | { kind: "held"; link: TargetLink; mode: string };

export function composerBar(
  running: readonly RunningItem[],
  choice: BarChoice,
  steering: boolean,
  /** The bar showed `turn` for the text in the field now. */
  sawTurn: boolean,
  /** The composer's mode that sends the text to the queue (Plan, Orchestrator, Operator). */
  queuedBy?: string,
): ComposerBar {
  switch (choice.kind) {
    case "cleared":
      return { kind: "none" };
    case "picked": {
      const target = composerTarget(running, choice.link, steering);
      return queuedBy &&
        target.kind === "picked" &&
        target.link.intent === "steer"
        ? { kind: "held", link: target.link, mode: queuedBy }
        : target;
    }
    case "default": {
      const target = composerTarget(running, null, steering);
      return target.kind === "none" && sawTurn && !queuedBy
        ? { kind: "ended" }
        : target;
    }
  }
}

/** What a send passes with its text. */
export type BarSend = Pick<ComposerTurnOptions, "link" | "waitForIdle">;

/**
 * The options a send carries, or `null` when the bar blocks it: a steer is
 * never sent as a guess (design 16.8). `turn` passes no link, because the app
 * stamps the whole-turn link when the text joins the turn (16.12.4). `none`
 * while steering means the user pressed ✕, so the message waits for the
 * turn end and never joins it.
 */
export function barSend(
  bar: Exclude<ComposerBar, { kind: "gone" | "ended" | "held" }>,
  steering: boolean,
): BarSend;
export function barSend(bar: ComposerBar, steering: boolean): BarSend | null;
export function barSend(bar: ComposerBar, steering: boolean): BarSend | null {
  switch (bar.kind) {
    case "none":
      return steering ? { waitForIdle: true } : {};
    case "turn":
      return {};
    case "picked":
      return { link: bar.link };
    case "gone":
    case "ended":
    case "held":
      return null;
  }
}

/**
 * The running turn of `session`, read from its blocks: the turn id of the
 * user block that started last. The app's own record of it lives in one
 * window only, and the floating chat is another window. `undefined` while
 * idle, or when the opener carries no turn id.
 */
export function runningTurnOf(
  session: Pick<Session, "busy" | "blocks">,
): TurnId | undefined {
  if (!session.busy) return undefined;
  let opener: Block | undefined;
  for (const block of session.blocks)
    if (
      block.role === "user" &&
      block.startedAt != null &&
      (!opener || block.startedAt >= opener.startedAt!)
    )
      opener = block;
  return opener?.turnId;
}

/**
 * Whether the busy rule would steer text typed now (design 16.8, the
 * busy branch of `submitSession`): a busy Mono chat whose harness can steer,
 * or a busy session whose follow-up setting is `steer` or whose agent only
 * waits on background work.
 */
export function steersTyped(
  session: Pick<
    Session,
    "busy" | "harness" | "worktreePreparing" | "backgroundTasks"
  >,
  mono: boolean,
  followUp: FollowUpBehavior,
): boolean {
  if (!session.busy || !canSteerHarness(session.harness)) return false;
  // M2 hook (PR I6 r1): when PR #19 lands, return false here while its model
  // predicate says the queue would hold new text behind one of the user's
  // own waiting messages, and hide the hover Steer in that state too.
  if (mono) return true;
  if (session.worktreePreparing) return false;
  return session.backgroundTasks?.length ? true : followUp === "steer";
}

/** A link that names `block`, with its first words as the quote. */
export function linkTo(
  intent: TargetLink["intent"],
  block: Pick<Block, "id" | "text">,
): TargetLink {
  return { intent, replyTo: block.id, quote: excerpt(block.text) };
}

/** How each intent reads in the bar, the label menu and the sent quote. */
export const INTENT_LOOK: Record<
  TargetLink["intent"],
  { glyph: string; bar: string; sent: string }
> = {
  steer: { glyph: "↪", bar: "Steering", sent: "Steer" },
  reply: { glyph: "↩", bar: "Reply", sent: "Reply" },
  continue: { glyph: "→", bar: "Continuing", sent: "Continuing" },
};

/**
 * Bidirectional controls (embeddings, overrides, isolates and the marks)
 * reorder the text around them, so a quote of someone else's message could
 * show its words out of order (PR I4 review n2). The quote drops them.
 */
const BIDI_CONTROLS = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

export function quoteText(quote: string): string {
  return quote.replace(BIDI_CONTROLS, "");
}
