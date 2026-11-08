import { useState, type ReactNode } from "react";
import { Bell, Check, ChevronDown, Clock } from "../../../shared/ui/icons";
import { SecondaryButton } from "../../../shared/ui/SecondaryButton";
import {
  actedLine,
  agentLabel,
  countdownText,
  dialogLine,
  roleWord,
  stalledSpan,
  statusWord,
  type AlarmActKind,
  type AlarmAgent,
  type AlarmCardView,
  type AlarmEscalation,
} from "../model/alarmViews";
import {
  AlarmCardFrame,
  AlarmChip,
  AlarmFacts,
  ApprovalButton,
} from "./alarmParts";

export type AlarmCardActions = {
  /** `alarms.<act>`; `target` is who it lands on (the lead at level 2). */
  onAct?: (act: Exclude<AlarmActKind, "answer">, target: AlarmAgent) => void;
  /** One of the dialog's own options (`alarms.answer`). */
  onAnswer?: (option: string) => void;
  /** The landing's Open button: the lead whose turn it was. */
  onOpenLead?: (lead: AlarmAgent) => void;
};

/**
 * A delivered `<mono_alarm>` (frame 2), or Tyler's landing above the stalled
 * agent's composer (frame 4). The agent still gets the tag text verbatim.
 */
export function AlarmCard({
  view,
  onAct,
  onAnswer,
  onOpenLead,
}: AlarmCardActions & { view: AlarmCardView }) {
  const [answering, setAnswering] = useState(false);
  // Tyler sits above every level, so his acts land on the stalled agent.
  const escalation = view.audience === "tyler" ? undefined : view.escalation;
  return (
    <div className="flex flex-col gap-1.5" data-alarm={view.id}>
      <AlarmCardFrame label={`Alarm ${view.id}`}>
        <AlarmHeader view={view} />
        <AlarmSentence view={view} escalation={escalation} />
        <AlarmFacts labelWidth="w-[92px]" rows={alarmFacts(view, escalation)} />
        <AlarmActs
          view={view}
          escalation={escalation}
          answering={answering}
          onToggleAnswer={() => setAnswering((open) => !open)}
          onAct={onAct}
          onOpenLead={onOpenLead}
        />
        {answering && view.dialog ? (
          <div className="flex items-center gap-2 pt-1.5 pl-0.5 text-[12px] text-content/55">
            <span>Answer for {view.stalled.name}:</span>
            {view.dialog.options.map((option, index) => (
              <ApprovalButton
                key={option}
                primary={index === 0}
                onClick={() => onAnswer?.(option)}
              >
                {option}
              </ApprovalButton>
            ))}
          </div>
        ) : null}
      </AlarmCardFrame>
      {view.acted ? <ActedLine view={view} /> : null}
    </div>
  );
}

function AlarmHeader({ view }: { view: AlarmCardView }) {
  const where =
    view.audience === "tyler"
      ? " · to you"
      : view.level < 2
        ? ` · level ${view.level}`
        : "";
  return (
    <div className="flex items-center gap-1.5 text-[11px] text-content/45">
      <Bell className="size-[13px] shrink-0 text-amber-400" />
      <span>Alarm from the app{where}</span>
      {view.level >= 2 ? (
        <AlarmChip tone="red">level {view.level}</AlarmChip>
      ) : null}
      {/* Someone acted, so it goes nowhere; the acted line says so. */}
      {view.acted ? null : (
        <span className="ml-auto flex items-center gap-1 tabular-nums">
          {view.next.kind === "last" ? null : (
            <Clock className="size-[11px] shrink-0" strokeWidth={2} />
          )}
          {countdownText(view.next)}
        </span>
      )}
    </div>
  );
}

function AlarmSentence({
  view,
  escalation,
}: {
  view: AlarmCardView;
  escalation?: AlarmEscalation;
}) {
  return (
    <p className="text-[13px] leading-[19px] text-content/85">
      {escalation ? (
        <>
          <span className="text-red-300">Not acted on</span> ·{" "}
          {roleWord(escalation.target.role)} {agentLabel(escalation.target)} did
          not act on {escalation.missed} for {escalation.minutes} min.
        </>
      ) : (
        <>
          <span className="text-amber-400">{statusWord(view.cause)}</span> ·{" "}
          {agentLabel(view.stalled)} is stalled {stalledSpan(view)}.
        </>
      )}
    </p>
  );
}

function alarmFacts(
  view: AlarmCardView,
  escalation: AlarmEscalation | undefined,
): { label: string; value: ReactNode }[] {
  const openWork = {
    label: "Open work",
    value: view.openWork.join("; ") || "none",
  };
  if (escalation)
    return [
      {
        label: roleWord(view.stalled.role),
        value: `${agentLabel(view.stalled)}, stalled ${stalledSpan(view)}`,
      },
      openWork,
      {
        label: "Told",
        value: view.sends
          .map(
            (send) => `${send.recipient} at ${send.at} (level ${send.level})`,
          )
          .join(" · "),
      },
    ];
  const activity = view.lastActivity;
  return [
    openWork,
    {
      label: "Last activity",
      value: activity ? (
        <>
          {activity.at} {activity.tool}{" "}
          <span className="font-mono text-[11.5px]">{activity.command}</span> (
          {activity.state})
        </>
      ) : (
        "none"
      ),
    },
    view.audience === "tyler"
      ? {
          label: "Told, no act",
          value: view.sends
            .map((send) => `${send.recipient} ${send.at}`)
            .join(" · "),
        }
      : { label: "Dialog", value: dialogLine(view.dialog) },
  ];
}

function AlarmActs({
  view,
  escalation,
  answering,
  onToggleAnswer,
  onAct,
  onOpenLead,
}: Pick<AlarmCardActions, "onAct" | "onOpenLead"> & {
  view: AlarmCardView;
  escalation?: AlarmEscalation;
  answering: boolean;
  onToggleAnswer: () => void;
}) {
  const target = escalation?.target ?? view.stalled;
  const named = escalation ? ` ${escalation.target.name}` : "";
  // The landing is the read, so it drops Read and offers the lead instead.
  const landing = view.audience === "tyler";
  return (
    <div className="mt-1 flex items-center gap-1.5">
      {landing ? null : (
        <SecondaryButton onClick={() => onAct?.("read", view.stalled)}>
          Read
        </SecondaryButton>
      )}
      <SecondaryButton
        disabled={!view.dialog}
        aria-expanded={view.dialog ? answering : undefined}
        onClick={onToggleAnswer}
      >
        Answer
        {view.dialog ? (
          <ChevronDown className="size-[11px]" strokeWidth={2} />
        ) : null}
      </SecondaryButton>
      <SecondaryButton onClick={() => onAct?.("steer", target)}>
        Steer{named}
      </SecondaryButton>
      <SecondaryButton onClick={() => onAct?.("nudge", target)}>
        Nudge{named}
      </SecondaryButton>
      <SecondaryButton onClick={() => onAct?.("dismiss", target)}>
        Dismiss
      </SecondaryButton>
      {landing && view.lead ? (
        <span className="ml-auto">
          <SecondaryButton onClick={() => onOpenLead?.(view.lead!)}>
            Open {view.lead.name}
          </SecondaryButton>
        </span>
      ) : null}
    </div>
  );
}

function ActedLine({ view }: { view: AlarmCardView }) {
  const acted = actedLine(view.acted!, view.stalled);
  return (
    <div className="flex items-center gap-1.5 text-[11px] text-content/50">
      <AlarmChip tone="gray">
        <Check className="size-2.5" strokeWidth={2.2} />
        {acted.chip}
      </AlarmChip>
      <span>{acted.text}</span>
    </div>
  );
}
