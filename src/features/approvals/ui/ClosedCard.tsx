import {
  Check,
  ChevronRight,
  Clock,
  PenLine,
  Trash2,
  Undo2,
  UserRemove,
  X,
  type IconComponent,
} from "../../../shared/ui/icons";
import type { ProjectLook } from "../../projects/ui/ProjectList";
import { quoteText } from "../../sessions/ui/composerIntent";
import {
  ago,
  closedAs,
  closedAt,
  dayOrTime,
  delivery,
  deliveryTime,
  filerRemoved,
  stamp,
  type Closed,
  type ClosedApproval,
} from "../model/approvals";
import { CardHead, PILL } from "./ApprovalCard";
import { GigaText } from "./GigaText";
import { OpenGiga } from "./OpenGiga";

const CLOSED: Record<
  Closed,
  {
    label: string;
    icon: IconComponent;
    tone: string;
    pill: string;
    box: string;
  }
> = {
  approved: {
    label: "Approved",
    icon: Check,
    tone: "text-emerald-400",
    pill: "bg-emerald-400/12",
    box: "bg-emerald-400/7 shadow-[inset_2px_0_0_var(--color-emerald-400)]",
  },
  rejected: {
    label: "Rejected",
    icon: X,
    tone: "text-red-300",
    pill: "bg-red-400/13",
    box: "bg-red-400/7 shadow-[inset_2px_0_0_var(--color-red-500)]",
  },
  changes_requested: {
    label: "Changes requested",
    icon: PenLine,
    tone: "text-amber-400",
    pill: "bg-amber-400/12",
    box: "bg-amber-400/7 shadow-[inset_2px_0_0_var(--color-amber-400)]",
  },
  withdrawn: {
    label: "Withdrawn",
    icon: Undo2,
    tone: "text-content/72",
    pill: "bg-content/7",
    box: "bg-content/4 shadow-[inset_2px_0_0_color-mix(in_srgb,var(--color-content)_40%,transparent)]",
  },
};

export function ClosedPill({ closed }: { closed: Closed }) {
  const { label, icon: Icon, tone, pill } = CLOSED[closed];
  return (
    <span className={`${PILL} ${pill} ${tone}`}>
      <Icon className="size-[11px]" strokeWidth={2.25} />
      {label}
    </span>
  );
}

function Decision({ card }: { card: ClosedApproval }) {
  const { status } = card;
  const { label, icon: Icon, tone, box } = CLOSED[closedAs(status)];
  const chosen = status.state === "decided" ? status.chosen : [];
  return (
    <div className={`mt-3.5 flex gap-2.5 rounded-lg px-3 py-2.5 ${box}`}>
      <Icon className={`mt-0.5 size-3.5 shrink-0 ${tone}`} />
      <div className="min-w-0">
        <div className="text-xs font-semibold">
          {label}{" "}
          <span className="font-normal text-content/62">
            by{" "}
            {status.state === "decided" ? (
              "you"
            ) : (
              <GigaText text={card.requestedBy.name} />
            )}{" "}
            · {stamp(closedAt(status))}
          </span>
        </div>
        {status.state === "decided" && card.options.length > 0 ? (
          <div className="mt-1.5 flex flex-col gap-[3px]">
            {card.options.map(({ label: option }) =>
              chosen.includes(option) ? (
                <div
                  key={option}
                  role="checkbox"
                  aria-checked
                  aria-disabled
                  className="flex items-center gap-[7px] text-[12.5px] text-content/85"
                >
                  <span className="grid size-3.5 shrink-0 place-items-center rounded-[3px] bg-accent text-white">
                    <Check className="size-2.5" strokeWidth={3} />
                  </span>
                  <GigaText text={option} />
                </div>
              ) : (
                <div
                  key={option}
                  role="checkbox"
                  aria-checked={false}
                  aria-disabled
                  className="flex items-center gap-[7px] text-[12.5px] text-content/55"
                >
                  <span className="size-3.5 shrink-0 rounded-[3px] border-[1.5px] border-content/35" />
                  <GigaText text={option} />
                </div>
              ),
            )}
          </div>
        ) : null}
        {status.state === "decided" && status.note ? (
          <div className="mt-[3px] text-[13px] leading-[19px] text-content/85">
            “<GigaText text={status.note} />”
          </div>
        ) : null}
      </div>
    </div>
  );
}

function DeliveryLine({
  card,
  now,
  onOpenMono,
}: {
  card: ClosedApproval;
  now: number;
  onOpenMono: (monoId: string) => void;
}) {
  const { status } = card;
  const sent = delivery(card);
  if (sent === "sent" && status.state === "decided")
    return (
      <div className="mt-2.5 flex items-center gap-1.5 text-[11.5px] text-content/62">
        <Check className="size-3 text-emerald-400" />
        <span>
          Sent to <GigaText text={card.requestedBy.name} /> as a queued
          follow-up · {deliveryTime(status.decidedAt, now)}
        </span>
      </div>
    );
  if (sent === "withdrawn")
    return (
      <div className="mt-2.5 text-[11.5px] text-content/62">
        Nothing to send: the filing Giga withdrew it. You cannot decide a
        withdrawn card.
      </div>
    );
  if (sent === "waiting")
    return (
      <div className="mt-2.5 flex items-center gap-2 text-[11.5px] text-content/72">
        <Clock className="size-3 shrink-0" />
        <span className="min-w-0 flex-1">
          Waiting to send: <GigaText text={card.requestedBy.name} /> has no chat
          to receive it yet.
        </span>
        <OpenGiga
          card={card}
          onOpenMono={onOpenMono}
          className="h-6 px-[9px] text-[11.5px]"
        />
      </div>
    );
  if (sent === "unqueued")
    return (
      <div className="mt-2.5 flex items-center gap-2 text-[11.5px] text-content/72">
        <Trash2 className="size-3 shrink-0" />
        <span>
          Not sent: you removed it from{" "}
          <GigaText text={card.requestedBy.name} />
          ’s queue.
        </span>
      </div>
    );
  return (
    <div className="mt-2.5 flex items-center gap-1.5 text-[11.5px] text-amber-400">
      <UserRemove className="size-3" />
      Not sent: the Giga that filed this was removed.
    </div>
  );
}

export function ClosedCard({
  card,
  look,
  now,
  onOpenMono,
}: {
  card: ClosedApproval;
  look: ProjectLook;
  now: number;
  onOpenMono: (monoId: string) => void;
}) {
  return (
    <article
      aria-label={quoteText(card.heading)}
      className="overflow-hidden rounded-[10px] border border-content/11 bg-content/2 px-4 pt-3.5 pb-4"
    >
      <CardHead
        card={card}
        look={look}
        statusPill={<ClosedPill closed={closedAs(card.status)} />}
        time={
          card.status.state === "decided"
            ? `decided ${dayOrTime(card.status.decidedAt, now)}`
            : dayOrTime(closedAt(card.status), now)
        }
      />
      <GigaText
        text={card.summary}
        className="mt-3 block truncate text-[13px] leading-5 text-content/82"
      />
      <Decision card={card} />
      <DeliveryLine card={card} now={now} onOpenMono={onOpenMono} />
    </article>
  );
}

export function ClosedRow({
  card,
  now,
  onOpen,
}: {
  card: ClosedApproval;
  now: number;
  onOpen: () => void;
}) {
  const sent = delivery(card);
  return (
    <button
      type="button"
      onClick={onOpen}
      className="grid min-h-[42px] w-full grid-cols-[auto_1fr_auto_auto] items-center gap-3 rounded-lg border border-content/8 bg-content/2 px-3.5 text-left hover:bg-content/4"
    >
      <ClosedPill closed={closedAs(card.status)} />
      <GigaText
        text={card.heading}
        className="truncate text-[13px] font-medium text-content/88"
      />
      <span className="text-xs whitespace-nowrap text-content/60">
        {filerRemoved(card) ? (
          <del className="decoration-content/40">
            <GigaText text={card.requestedBy.name} />
          </del>
        ) : (
          <GigaText text={card.requestedBy.name} />
        )}
        {sent === "filer_removed" ? (
          <span className="text-amber-400"> not sent</span>
        ) : null}{" "}
        ·{" "}
        {sent === "waiting" || sent === "unqueued" ? (
          <>
            <span className="inline-flex items-center gap-1 align-[-2px] font-medium text-content/82">
              {sent === "waiting" ? (
                <Clock className="size-3" />
              ) : (
                <Trash2 className="size-3" />
              )}
              {sent === "waiting" ? "waiting to send" : "not sent"}
            </span>{" "}
            ·{" "}
          </>
        ) : null}
        {ago(closedAt(card.status), now)}
      </span>
      <ChevronRight className="size-3.5 text-content/55" />
    </button>
  );
}
