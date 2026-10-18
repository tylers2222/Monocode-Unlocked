import { Check, Terminal } from "../../../shared/ui/icons";
import { PixelMascot } from "../../projects/ui/PixelMascot";
import {
  STATUS_LABELS,
  agentCreator,
  formatDay,
  formatTarget,
  timelineGroups,
  type AgentCreator,
  type Milestone,
  type MilestoneStatus,
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
};

const GROUP_DOT: Record<MilestoneStatus, string> = {
  done: "grid place-items-center border border-emerald-400/60 bg-emerald-400/18 text-emerald-400",
  in_progress: "bg-accent ring-3 ring-accent/25",
  planned: "border-[1.5px] border-content/70 bg-background-base",
  idea: "border-[1.5px] border-dashed border-content/45 bg-background-base",
};

const ROW_DOT: Record<MilestoneStatus, string> = {
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
}: Props) {
  const groups = timelineGroups(milestones, hiddenStatuses);
  const todayLine = (
    <div
      data-today
      className="grid h-[26px] mt-2.5 grid-cols-[14px_1fr] items-center gap-3.5"
    >
      <span className="h-0.5 w-3.5 rounded-sm bg-accent" />
      <span className="flex items-center gap-2.5 text-[11px] font-medium text-accent after:flex-1 after:border-t after:border-dashed after:border-accent/35">
        Today · {formatDay(today, today)}
      </span>
    </div>
  );
  return (
    <div className="relative mx-auto w-full max-w-[780px] px-8 pt-1.5 pb-7 before:absolute before:top-[22px] before:bottom-[30px] before:left-[39px] before:w-px before:bg-content/16">
      {groups[0]?.status === "done" ? null : todayLine}
      {groups.map(({ status, milestones }) => (
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
            </span>
          </h3>
          <ol>
            {milestones.map((milestone) => (
              <MilestoneRow
                key={milestone.id}
                milestone={milestone}
                highlighted={highlightIds.has(milestone.id)}
                lookUpCreator={lookUpCreator}
                today={today}
              />
            ))}
          </ol>
          {status === "done" ? todayLine : null}
        </section>
      ))}
    </div>
  );
}

function MilestoneRow({
  milestone,
  highlighted,
  lookUpCreator,
  today,
}: {
  milestone: Milestone;
  highlighted: boolean;
  lookUpCreator: NonNullable<Props["lookUpCreator"]>;
  today: Date;
}) {
  const done = milestone.status === "done";
  const creator = agentCreator(milestone.createdBy);
  return (
    <li
      tabIndex={0}
      data-milestone={milestone.id}
      className={`relative grid grid-cols-[14px_1fr_auto] items-start gap-3.5 rounded-lg py-2 pr-2.5 outline-none focus-visible:bg-content/4 ${
        highlighted ? "bg-accent/8 inset-ring inset-ring-accent/30" : ""
      }`}
    >
      <span className="grid h-5 place-items-center">
        <i className={`block size-[7px] rounded-full ${ROW_DOT[milestone.status]}`} />
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
        {milestone.target === null ? (
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
        highlighted ? "bg-accent/14 text-accent" : "bg-content/6 text-content/50"
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
