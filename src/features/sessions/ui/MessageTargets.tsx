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
      {intents.map((intent) => (
        <button
          key={intent}
          type="button"
          data-pick-intent={intent}
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
          <span aria-hidden>{INTENT_LOOK[intent].glyph}</span>
          {intent === "steer" ? "Steer" : "Reply"}
        </button>
      ))}
    </span>
  );
}
