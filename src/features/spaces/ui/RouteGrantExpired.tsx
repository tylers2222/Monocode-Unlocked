import { CircleAlert, RefreshCw, Undo2 } from "../../../shared/ui/icons";

/**
 * The turn a routing grant ended stopped before it was sent (approved frame
 * 6, ADE-170): back to the first login, or ask for another grant.
 */
export function RouteGrantExpired({
  title,
  firstLoginBack,
  fromLabel,
  onGoBack,
  onAskAgain,
}: {
  title: string;
  firstLoginBack?: string;
  fromLabel: string;
  onGoBack: () => void;
  onAskAgain: () => void;
}) {
  return (
    <div
      className="mx-1.5 mb-2 rounded-lg border border-red-400/25 bg-red-400/6 px-3 py-2.5 text-[12px] text-content/60"
      data-route-grant-expired
      role="alert"
    >
      <div className="flex items-center gap-2">
        <CircleAlert className="size-3.5 shrink-0 text-red-400" />
        <span className="text-content/85">{title}</span>
      </div>
      <div className="pl-5.5">
        <p className="mt-1 leading-relaxed">
          This session stopped before its next turn. Your conversation is saved,
          and your message waits below.
        </p>
        {firstLoginBack ? (
          <p className="mt-1 text-content/45">{firstLoginBack}</p>
        ) : null}
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <button
            type="button"
            onClick={onGoBack}
            className="flex h-6.5 shrink-0 items-center gap-1.5 rounded-md bg-white px-2.5 text-[12px] font-medium text-black hover:bg-white/90"
          >
            <Undo2 className="size-3.5" />
            Go back to {fromLabel}
          </button>
          <button
            type="button"
            onClick={onAskAgain}
            className="flex h-6.5 shrink-0 items-center gap-1.5 rounded-md bg-selection px-2.5 text-[12px] text-content hover:bg-selection-hover"
          >
            <RefreshCw className="size-3.5 text-content/50" />
            Ask again
          </button>
        </div>
      </div>
    </div>
  );
}
