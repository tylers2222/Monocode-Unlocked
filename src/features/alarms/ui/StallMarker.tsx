import { useId, useRef, useState, type ReactNode } from "react";
import { Popover } from "../../../shared/ui/Popover";
import { Clock, MessageQuestion } from "../../../shared/ui/icons";
import {
  causeLine,
  lastActivityText,
  stallChipLabel,
  toldLine,
  type StallMarkerView,
} from "../model/alarmViews";
import { AlarmChip } from "./alarmParts";

function MarkerIcon({ marker }: { marker: StallMarkerView }) {
  return marker.cause === "dialog" ? (
    <MessageQuestion className="size-2.5 shrink-0" strokeWidth={2} />
  ) : (
    <Clock className="size-2.5 shrink-0" strokeWidth={2} />
  );
}

/** Frame 1: the amber chip that takes a stalled session row's status words. */
export function StallChip({ marker }: { marker: StallMarkerView }) {
  return (
    <StallHover marker={marker}>
      <AlarmChip tone="amber">
        <MarkerIcon marker={marker} />
        {stallChipLabel(marker)}
      </AlarmChip>
    </StallHover>
  );
}

/** Frame 1: a Mono rail row has no room for words, so clock and minutes. */
export function StallRailMarker({ marker }: { marker: StallMarkerView }) {
  return (
    <StallHover marker={marker}>
      <span className="flex shrink-0 items-center gap-[3px] text-[11px] tabular-nums text-amber-400">
        <MarkerIcon marker={marker} />
        {marker.minutes}m
      </span>
    </StallHover>
  );
}

function StallHover({
  marker,
  children,
}: {
  marker: StallMarkerView;
  children: ReactNode;
}) {
  const anchor = useRef<HTMLSpanElement>(null);
  const tooltipId = useId();
  const [open, setOpen] = useState(false);
  return (
    <span
      ref={anchor}
      role="status"
      aria-label={stallChipLabel(marker)}
      aria-describedby={open ? tooltipId : undefined}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      className="inline-flex min-w-0"
    >
      {children}
      {open ? (
        <Popover
          anchor={anchor}
          side="right"
          width={300}
          role="tooltip"
          id={tooltipId}
          className="pointer-events-none px-3 py-2.5"
        >
          <StallDetails marker={marker} />
        </Popover>
      ) : null}
    </span>
  );
}

/** Why the agent is stalled and who was told. */
export function StallDetails({ marker }: { marker: StallMarkerView }) {
  const rows: [string, string][] = [
    ["Cause", causeLine(marker)],
    ["Open work", marker.openWork.join("; ") || "none"],
    [
      "Last activity",
      marker.lastActivity ? lastActivityText(marker.lastActivity) : "none",
    ],
    ["Told", toldLine(marker.sends)],
  ];
  return (
    <div className="flex flex-col gap-1 text-[12px] leading-[18px] text-content/70">
      <div className="font-medium text-content">
        {marker.stalled.name} is stalled since {marker.stalledSince}
      </div>
      <dl className="flex flex-col">
        {rows.map(([label, value]) => (
          <div key={label} className="flex min-w-0 gap-2">
            <dt className="w-[78px] shrink-0 text-content/40">{label}</dt>
            <dd className="min-w-0 flex-1 break-words">{value}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
