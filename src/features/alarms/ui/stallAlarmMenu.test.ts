import { describe, expect, it } from "vitest";
import { stallAlarmMenuItem, stallAlarmPick } from "./stallAlarmMenu";

function picks(item: ReturnType<typeof stallAlarmMenuItem>) {
  if (item.kind !== "item") throw new Error("not an item");
  return (item.submenu ?? []).map((entry) => ({
    label: entry.label,
    checked: entry.checked,
    description: entry.description,
  }));
}

describe("Stall alarm session menu, frame 5", () => {
  it("shows the default and the presets, with Default checked", () => {
    // Defect: the menu hides the session's value, or offers values outside 5 to 240.
    const item = stallAlarmMenuItem({ minutes: null, defaultMinutes: 15 });
    expect(item).toMatchObject({ label: "Stall alarm", shortcut: "15 min" });
    expect(picks(item)).toEqual([
      { label: "Default (15 min)", checked: true, description: undefined },
      { label: "5 min", checked: false, description: undefined },
      { label: "30 min", checked: false, description: undefined },
      { label: "1 hour", checked: false, description: undefined },
      { label: "2 hours", checked: false, description: undefined },
      { label: "4 hours", checked: false, description: undefined },
    ]);
  });

  it("checks a lead's choice and says who set it", () => {
    // Defect: a value a lead set with alarms.set shows as Default, or not at all.
    const item = stallAlarmMenuItem({
      minutes: 45,
      defaultMinutes: 15,
      setBy: "PR-S1",
    });
    expect(item).toMatchObject({ shortcut: "45 min" });
    expect(picks(item).filter((entry) => entry.checked)).toEqual([
      { label: "45 min", checked: true, description: "set by PR-S1" },
    ]);
  });

  it("reads a pick back as minutes, Default as null, anything else as not ours", () => {
    // Defect: picking Default stores 0 minutes, or another menu's pick sets the stall time.
    expect(stallAlarmPick("stall-alarm:60")).toBe(60);
    expect(stallAlarmPick("stall-alarm:default")).toBeNull();
    expect(stallAlarmPick("reminder:1h")).toBeUndefined();
  });
});
