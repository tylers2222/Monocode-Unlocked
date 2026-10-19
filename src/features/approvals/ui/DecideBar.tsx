import type { ReactNode } from "react";
import { Check, CircleAlert, PenLine, X } from "../../../shared/ui/icons";
import {
  filerRemoved,
  type Approval,
  type ApproveButton,
} from "../model/approvals";
import type { Verdict } from "../model/boardDecision";

const BUTTON =
  "inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-[11px] text-xs font-medium whitespace-nowrap";

function Keys({
  keys,
  joiner = "",
  label,
}: {
  keys: string[];
  joiner?: string;
  label: string;
}) {
  return (
    <span className="flex items-center gap-1">
      {keys.map((key, index) => (
        <span key={index} className="contents">
          {index > 0 ? joiner : null}
          <kbd className="rounded border border-b-2 border-content/18 px-1 font-sans text-[10.5px] leading-[15px] text-content/75">
            {key}
          </kbd>
        </span>
      ))}
      {label}
    </span>
  );
}

function KeyHints({ rows, approve }: { rows: number; approve: ApproveButton }) {
  return (
    <div className="mt-2 hidden items-center gap-2.5 text-[11px] text-content/55 group-focus-within:flex">
      {rows > 0 ? (
        <>
          <Keys keys={["↑", "↓"]} label="move" />
          <Keys keys={["Space"]} label="tick" />
          <Keys
            keys={rows === 1 ? ["1"] : ["1", String(Math.min(rows, 9))]}
            joiner="–"
            label="tick by number"
          />
        </>
      ) : null}
      <Keys keys={["⌘", "↩"]} label="approve" />
      {approve.enabled ? null : (
        <span className="ml-auto">Tick an option to approve</span>
      )}
    </div>
  );
}

function NothingSent() {
  return (
    <div className="mt-2 flex items-center gap-1.5 text-[11.5px] text-amber-400">
      <CircleAlert className="size-3" />
      Deciding closes the card here. Nothing is sent.
    </div>
  );
}

export function DecideBar({
  card,
  comment,
  onComment,
  approve,
  onDecide,
}: {
  card: Approval;
  comment: string;
  onComment: (comment: string) => void;
  approve: ApproveButton;
  onDecide: (verdict: Verdict) => void;
}) {
  const rows = card.options.length;
  const action = (verdict: Verdict, className: string, label: ReactNode) => (
    <button
      type="button"
      onClick={() => onDecide(verdict)}
      className={`${BUTTON} ${className}`}
    >
      {label}
    </button>
  );
  return (
    <div className="border-t border-content/7 bg-content/2 px-4 py-3">
      <div className="flex items-center gap-2">
        <textarea
          aria-label="Comment"
          dir="auto"
          rows={1}
          value={comment}
          onChange={(event) => onComment(event.target.value)}
          placeholder="Add a comment (optional)"
          className={`min-w-0 flex-1 resize-none rounded-md border border-content/12 bg-transparent px-2.5 py-[5px] text-xs leading-[18px] text-content/88 outline-none placeholder:text-content/50 focus:h-14 focus:border-accent/55 focus:ring-3 focus:ring-accent/15 ${
            comment ? "h-14" : "h-7"
          }`}
        />
        {action(
          "rejected",
          "border border-red-400/30 text-red-300 hover:bg-red-400/10",
          <>
            <X className="size-3" />
            Reject
          </>,
        )}
        {action(
          "changes_requested",
          "border border-content/16 text-content/88 hover:bg-content/6",
          <>
            <PenLine className="size-3" />
            Ask for changes
          </>,
        )}
        <button
          type="button"
          disabled={!approve.enabled}
          onClick={() => onDecide("approved")}
          className={`${BUTTON} bg-content text-background-base hover:bg-content/85 disabled:bg-content/18 disabled:text-content/50`}
        >
          <Check className="size-3" strokeWidth={2.25} />
          {approve.label}
        </button>
      </div>
      {filerRemoved(card) ? <NothingSent /> : null}
      <KeyHints rows={rows} approve={approve} />
    </div>
  );
}
