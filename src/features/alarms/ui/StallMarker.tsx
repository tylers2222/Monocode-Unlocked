import {
  useId,
  useRef,
  useState,
  type FocusEvent,
  type ReactNode,
} from "react";
import { Popover } from "../../../shared/ui/Popover";
import { Clock, MessageQuestion } from "../../../shared/ui/icons";
import {
  causeLine,
  lastActivityText,
  stallChipLabel,
  statusWord,
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

/** Whether the marker's row has keyboard focus, and the id its card takes so the row can point at it. */
export type StallFocus = { open: boolean; cardId: string };

type MarkerProps = { marker: StallMarkerView; focus?: StallFocus };

/**
 * The marker sits inside its row's focus target, so the row opens the card
 * from the keyboard, as hover does: `row(key, stall)` goes on the focusable
 * row, `marker(key)` on the marker inside it.
 */
export function useStallFocus() {
  const baseId = useId();
  const [focused, setFocused] = useState<string | null>(null);
  const cardId = (key: string) => `${baseId}${key}`;
  return {
    marker: (key: string): StallFocus => ({
      open: focused === key,
      cardId: cardId(key),
    }),
    row: (key: string, stall: StallMarkerView | undefined) => ({
      onFocus: (event: FocusEvent<HTMLElement>) =>
        setFocused(
          stall && event.currentTarget.matches(":focus-visible") ? key : null,
        ),
      onBlur: () => setFocused((current) => (current === key ? null : current)),
      describedBy: stall && focused === key ? cardId(key) : undefined,
    }),
  };
}

/** Frame 1: the amber chip that takes a stalled session row's status words. */
export function StallChip({ marker, focus }: MarkerProps) {
  return (
    <StallHover marker={marker} focus={focus}>
      <AlarmChip tone="amber">
        <MarkerIcon marker={marker} />
        {stallChipLabel(marker)}
      </AlarmChip>
    </StallHover>
  );
}

/** Frame 1: a Mono rail row has no room for words, so clock and minutes. */
export function StallRailMarker({ marker, focus }: MarkerProps) {
  return (
    <StallHover marker={marker} focus={focus}>
      <span className="flex shrink-0 items-center gap-[3px] text-[11px] tabular-nums text-amber-400">
        <MarkerIcon marker={marker} />
        {marker.minutes}m
      </span>
    </StallHover>
  );
}

function StallHover({
  marker,
  focus,
  children,
}: MarkerProps & { children: ReactNode }) {
  const anchor = useRef<HTMLSpanElement>(null);
  const hoverId = useId();
  const tooltipId = focus?.cardId ?? hoverId;
  const [hovered, setHovered] = useState(false);
  return (
    <>
      <span
        ref={anchor}
        role="img"
        aria-label={stallChipLabel(marker)}
        aria-describedby={hovered ? tooltipId : undefined}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        className="inline-flex min-w-0"
      >
        {children}
        {hovered || focus?.open ? (
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
      {/* Speaks when the stall starts or its cause changes, not at each minute. */}
      <span role="status" className="sr-only">
        {statusWord(marker.cause)}
      </span>
    </>
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
    ["Told", toldLine(marker)],
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
