import {
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import {
  Check,
  ChevronDown,
  GripVertical,
  Terminal,
} from "../../../shared/ui/icons";
import { PixelMascot } from "../../projects/ui/PixelMascot";
import { dayOf, formatDay } from "../model/day";
import type { Milestone, MilestoneStatus } from "../model/milestones";
import { dropBeside, orderKey, stepOf, type Drop } from "../model/reorder";
import {
  STATUS_LABELS,
  agentCreator,
  formatTarget,
  timelineGroups,
  type AgentCreator,
} from "./timeline";

/** How the page names an agent creator; a Mono also brings its mascot. */
export type CreatorLook = {
  name: string;
  mascot?: { name: string; color: string };
};

type Props = {
  milestones: readonly Milestone[];
  hiddenStatuses?: readonly MilestoneStatus[];
  /** Rows an agent just added: blue highlight and a "New" label. */
  highlightIds?: ReadonlySet<string>;
  lookUpCreator?: (creator: AgentCreator) => CreatorLook | undefined;
  today?: Date;
  /** The open editor: in place of row `id`, or above the groups when null. */
  open?: { id: string | null; editor: ReactNode };
  /** Opens the editor on a row, by click or by Enter or Space. */
  onOpen?: (id: string) => void;
  /**
   * A row dragged by its grip or moved with Alt+ArrowUp/Down; "changed" when
   * the list changed under a drag, so nothing is moved.
   */
  onMove?: (move: MoveEnd) => void;
};

export type MoveEnd = { kind: "drop"; drop: Drop } | { kind: "changed" };

/** The row the pointer is over, the half it is on, and where the drop line sits in the timeline. */
type Over = { id: string; side: "before" | "after"; lineY: number };

/** The one gesture under way: a row dragged by its grip, or a row moved by keys that keeps focus. */
type Gesture =
  | {
      kind: "drag";
      id: string;
      from: string;
      over: Over | null;
    }
  | { kind: "keyed"; id: string }
  | null;

/** The row nearest the pointer, and which half of it the pointer is on. */
function overAt(event: ReactPointerEvent, id: string): Over | null {
  const timeline = event.currentTarget.closest("[data-timeline]")!;
  const origin = timeline.getBoundingClientRect().top;
  const rows = timeline.querySelectorAll<HTMLElement>("[data-milestone]");
  let best: { over: Over; distance: number } | null = null;
  for (const row of rows) {
    if (row.dataset.milestone === id) continue;
    const { top, bottom, height } = row.getBoundingClientRect();
    const middle = top + height / 2;
    const distance = Math.abs(event.clientY - middle);
    const before = event.clientY < middle;
    if (!best || distance < best.distance)
      best = {
        over: {
          id: row.dataset.milestone!,
          side: before ? "before" : "after",
          lineY: (before ? top : bottom) - origin,
        },
        distance,
      };
  }
  return best?.over ?? null;
}

const GROUP_DOT: Record<MilestoneStatus, string> = {
  done: "grid place-items-center border border-emerald-400/60 bg-emerald-400/18 text-emerald-400",
  in_progress: "bg-accent ring-3 ring-accent/25",
  planned: "border-[1.5px] border-content/70 bg-background-base",
  idea: "border-[1.5px] border-dashed border-content/45 bg-background-base",
};

export const ROW_DOT: Record<MilestoneStatus, string> = {
  done: "bg-emerald-400 opacity-70",
  in_progress: "bg-accent",
  planned: "border-[1.5px] border-content/60 bg-background-base",
  idea: "border border-dashed border-content/45 bg-background-base",
};

const NO_HIGHLIGHTS: ReadonlySet<string> = new Set();

export function MilestoneTimeline({
  milestones,
  hiddenStatuses = [],
  highlightIds = NO_HIGHLIGHTS,
  lookUpCreator = () => undefined,
  today = new Date(),
  open,
  onOpen,
  onMove,
}: Props) {
  const [doneCollapsed, setDoneCollapsed] = useState(false);
  const [gesture, setGesture] = useState<Gesture>(null);
  const groups = timelineGroups(milestones, hiddenStatuses);
  const shown = groups.filter(
    (group) => !(group.status === "done" && doneCollapsed),
  );
  const drag = gesture?.kind === "drag" ? gesture : null;
  const grip = {
    down: (id: string, event: ReactPointerEvent) => {
      if (event.button !== 0) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      setGesture({
        kind: "drag",
        id,
        from: orderKey(milestones),
        over: null,
      });
    },
    move: (event: ReactPointerEvent) => {
      if (!drag) return;
      setGesture({
        ...drag,
        over: overAt(event, drag.id),
      });
    },
    up: () => {
      setGesture(null);
      if (!drag?.over) return;
      if (orderKey(milestones) !== drag.from)
        return onMove?.({ kind: "changed" });
      const drop = dropBeside(shown, drag.id, drag.over);
      if (drop) onMove?.({ kind: "drop", drop });
    },
    cancel: () => setGesture(null),
  };
  const step = (id: string, delta: -1 | 1) => {
    const drop = stepOf(shown, id, delta);
    if (!drop) return;
    setGesture({ kind: "keyed", id });
    onMove?.({ kind: "drop", drop });
  };
  const todayLine = (
    <div
      data-today
      className="grid h-[26px] mt-2.5 grid-cols-[14px_1fr] items-center gap-3.5"
    >
      <span className="h-0.5 w-3.5 rounded-sm bg-accent" />
      <span className="flex items-center gap-2.5 text-[11px] font-medium text-(--link-color) after:flex-1 after:border-t after:border-dashed after:border-accent/35 dark:text-accent">
        Today · {formatDay(dayOf(today), false)}
      </span>
    </div>
  );
  return (
    <div
      data-timeline
      className="relative mx-auto w-full max-w-[780px] px-8 pt-1.5 pb-7 before:absolute before:top-[22px] before:bottom-[30px] before:left-[39px] before:w-px before:bg-content/16"
    >
      {open?.id === null ? (
        <div className="relative mt-4 ml-7">{open.editor}</div>
      ) : null}
      {groups[0]?.status === "done" || hiddenStatuses.includes("done")
        ? null
        : todayLine}
      {groups.map(({ status, milestones }) => {
        const collapsed = status === "done" && doneCollapsed;
        return (
          <section key={status} aria-label={STATUS_LABELS[status]}>
            <h3 className="relative mt-4 grid h-[30px] grid-cols-[14px_1fr] items-center gap-3.5">
              <span className={`size-3 rounded-full ${GROUP_DOT[status]}`}>
                {status === "done" ? <Check size={8} strokeWidth={3} /> : null}
              </span>
              <span className="flex items-center gap-1.5 text-xs font-semibold text-content/75">
                {STATUS_LABELS[status]}
                <span className="font-normal text-content/35">
                  {milestones.length}
                </span>
                {status === "done" ? (
                  <button
                    type="button"
                    aria-expanded={!collapsed}
                    aria-label={collapsed ? "Show Done" : "Hide Done"}
                    onClick={() => setDoneCollapsed(!collapsed)}
                    className="grid size-4 place-items-center rounded text-content/40 outline-none hover:bg-content/10 hover:text-content focus-visible:ring-1 focus-visible:ring-accent/50"
                  >
                    <ChevronDown
                      size={11}
                      className={collapsed ? "-rotate-90" : undefined}
                    />
                  </button>
                ) : null}
              </span>
            </h3>
            {collapsed ? null : (
              <ol>
                {milestones.map((milestone) =>
                  open?.id === milestone.id ? (
                    <li key={milestone.id} className="ml-7 py-1">
                      {open.editor}
                    </li>
                  ) : (
                    <MilestoneRow
                      key={milestone.id}
                      milestone={milestone}
                      highlighted={highlightIds.has(milestone.id)}
                      lookUpCreator={lookUpCreator}
                      today={today}
                      onOpen={onOpen}
                      grip={grip}
                      onStep={step}
                      lifted={drag?.id === milestone.id}
                      dropLine={
                        drag?.over?.id === milestone.id ? drag.over.side : null
                      }
                      refocus={
                        gesture?.kind === "keyed" && gesture.id === milestone.id
                      }
                    />
                  ),
                )}
              </ol>
            )}
            {status === "done" ? todayLine : null}
          </section>
        );
      })}
      {drag?.over ? (
        <LiftedCard
          milestone={milestones.find((row) => row.id === drag.id)!}
          y={drag.over.lineY}
          today={today}
        />
      ) : null}
    </div>
  );
}

/** Frame M·6 mid-drag: the dragged row's card, lifted, just under the drop line. */
function LiftedCard({
  milestone,
  y,
  today,
}: {
  milestone: Milestone;
  y: number;
  today: Date;
}) {
  return (
    <div
      aria-hidden
      data-lifted
      style={{ top: y + 4 }}
      className="pointer-events-none absolute right-6 left-[66px] z-20 flex items-center gap-3 rounded-lg bg-background-base py-2 pr-2.5 pl-3 text-sm font-medium shadow-lg ring-1 ring-content/12"
    >
      <i
        className={`block size-[7px] shrink-0 rounded-full ${ROW_DOT[milestone.status]}`}
      />
      <span className="min-w-0 flex-1 truncate">{milestone.title}</span>
      <span className="text-xs font-normal text-content/55 tabular-nums">
        {milestone.target ? formatTarget(milestone.target, today) : "Later"}
      </span>
    </div>
  );
}

function MilestoneRow({
  milestone,
  highlighted,
  lookUpCreator,
  today,
  onOpen,
  grip,
  onStep,
  lifted,
  dropLine,
  refocus,
}: {
  milestone: Milestone;
  highlighted: boolean;
  lookUpCreator: NonNullable<Props["lookUpCreator"]>;
  today: Date;
  onOpen: Props["onOpen"];
  grip: {
    down: (id: string, event: ReactPointerEvent) => void;
    move: (event: ReactPointerEvent) => void;
    up: () => void;
    cancel: () => void;
  };
  onStep: (id: string, delta: -1 | 1) => void;
  /** Dragged: the row stays as a faded ghost while its lifted card moves. */
  lifted: boolean;
  dropLine: Over["side"] | null;
  /** Focus moves with the row a key moved, though the move may remount it. */
  refocus: boolean;
}) {
  const done = milestone.status === "done";
  const creator = agentCreator(milestone.createdBy);
  return (
    <li
      tabIndex={0}
      data-milestone={milestone.id}
      ref={
        refocus
          ? (row) => {
              if (row && document.activeElement === document.body) row.focus();
            }
          : undefined
      }
      onClick={() => onOpen?.(milestone.id)}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget) return;
        if (
          event.altKey &&
          (event.key === "ArrowUp" || event.key === "ArrowDown")
        ) {
          event.preventDefault();
          onStep(milestone.id, event.key === "ArrowUp" ? -1 : 1);
          return;
        }
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        onOpen?.(milestone.id);
      }}
      className={`group/row relative grid grid-cols-[14px_1fr_auto] items-start gap-3.5 rounded-lg py-2 pr-2.5 outline-none focus-visible:ring-1 focus-visible:ring-accent/50 focus-visible:ring-inset ${
        highlighted
          ? "bg-accent/8 inset-ring inset-ring-accent/30 motion-safe:animate-[approval-toast-in_220ms_cubic-bezier(0.22,1,0.36,1)]"
          : ""
      } ${lifted ? "opacity-35" : ""}`}
    >
      <span
        data-grip
        aria-hidden
        onPointerDown={(event) => grip.down(milestone.id, event)}
        onPointerMove={grip.move}
        onPointerUp={grip.up}
        onPointerCancel={grip.cancel}
        onClick={(event) => event.stopPropagation()}
        className="absolute top-2 -left-6 grid h-5 w-4 cursor-grab touch-none place-items-center text-content/35 opacity-0 group-hover/row:opacity-100"
      >
        <GripVertical className="size-3.5" />
      </span>
      {dropLine ? (
        <span
          data-drop-line={dropLine}
          className={`pointer-events-none absolute right-0 -left-[22px] z-30 h-0.5 rounded-full bg-accent before:absolute before:-top-[3px] before:left-0 before:size-2 before:rounded-full before:border-2 before:border-accent before:bg-background-base ${
            dropLine === "before" ? "-top-px" : "-bottom-px"
          }`}
        />
      ) : null}
      <span className="grid h-5 place-items-center">
        <i
          className={`block size-[7px] rounded-full ${ROW_DOT[milestone.status]}`}
        />
      </span>
      <div className="min-w-0">
        <div
          className={`flex min-w-0 items-center gap-2 text-sm leading-5 font-medium ${
            done ? "text-content/50" : ""
          }`}
        >
          <span className="min-w-0 truncate">{milestone.title}</span>
          {creator ? (
            <AddedBy
              creator={creator}
              look={lookUpCreator(creator)}
              highlighted={highlighted}
            />
          ) : null}
          {highlighted ? (
            <span className="rounded bg-accent px-[5px] text-[10px] leading-4 font-semibold text-white">
              New
            </span>
          ) : null}
        </div>
        {milestone.note ? (
          <div className="mt-px truncate text-xs leading-[18px] text-content/45">
            {milestone.note}
          </div>
        ) : null}
      </div>
      <span
        className={`min-w-16 text-right text-xs leading-5 whitespace-nowrap tabular-nums ${
          done ? "text-content/35" : "text-content/55"
        }`}
      >
        {!milestone.target ? (
          <span className="rounded border border-dashed border-content/20 px-1.5 py-px text-[11px] text-content/45">
            Later
          </span>
        ) : (
          formatTarget(milestone.target, today)
        )}
      </span>
    </li>
  );
}

function AddedBy({
  creator,
  look,
  highlighted,
}: {
  creator: AgentCreator;
  look: CreatorLook | undefined;
  highlighted: boolean;
}) {
  const label =
    creator.kind === "mono"
      ? `added by ${look?.name ?? "a Mono"}`
      : `added by session${look ? ` · ${look.name}` : ""}`;
  return (
    <span
      className={`inline-flex shrink-0 items-center gap-1 rounded-full py-px pr-2 pl-[5px] text-[11px] font-normal whitespace-nowrap ${
        highlighted
          ? "bg-accent/12 text-(--link-color) dark:bg-accent/14 dark:text-accent"
          : "bg-content/6 text-content/50"
      }`}
    >
      {look?.mascot ? (
        <PixelMascot
          name={look.mascot.name}
          color={look.mascot.color}
          still
          className="size-[11px] shrink-0"
        />
      ) : creator.kind === "session" ? (
        <Terminal size={11} />
      ) : null}
      {label}
    </span>
  );
}
