import { Check, Hourglass } from "../../../shared/ui/icons";
import {
  agentLabel,
  holdClock,
  holdFraction,
  type SleepHoldView,
} from "../model/alarmViews";
import {
  AlarmCardFrame,
  AlarmChip,
  AlarmFacts,
  ApprovalButton,
} from "./alarmParts";

/**
 * Frame 3: a held foreground wait under its tool row, waiting for Tyler's OK,
 * then folded to one line once it has a decision.
 */
export function SleepHoldCard({
  view,
  onAllow,
  onDeny,
}: {
  view: SleepHoldView;
  /** `holds.allow`. */
  onAllow?: (holdId: string) => void;
  /** `holds.deny`. */
  onDeny?: (holdId: string) => void;
}) {
  if (view.decision) return <SleepHoldResult view={view} />;
  return (
    <div className="mt-1 mb-0.5 ml-5" data-sleep-hold={view.id}>
      <AlarmCardFrame label={`Sleep hold ${view.id}`}>
        <div className="flex items-center gap-1.5 text-[11px] text-content/45">
          <Hourglass className="size-[13px] shrink-0 text-amber-400" />
          <span>Sleep hold · waiting for your OK</span>
          <span className="ml-auto font-mono tabular-nums text-amber-400">
            {holdClock(view.remainingMs)}
          </span>
        </div>
        <div
          role="progressbar"
          aria-label="Time left to answer"
          aria-valuemin={0}
          aria-valuemax={view.windowMs}
          aria-valuenow={view.remainingMs}
          className="h-0.5 overflow-hidden rounded-xs bg-content/8"
        >
          <div
            className="h-full bg-amber-400"
            style={{ width: `${holdFraction(view) * 100}%` }}
          />
        </div>
        <p className="text-[13px] leading-[19px] text-content/85">
          {agentLabel(view.agent)} wants to wait in the foreground.
        </p>
        <AlarmFacts
          labelWidth="w-[80px]"
          rows={[
            {
              label: "Command",
              value: (
                <span className="font-mono text-[11.5px] text-content">
                  {view.command}
                </span>
              ),
            },
            { label: "How long", value: view.howLong },
            {
              label: "Reason",
              value: view.reason ? `"${view.reason}"` : "(none given)",
            },
          ]}
        />
        <div className="mt-0.5 flex items-center gap-2">
          <ApprovalButton primary onClick={() => onAllow?.(view.id)}>
            Allow
          </ApprovalButton>
          <ApprovalButton primary={false} onClick={() => onDeny?.(view.id)}>
            Deny
          </ApprovalButton>
          <span className="text-[11px] text-content/45">
            No answer in 2 min = refused.
          </span>
        </div>
      </AlarmCardFrame>
    </div>
  );
}

function SleepHoldResult({ view }: { view: SleepHoldView }) {
  const decision = view.decision!;
  const allowed = decision.kind === "allow";
  const chip =
    decision.kind === "allow"
      ? "Allowed"
      : decision.kind === "deny"
        ? "Denied"
        : "Refused";
  const text =
    decision.kind === "timeout"
      ? "no answer in 2 min · the agent was told to run it in the background"
      : [
          `by ${decision.by} at ${decision.at}`,
          decision.via === "notification" ? "from the notification" : null,
          decision.reason ? `"${decision.reason}"` : null,
        ]
          .filter(Boolean)
          .join(" · ");
  return (
    <div
      className="flex items-center gap-1.5 py-0.5 pl-5 text-[11px] text-content/50"
      data-sleep-hold={view.id}
    >
      <AlarmChip tone={allowed ? "gray" : "red"}>
        {allowed ? <Check className="size-2.5" strokeWidth={2.2} /> : null}
        {chip}
      </AlarmChip>
      <span>{text}</span>
    </div>
  );
}
