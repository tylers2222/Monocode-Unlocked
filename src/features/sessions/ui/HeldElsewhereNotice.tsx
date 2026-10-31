import { AppWindow, ArrowUpRight } from "../../../shared/ui/icons";

/**
 * ADE-141 frames A1-A2: in place of the composer, for a chat another window
 * holds. This window shows it read-only and sends nothing.
 */
export function HeldElsewhereNotice({ onGo }: { onGo: () => void }) {
  return (
    <div
      role="status"
      data-held-elsewhere
      className="mx-1.5 mb-1.5 flex items-center gap-3 rounded-lg border border-content/10 bg-content/[0.03] py-3 pr-3 pl-3.5"
    >
      <AppWindow className="size-4 shrink-0 text-content/55" />
      <div className="min-w-0 flex-1">
        <div className="text-[13px] leading-[18px] font-medium text-content">
          Open in another window
        </div>
        <div className="text-[12px] leading-[18px] text-content/50">
          Go there to see it live or send.
        </div>
      </div>
      <button
        type="button"
        onClick={onGo}
        className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md bg-selection-strong px-2.5 text-[12px] font-medium whitespace-nowrap text-content"
      >
        Go to that window
        <ArrowUpRight className="size-3" />
      </button>
    </div>
  );
}
