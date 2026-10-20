import { CheckCircle, ListEnd } from "../../../shared/ui/icons";
import { SentNotice } from "../../sessions/ui/SentNotice";
import type { BoardDecision, Verdict } from "../model/boardDecision";

const VERDICT: Record<Verdict, { label: string; tone: string }> = {
  approved: { label: "Approved", tone: "text-emerald-400" },
  rejected: { label: "Rejected", tone: "text-red-300" },
  changes_requested: { label: "Changes requested", tone: "text-amber-400" },
};

const quoted = (text: string) => `“${text}”`;

function Headline({ decision }: { decision: BoardDecision }) {
  const verdict = VERDICT[decision.verdict];
  return (
    <>
      <span className={verdict.tone}>{verdict.label}</span> ·{" "}
      <span className="font-mono text-[11.5px] text-content/65">
        APR-{decision.number}
      </span>{" "}
      {quoted(decision.heading)}
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
      {decision.chosen.length > 0
        ? ` · chose ${decision.chosen.map(quoted).join(", ")}`
        : null}
      {decision.note ? (
        <span className="mt-0.5 block text-content/75">
          Your comment: {quoted(decision.note)}
        </span>
      ) : null}
    </SentNotice>
  );
}

/** Frame A·7: a board decision that waits for its Giga's turn to end. */
export function QueuedDecisionRow({
  decision,
  error,
}: {
  decision: BoardDecision;
  error?: string;
}) {
  return (
    <div data-queued-decision className="flex gap-2 py-1.5">
      <ListEnd className="mt-px size-3.5 shrink-0 text-content/60" />
      <div className="min-w-0 flex-1">
        <div className="text-[11px] text-content/70">
          Decision from the approvals board · not sent yet
        </div>
        <p className="mt-px text-[12.5px] leading-[18px] text-content/85">
          <Headline decision={decision} />
        </p>
        {error ? (
          <p role="alert" className="text-[12px] text-red-400">
            {error}
          </p>
        ) : null}
      </div>
    </div>
  );
}
