import { Check } from "../../../shared/ui/icons";
import { Popover, type PopoverDismissReason } from "../../../shared/ui/Popover";
import type {
  Milestone,
  MilestoneSettings as Settings,
  MilestoneStatus,
} from "../model/milestones";
import { GROUP_DOT } from "./MilestoneTimeline";
import { STATUS_LABELS } from "./timeline";

/** Frame M·8 lists the statuses in this order. */
const STATUSES: readonly MilestoneStatus[] = [
  "in_progress",
  "planned",
  "idea",
  "done",
];

/** The hidden statuses after `status` is shown or hidden. */
export function hiddenAfter(
  hidden: readonly MilestoneStatus[],
  status: MilestoneStatus,
  shown: boolean,
): MilestoneStatus[] {
  const rest = hidden.filter((other) => other !== status);
  return shown ? rest : [...rest, status];
}

/** Frame M·8: one project's milestones on or off, and which statuses the timeline shows. */
export function MilestoneSettings({
  anchor,
  name,
  settings,
  milestones,
  onChange,
  onDismiss,
}: {
  anchor: HTMLElement;
  name: string;
  settings: Settings;
  milestones: readonly Milestone[];
  onChange: (patch: Partial<Settings>) => void;
  onDismiss: (reason: PopoverDismissReason) => void;
}) {
  return (
    <Popover
      anchor={anchor}
      side="bottom"
      align="end"
      gap={4}
      width={300}
      autoFocus
      onDismiss={onDismiss}
      ignore="[data-milestone-settings]"
      role="dialog"
      aria-label={`Milestones in ${name}`}
      tabIndex={-1}
      className="text-content outline-none"
    >
      <p className="px-3.5 pt-3 pb-2.5 text-xs text-content/50">
        Milestones in <b className="font-medium text-content">{name}</b>
      </p>
      <div className="flex items-center gap-2.5 px-3.5 pt-1 pb-3">
        <div className="min-w-0 flex-1">
          <p className="text-[13px]">Milestones for this project</p>
          <p className="text-[11px] text-content/40">
            Off hides the project here. Agents get “Milestones are off for this
            project”.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={settings.enabled}
          aria-label="Milestones for this project"
          onClick={() => onChange({ enabled: !settings.enabled })}
          className={`relative h-5 w-9 shrink-0 rounded-full outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
            settings.enabled ? "bg-accent" : "bg-content/20"
          }`}
        >
          <span
            className={`absolute top-0.5 size-4 rounded-full bg-white ${
              settings.enabled ? "left-4.5" : "left-0.5"
            }`}
          />
        </button>
      </div>
      <p className="border-t border-content/7 px-3.5 pt-2.5 pb-1 text-[11px] text-content/45">
        Show on the timeline
      </p>
      {STATUSES.map((status) => {
        const shown = !settings.hiddenStatuses.includes(status);
        return (
          <label
            key={status}
            className="flex h-8 cursor-pointer items-center gap-2.5 px-3.5"
          >
            <span className="relative grid size-4 shrink-0 place-items-center">
              <input
                type="checkbox"
                aria-label={STATUS_LABELS[status]}
                checked={shown}
                onChange={() =>
                  onChange({
                    hiddenStatuses: hiddenAfter(
                      settings.hiddenStatuses,
                      status,
                      !shown,
                    ),
                  })
                }
                className="peer size-4 cursor-pointer appearance-none rounded border border-content/30 checked:border-accent checked:bg-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              />
              <Check
                aria-hidden
                strokeWidth={3}
                className="pointer-events-none absolute hidden size-[11px] text-white peer-checked:block"
              />
            </span>
            <i
              className={`block size-2.5 shrink-0 rounded-full ${GROUP_DOT[status]}`}
            />
            <span
              className={`flex-1 text-[13px] ${shown ? "" : "text-content/55"}`}
            >
              {STATUS_LABELS[status]}
            </span>
            <span className="text-[11px] text-content/35 tabular-nums">
              {milestones.filter((row) => row.status === status).length}
            </span>
          </label>
        );
      })}
      <p className="px-3.5 pt-2 pb-3 text-[11px] leading-4 text-content/40">
        Hidden milestones stay saved, and agents can still list them.
      </p>
    </Popover>
  );
}
