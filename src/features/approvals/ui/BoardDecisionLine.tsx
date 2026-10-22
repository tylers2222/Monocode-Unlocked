import { Fragment, type ReactNode } from "react";
import { CheckCircle, ListEnd } from "../../../shared/ui/icons";
import { quoteText } from "../../sessions/ui/composerIntent";
import { SentNotice } from "../../sessions/ui/SentNotice";
import type { BoardDecision, Verdict } from "../model/boardDecision";

const VERDICT: Record<Verdict, { label: string; tone: string }> = {
  approved: { label: "Approved", tone: "text-emerald-400" },
  rejected: { label: "Rejected", tone: "text-red-300" },
  changes_requested: { label: "Changes requested", tone: "text-amber-400" },
};

/**
 * Text a Giga or the user wrote, in quotes. Bidi controls are dropped and
 * the text keeps its own direction, so it cannot reorder the line around it.
 */
function Quoted({ text }: { text: string }) {
  return (
    <>
      “<span dir="auto">{quoteText(text)}</span>”
    </>
  );
}

function Headline({ decision }: { decision: BoardDecision }) {
  const verdict = VERDICT[decision.verdict];
  return (
    <>
      <span className={verdict.tone}>{verdict.label}</span> ·{" "}
      <span className="font-mono text-[11.5px] text-content/65">
        APR-{decision.number}
      </span>{" "}
      <Quoted text={decision.heading} />
    </>
  );
}

/** Frame A·7: a sent board decision, drawn as the transcript's sent notice. */
export function SentDecision({
  decision,
  sentText,
  agentName,
  embedded,
}: {
  decision: BoardDecision;
  sentText: string;
  agentName?: string;
  embedded: boolean;
}) {
  return (
    <SentNotice
      from="Decision from the approvals board"
      icon={
        <CheckCircle
          className={`mt-px size-3.5 shrink-0 ${VERDICT[decision.verdict].tone}`}
        />
      }
      sentText={sentText}
      agentName={agentName}
      embedded={embedded}
    >
      <Headline decision={decision} />
      {decision.chosen.length > 0 ? " · chose " : null}
      {decision.chosen.map((label, index) => (
        <Fragment key={label}>
          {index > 0 ? ", " : null}
          <Quoted text={label} />
        </Fragment>
      ))}
      {decision.note ? (
        <span className="mt-0.5 block text-content/75">
          Your comment: <Quoted text={decision.note} />
        </span>
      ) : null}
    </SentNotice>
  );
}

/**
 * Frame A·7: a board decision that waits for its Giga's turn to end.
 * `remove` is the queue's own remove control; `children` is its failure
 * line when the send failed (A·7b).
 */
export function QueuedDecisionRow({
  decision,
  remove,
  children,
}: {
  decision: BoardDecision;
  remove: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div data-queued-decision className="flex gap-2 py-1.5 text-[12px]">
      <ListEnd className="mt-px size-3.5 shrink-0 text-content/60" />
      <div className="min-w-0 flex-1">
        <div className="text-[11px] text-content/70">
          Decision from the approvals board · not sent yet
        </div>
        <p className="mt-px text-[12.5px] leading-[18px] text-content/85">
          <Headline decision={decision} />
        </p>
        {children}
      </div>
      {remove}
    </div>
  );
}
