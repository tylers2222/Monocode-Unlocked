import type { MessageLink } from "../model/session";
import { INTENT_LOOK, quoteText } from "./composerIntent";

/**
 * The light grey quote above a sent message that names what it refers to
 * (design 16.8, 16.12.6, doop E5, E9): the intent's label, then the first
 * words of the target. A whole-turn steer names no message, so it has the
 * label only.
 */
export function MessageQuote({ link }: { link: MessageLink }) {
  const look = INTENT_LOOK[link.intent];
  return (
    <div className="message-quote" data-intent={link.intent}>
      <div className="message-quote-label">
        <span aria-hidden className="message-quote-glyph">
          {look.glyph}
        </span>
        {link.scope ? `${look.sent} · whole turn` : look.sent}
      </div>
      {link.scope ? null : (
        <div className="message-quote-text" dir="auto">
          {quoteText(link.quote)}
        </div>
      )}
    </div>
  );
}
