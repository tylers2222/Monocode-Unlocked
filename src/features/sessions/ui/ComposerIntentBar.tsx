import { useEffect, useRef, useState } from "react";
import { X } from "../../../shared/ui/icons";
import type { RunningItem } from "../model/messageIntent";
import type { TargetLink } from "../model/session";
import {
  barSend,
  composerBar,
  INTENT_LOOK,
  NO_CHOICE,
  quoteText,
  type BarChoice,
  type ComposerBar,
} from "./composerIntent";

/** What a pane tells its composer about what the message refers to (design 16.8). */
export type ComposerIntent = {
  running: readonly RunningItem[];
  /** The busy rule would steer the typed text into the running turn. */
  steering: boolean;
  choice: BarChoice;
  onChoiceChange: (choice: BarChoice) => void;
  /** The Mono a whole-turn steer names; absent for a project session (Q-W1). */
  monoName?: string;
};

/**
 * The bar of one composer. `typing` is whether the field holds text or
 * files. `queues` is true when the composer's own mode (plan, orchestrate,
 * /operator) sends the text to the queue, so it cannot steer.
 */
export function useComposerBar(
  intent: ComposerIntent | undefined,
  typing: boolean,
  queues = false,
) {
  const steering = !!intent?.steering && !queues;
  const [sawTurn, setSawTurn] = useState(false);
  const bar: ComposerBar = intent
    ? composerBar(intent.running, intent.choice, steering, sawTurn && typing)
    : { kind: "none" };
  const showingTurn = bar.kind === "turn" && typing;
  const cleared = intent?.choice.kind === "cleared";
  const onChoiceChange = intent?.onChoiceChange;
  useEffect(() => {
    if (typing) {
      if (showingTurn) setSawTurn(true);
      return;
    }
    // ✕ belongs to the text it cleared; the next message starts fresh.
    setSawTurn(false);
    if (cleared) onChoiceChange?.(NO_CHOICE);
  }, [typing, showingTurn, cleared, onChoiceChange]);
  return {
    bar,
    /** The options a send carries; `null` while the bar blocks Send. */
    send: barSend(bar, steering),
    /** "Send as new": the same text as a `new` message. */
    asNew: barSend({ kind: "none" }, steering),
    /** The bar after a send the app took. */
    sent() {
      setSawTurn(false);
      if (intent && intent.choice.kind !== "default")
        intent.onChoiceChange(NO_CHOICE);
    },
  };
}

/**
 * The line above the composer's field that names what the message refers
 * to, with ✕ to make it a new message (design 16.8, 16.12.6, doop E1, E6,
 * E8, E10). `turn` and `ended` show only while the user types.
 */
export function ComposerIntentBar({
  bar,
  intent,
  typing,
  onSendAsNew,
}: {
  bar: ComposerBar;
  intent: ComposerIntent;
  typing: boolean;
  onSendAsNew: () => void;
}) {
  if (bar.kind === "none") return null;
  if ((bar.kind === "turn" || bar.kind === "ended") && !typing) return null;
  const clear = () => intent.onChoiceChange({ kind: "cleared" });
  const scope = (
    <span className="composer-intent-target">
      everything{" "}
      <span className="composer-intent-name">
        {intent.monoName ?? "this session"}
      </span>{" "}
      is doing
    </span>
  );
  return (
    <div
      role="group"
      aria-label="Message target"
      data-composer-intent={bar.kind}
      className="composer-intent-bar popover-surface"
    >
      {bar.kind === "picked" || bar.kind === "gone" ? (
        <>
          <IntentMenu
            link={bar.link}
            running={intent.running}
            onPick={(link) => intent.onChoiceChange({ kind: "picked", link })}
          />
          <span aria-hidden className="composer-intent-dot">
            ·
          </span>
          <span className="composer-intent-target" dir="auto">
            {quoteText(bar.link.quote)}
          </span>
        </>
      ) : (
        <>
          <span className="composer-intent-label" data-intent="steer">
            <span aria-hidden className="composer-intent-glyph">
              {INTENT_LOOK.steer.glyph}
            </span>
            {INTENT_LOOK.steer.bar}
          </span>
          <span aria-hidden className="composer-intent-dot">
            ·
          </span>
          {scope}
        </>
      )}
      {bar.kind === "gone" || bar.kind === "ended" ? (
        <>
          <span
            className="composer-intent-chip"
            title="That work finished. Edit the message or send it as new."
          >
            {bar.kind === "gone" ? "Not running" : "Nothing running"}
          </span>
          <button
            type="button"
            className="composer-intent-act"
            onMouseDown={(event) => event.preventDefault()}
            onClick={onSendAsNew}
          >
            Send as new
          </button>
        </>
      ) : null}
      <button
        type="button"
        title="Send as a new message"
        aria-label="Clear message target"
        className="composer-intent-clear"
        onMouseDown={(event) => event.preventDefault()}
        onClick={clear}
      >
        <X className="size-3" strokeWidth={1.75} />
      </button>
    </div>
  );
}

/** The bar's label: a menu that switches Reply, Continuing and, while the message's work runs, Steering. */
function IntentMenu({
  link,
  running,
  onPick,
}: {
  link: TargetLink;
  running: readonly RunningItem[];
  onPick: (link: TargetLink) => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);
  const intents: TargetLink["intent"][] = running.some(
    (item) => item.blockId === link.replyTo,
  )
    ? ["steer", "reply", "continue"]
    : ["reply", "continue"];
  const look = INTENT_LOOK[link.intent];
  return (
    <span ref={root} className="relative flex shrink-0">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        data-intent={link.intent}
        className="composer-intent-label"
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") setOpen(false);
        }}
      >
        <span aria-hidden className="composer-intent-glyph">
          {look.glyph}
        </span>
        {look.bar}
      </button>
      {open ? (
        <span role="menu" className="composer-intent-menu popover-surface">
          {intents.map((intent) => (
            <button
              key={intent}
              type="button"
              role="menuitemradio"
              aria-checked={intent === link.intent}
              data-intent={intent}
              className="composer-intent-option"
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => {
                setOpen(false);
                onPick({ ...link, intent });
              }}
            >
              <span aria-hidden className="composer-intent-glyph">
                {INTENT_LOOK[intent].glyph}
              </span>
              {INTENT_LOOK[intent].bar}
            </button>
          ))}
        </span>
      ) : null}
    </span>
  );
}
