import { describe, expect, it } from "vitest";
import { projectRemovalItems, REMOVE_FROM_SPACE } from "./projectRemovalItems";

const rows = (
  facts: Parameters<typeof projectRemovalItems>[1],
  remote = false,
) =>
  projectRemovalItems("Space 2", facts, remote).map((item) => [
    item.id,
    item.label,
    item.description,
  ]);

describe("the row menu's Remove, Archive and Delete (frame 20)", () => {
  it("offers all three, with their lines, on a shared row", () => {
    expect(rows({ offersRemove: true })).toEqual([
      [
        REMOVE_FROM_SPACE,
        "Remove from Space 2",
        "Its conversations stay. They come back if you add it again.",
      ],
      ["archive", "Archive", "Hides it in every Space."],
      ["delete", "Delete", "Space 2's conversations only. You confirm next."],
    ]);
  });

  it("keeps today's two rows, with no lines, on a row only in this Space", () => {
    expect(rows({ offersRemove: false })).toEqual([
      ["archive", "Archive", undefined],
      ["delete", "Delete", undefined],
    ]);
  });

  it("offers no Remove until the row's facts are read", () => {
    expect(rows(null)).toEqual([
      ["archive", "Archive", undefined],
      ["delete", "Delete", undefined],
    ]);
  });

  it("names the machine on a remote row's Remove, and keeps Delete's line", () => {
    expect(rows({ offersRemove: true, machine: "example-machine" }, true)).toEqual([
      [REMOVE_FROM_SPACE, "Remove from Space 2", "Nothing on example-machine changes."],
      ["archive", "Archive", "Hides it in every Space."],
      [
        "delete",
        "Delete",
        "Takes it off this Space. Nothing on the machine changes.",
      ],
    ]);
  });

  it("says the machine is untouched on a remote row only in this Space", () => {
    expect(rows({ offersRemove: false, machine: "m" }, true)).toEqual([
      ["archive", "Archive", undefined],
      [
        "delete",
        "Delete",
        "Takes it off this Space. Nothing on the machine changes.",
      ],
    ]);
  });

  it("sets the group off with one separator, on its first row", () => {
    for (const offersRemove of [true, false]) {
      expect(
        projectRemovalItems("Space 2", { offersRemove }, false).map(
          (item) => item.sepBefore === true,
        ),
      ).toEqual(offersRemove ? [true, false, false] : [true, false]);
    }
  });
});
