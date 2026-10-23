import { quoteText } from "../../sessions/ui/composerIntent";
import { GigaText } from "./GigaText";
import { ago, type Approval } from "../model/approvals";

const PILL =
  "inline-flex h-5 items-center gap-1.5 rounded-[5px] px-[7px] text-[11px] font-semibold whitespace-nowrap";

function Urgency({ card }: { card: Approval }) {
  return card.urgency === "blocking" ? (
    <span className={`${PILL} bg-red-400/13 text-red-300`}>
      <i className="size-1.5 rounded-full bg-red-500" />
      Blocking
    </span>
  ) : (
    <span className={`${PILL} bg-content/7 text-content/72`}>
      <i className="size-1.5 rounded-full border-[1.5px] border-content/55" />
      Not blocking
    </span>
  );
}

function CardHead({ card, time }: { card: Approval; time: string }) {
  return (
    <>
      <div className="flex h-5 items-center gap-2 text-[11px]">
        <Urgency card={card} />
        <span className="font-mono text-content/55">APR-{card.number}</span>
        <span className="ml-auto whitespace-nowrap text-content/60">
          {time}
        </span>
      </div>
      <GigaText
        text={card.heading}
        className="mt-2 block text-base leading-[22px] font-semibold"
      />
    </>
  );
}

export function OpenCard({ card, now }: { card: Approval; now: number }) {
  return (
    <article
      aria-label={quoteText(card.heading)}
      className={`overflow-hidden rounded-[10px] border bg-content/3 ${
        card.urgency === "blocking" ? "border-red-300/30" : "border-content/11"
      }`}
    >
      <div className="px-4 pt-3.5 pb-4">
        <CardHead card={card} time={ago(card.createdAt, now)} />
        <GigaText
          text={card.summary}
          className="mt-3 block text-[13px] leading-5 text-content/82"
        />
      </div>
    </article>
  );
}
