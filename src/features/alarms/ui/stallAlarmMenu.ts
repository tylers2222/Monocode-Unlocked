import type { ExplorerMenuItem } from "../../files/ui/ExplorerMenu";
import {
  stallMinuteChoices,
  stallMinutesLabel,
  type StallMinutesView,
} from "../model/alarmViews";

const PICK = "stall-alarm:";

/** Frame 5: "Stall alarm ›" beside Remind me, showing the session's value. */
export function stallAlarmMenuItem(view: StallMinutesView): ExplorerMenuItem {
  const setBy = view.setBy ? `set by ${view.setBy}` : undefined;
  return {
    kind: "item",
    id: "stall-alarm",
    label: "Stall alarm",
    shortcut: stallMinutesLabel(view.minutes ?? view.defaultMinutes),
    submenu: [
      {
        kind: "item",
        id: `${PICK}default`,
        label: `Default (${stallMinutesLabel(view.defaultMinutes)})`,
        checked: view.minutes == null,
      },
      ...stallMinuteChoices(view.minutes)
        // Default already names the default, unless it was set on purpose.
        .filter(
          (minutes) =>
            minutes !== view.defaultMinutes || minutes === view.minutes,
        )
        .map((minutes) => ({
          kind: "item" as const,
          id: `${PICK}${minutes}`,
          label: stallMinutesLabel(minutes),
          checked: view.minutes === minutes,
          description: view.minutes === minutes ? setBy : undefined,
        })),
    ],
  };
}

/**
 * The minutes a menu pick sets: a number, null for Default, undefined when
 * the pick is not a stall alarm one.
 */
export function stallAlarmPick(id: string): number | null | undefined {
  if (!id.startsWith(PICK)) return undefined;
  const value = id.slice(PICK.length);
  if (value === "default") return null;
  const minutes = Number(value);
  return Number.isInteger(minutes) && minutes > 0 ? minutes : undefined;
}
