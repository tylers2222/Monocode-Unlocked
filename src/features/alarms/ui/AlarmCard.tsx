import { useState, type ReactNode } from "react";
import { ApprovalButton } from "../../../shared/ui/ApprovalButton";
import { Bell, Check, ChevronDown, Clock } from "../../../shared/ui/icons";
import { SecondaryButton } from "../../../shared/ui/SecondaryButton";
import {
  actedLine,
  agentLabel,
  alarmTarget,
  countdownText,
  dialogLine,
  roleWord,
  stalledSpan,
  statusWord,
  type AlarmActed,
  type AlarmActEvent,
  type AlarmAgent,
  type AlarmCardView,
  type AlarmDialog,
  type AlarmEscalation,
} from "../model/alarmViews";
import { AlarmCardFrame, AlarmChip, AlarmFacts } from "./alarmParts";

export type AlarmCardActions = {
  /** Every act, with the alarm id it is about (design section 7). */
  onAct?: (event: AlarmActEvent) => void;
  /** The landing's Open button: the lead whose turn it was. */
  onOpenLead?: (lead: AlarmAgent) => void;
};

/** The acts that open a row under the card before they send. */
type OpenAct = "answer" | "steer" | "dismiss";

type CardActBody<E = Extract<AlarmActEvent, { kind: "alarm" }>> =
  E extends unknown ? Omit<E, "kind" | "alarmId"> : never;

/**
 * A delivered `<mono_alarm>` (frame 2), or Tyler's landing above the stalled
 * agent's composer (frame 4). The agent still gets the tag text verbatim.
 */
export function AlarmCard({
  view,
  onAct,
  onOpenLead,
}: AlarmCardActions & { view: AlarmCardView }) {
  const [open, setOpen] = useState<OpenAct | null>(null);
  const escalation =
    view.audience === "provisioner" ? view.escalation : undefined;
  const send = (body: CardActBody) => {
    setOpen(null);
    onAct?.({ ...body, kind: "alarm", alarmId: view.id });
  };
  return (
    <div className="flex flex-col gap-1.5" data-alarm={view.id}>
      <AlarmCardFrame label={`Alarm ${view.id}`}>
        <AlarmHeader view={view} />
        <AlarmSentence view={view} escalation={escalation} />
        <AlarmFacts labelWidth="w-[92px]" rows={alarmFacts(view, escalation)} />
        <AlarmActs
          view={view}
          open={open}
          onToggle={(act) =>
            setOpen((current) => (current === act ? null : act))
          }
          onSend={send}
          onOpenLead={onOpenLead}
        />
        <OpenActRow view={view} open={open} onSend={send} />
      </AlarmCardFrame>
      {view.acted ? (
        <ActedLine acted={view.acted} target={alarmTarget(view)} />
      ) : null}
    </div>
  );
}

function AlarmHeader({ view }: { view: AlarmCardView }) {
  const tyler = view.audience === "tyler";
  const escalated = tyler ? view.level >= 2 : !!view.escalation;
  const where = tyler ? " · to you" : escalated ? "" : " · level 1";
  return (
    <div className="flex items-center gap-1.5 text-[11px] text-content/45">
      <Bell className="size-[13px] shrink-0 text-amber-400" />
      <span>Alarm from the app{where}</span>
      {escalated ? <AlarmChip tone="red">level {view.level}</AlarmChip> : null}
      {/* Someone acted, so it goes nowhere; the acted line says so. */}
      {view.acted ? null : (
        <span className="ml-auto flex items-center gap-1 tabular-nums">
          {tyler ? null : (
            <Clock className="size-[11px] shrink-0" strokeWidth={2} />
          )}
          {countdownText(view)}
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

type Fact = { label: string; value: ReactNode };

/** Who was told; no row at all when nobody was. */
function toldFact(
  label: string,
  sends: AlarmCardView["sends"],
  format: (send: AlarmCardView["sends"][number]) => string,
): Fact[] {
  return sends.length > 0
    ? [{ label, value: sends.map(format).join(" · ") }]
    : [];
}

function alarmFacts(
  view: AlarmCardView,
  escalation: AlarmEscalation | undefined,
): Fact[] {
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
      ...toldFact(
        "Told",
        view.sends,
        (send) => `${send.recipient} at ${send.at} (level ${send.level})`,
      ),
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
    ...(view.audience === "tyler"
      ? toldFact(
          view.acted ? "Told" : "Told, no act",
          view.sends,
          (send) => `${send.recipient} ${send.at}`,
        )
      : [{ label: "Dialog", value: dialogLine(view.dialog) }]),
  ];
}

function AlarmActs({
  view,
  open,
  onToggle,
  onSend,
  onOpenLead,
}: Pick<AlarmCardActions, "onOpenLead"> & {
  view: AlarmCardView;
  open: OpenAct | null;
  onToggle: (act: OpenAct) => void;
  onSend: (body: CardActBody) => void;
}) {
  const target = alarmTarget(view);
  const named = target === view.stalled ? "" : ` ${target.name}`;
  // The landing is the read, so it drops Read and offers the lead instead.
  const lead = view.audience === "tyler" ? view.lead : undefined;
  return (
    <div className="mt-1 flex items-center gap-1.5">
      {view.audience === "tyler" ? null : (
        <SecondaryButton
          onClick={() => onSend({ act: "read", target: view.stalled })}
        >
          Read
        </SecondaryButton>
      )}
      <SecondaryButton
        disabled={!view.dialog}
        aria-expanded={view.dialog ? open === "answer" : undefined}
        onClick={() => onToggle("answer")}
      >
        Answer
        {view.dialog ? (
          <ChevronDown className="size-[11px]" strokeWidth={2} />
        ) : null}
      </SecondaryButton>
      <SecondaryButton
        aria-expanded={open === "steer"}
        onClick={() => onToggle("steer")}
      >
        Steer{named}
      </SecondaryButton>
      <SecondaryButton onClick={() => onSend({ act: "nudge", target })}>
        Nudge{named}
      </SecondaryButton>
      <SecondaryButton
        aria-expanded={open === "dismiss"}
        onClick={() => onToggle("dismiss")}
      >
        Dismiss
      </SecondaryButton>
      {lead ? (
        <span className="ml-auto">
          <SecondaryButton onClick={() => onOpenLead?.(lead)}>
            Open {lead.name}
          </SecondaryButton>
        </span>
      ) : null}
    </div>
  );
}

/** The row an open act shows under the card: the dialog's answers, or a one-line box. */
function OpenActRow({
  view,
  open,
  onSend,
}: {
  view: AlarmCardView;
  open: OpenAct | null;
  onSend: (body: CardActBody) => void;
}) {
  const target = alarmTarget(view);
  if (open === "answer" && view.dialog)
    return (
      <AnswerRow
        agent={view.stalled}
        dialog={view.dialog}
        onAnswer={(answer) =>
          onSend({ act: "answer", target: view.stalled, answer })
        }
      />
    );
  if (open === "steer")
    return (
      <TextActRow
        label={`Steer ${target.name}:`}
        placeholder="One line, sent into its turn"
        onSubmit={(text) => onSend({ act: "steer", target, text })}
      />
    );
  if (open === "dismiss")
    return (
      <TextActRow
        label="Reason to dismiss:"
        placeholder="Why it needs no act"
        onSubmit={(text) => onSend({ act: "dismiss", target, text })}
      />
    );
  return null;
}

function AnswerRow({
  agent,
  dialog,
  onAnswer,
}: {
  agent: AlarmAgent;
  dialog: AlarmDialog;
  onAnswer: (answer: Extract<CardActBody, { act: "answer" }>["answer"]) => void;
}) {
  const label = `Answer for ${agent.name}:`;
  if (dialog.options.length === 0)
    return (
      <TextActRow
        label={label}
        placeholder="Type your answer"
        onSubmit={(text) => onAnswer({ kind: "text", text })}
      />
    );
  return (
    <ActRow label={label}>
      {dialog.options.map((option, index) => (
        <ApprovalButton
          key={option}
          primary={index === 0}
          onClick={() => onAnswer({ kind: "option", option })}
        >
          {option}
        </ApprovalButton>
      ))}
    </ActRow>
  );
}

function ActRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2 pt-1.5 pl-0.5 text-[12px] text-content/55">
      <span className="shrink-0">{label}</span>
      {children}
    </div>
  );
}

function TextActRow({
  label,
  placeholder,
  onSubmit,
}: {
  label: string;
  placeholder: string;
  onSubmit: (text: string) => void;
}) {
  const [text, setText] = useState("");
  const value = text.trim();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (value) onSubmit(value);
      }}
    >
      <ActRow label={label}>
        <input
          value={text}
          aria-label={label.replace(/:$/, "")}
          placeholder={placeholder}
          onChange={(event) => setText(event.target.value)}
          className="min-w-0 flex-1 rounded-md border border-content/15 bg-transparent px-2 py-0.5 text-[12px] text-content outline-none placeholder:text-content/35 focus:border-content/30"
        />
        <ApprovalButton
          primary
          disabled={!value}
          onClick={() => onSubmit(value)}
        >
          Send
        </ApprovalButton>
      </ActRow>
    </form>
  );
}

function ActedLine({
  acted,
  target,
}: {
  acted: AlarmActed;
  target: AlarmAgent;
}) {
  const line = actedLine(acted, target);
  return (
    <div className="flex items-center gap-1.5 text-[11px] text-content/50">
      <AlarmChip tone="gray">
        <Check className="size-2.5" strokeWidth={2.2} />
        {line.chip}
      </AlarmChip>
      <span>{line.text}</span>
    </div>
  );
}
