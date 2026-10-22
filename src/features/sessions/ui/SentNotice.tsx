import type { ReactNode } from "react";
import { ChevronRight } from "../../../shared/ui/icons";

/**
 * Design U10: a sent notice is the app talking, not you. It takes the agent's
 * column, and "What the agent got" shows the exact text it read.
 */
export function SentNotice({
  from,
  icon,
  sentText,
  agentName,
  embedded,
  children,
}: {
  /** Who in the app sent it, before " · sent to". */
  from: string;
  icon: ReactNode;
  sentText: string;
  agentName?: string;
  embedded: boolean;
  /** The one sentence that says what was sent. */
  children: ReactNode;
}) {
  return (
    <div
      data-mono-notice
      className={`flex min-w-0 gap-2 pt-1.5 pb-5 font-sans ${embedded ? "" : "px-4"}`}
    >
      {icon}
      <div className="min-w-0 flex-1">
        <div className="text-[11px] text-content/70">
          {from} · {agentName ? `sent to ${agentName}` : "sent"}
        </div>
        <p className="mt-0.5 text-[13px] text-content/80">{children}</p>
        <details className="group/notice mt-1 min-w-0">
          <summary className="flex w-fit cursor-pointer list-none items-center gap-1.5 rounded text-[11px] text-content/55 transition-colors hover:text-content/80 focus-visible:outline focus-visible:outline-1 focus-visible:outline-content/40 [&::-webkit-details-marker]:hidden">
            <ChevronRight className="size-3 shrink-0 transition-transform group-open/notice:rotate-90" />
            <span>What the agent got</span>
          </summary>
          <pre className="mt-2 max-h-72 min-w-0 overflow-auto overscroll-contain rounded-md border border-content/10 bg-content/5 p-2.5 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-words text-content/70">
            {sentText}
          </pre>
        </details>
      </div>
    </div>
  );
}
