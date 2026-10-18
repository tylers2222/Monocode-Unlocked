import { createContext, useContext, type CSSProperties } from "react";
import type { Block, TargetLink } from "../model/session";
import { INTENT_LOOK, linkTo } from "./composerIntent";

/** What a transcript needs to let the user pick a message to answer or steer (design 16.8). */
export type MessageTargets = {
  /** Blocks of the running turn, which a steer may name. */
  running: ReadonlySet<string>;
  pick: (link: TargetLink) => void;
};

export const MessageTargetsContext = createContext<MessageTargets | null>(null);

/**
 * The hover actions that pick `block` as the composer's target, iMessage
 * style: Reply, and Steer while the block's work runs. Continue is chosen
 * from the bar's label menu (Q-I2).
 */
export function PickTargetActions({
  block,
  className,
  style,
}: {
  block: Pick<Block, "id" | "text">;
  className?: string;
  style?: CSSProperties;
}) {
  const targets = useContext(MessageTargetsContext);
  if (!targets) return null;
  const intents: TargetLink["intent"][] = targets.running.has(block.id)
    ? ["reply", "steer"]
    : ["reply"];
  return (
    <span className={className} style={style}>
      {intents.map((intent) => {
        const label = intent === "steer" ? "Steer" : "Reply";
        return (
          <button
            key={intent}
            type="button"
            data-pick-intent={intent}
            aria-label={label}
            title={
              intent === "steer"
                ? "Steer the work this message started"
                : "Reply to this message"
            }
            onClick={(event) => {
              event.stopPropagation();
              targets.pick(linkTo(intent, block));
            }}
            className="pick-target-action"
          >
            <span aria-hidden className="pick-target-glyph">
              {INTENT_LOOK[intent].glyph}
            </span>
            <span className="pick-target-label">{label}</span>
          </button>
        );
      })}
    </span>
  );
}

/**
 * A message stacked under another of the user's sits too close to it for a
 * pill on its top edge, which would cover the message above and take its
 * pointer (PR I6 review r2 m3). So the pill sits beside the bubble, in the
 * room the right-aligned bubble leaves in its row. With its labels it may not
 * fit there; then it shows only the glyphs, which fit the row's left gutter.
 * The row calls this on hover and focus, before the pill shows.
 */
export function fitPillBeside({
  currentTarget: row,
}: {
  currentTarget: HTMLElement;
}) {
  const pill = row.querySelector<HTMLElement>(".pick-target-pill-beside");
  const zone = pill?.offsetParent;
  if (!pill || !zone) return;
  delete pill.dataset.glyphs;
  const room =
    zone.getBoundingClientRect().left - row.getBoundingClientRect().left;
  if (pill.offsetWidth + PILL_GAP > room) pill.dataset.glyphs = "";
}

/** The space between the pill and the bubble beside it, as in index.css. */
const PILL_GAP = 4;
