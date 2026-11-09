import { describe, expect, it } from "vitest";
import { projectDeleteText } from "./projectDeleteDialog";

const shared = {
  lastSpace: false,
  sessionCount: 11,
  otherSpaces: ["space-1"],
  hiddenSessionCount: 0,
};
const lastSpace = {
  lastSpace: true,
  sessionCount: 11,
  otherSpaces: [],
  hiddenSessionCount: 0,
};
const base = {
  name: "vibtra",
  path: "/somewhere/vibtra",
  space: "Space 2",
  others: ["Space 1"],
};

describe("the Delete dialog's text", () => {
  it("T10: this Space's chats, and what the other Space keeps", () => {
    expect(projectDeleteText({ ...base, preview: shared })).toEqual({
      title: "Delete “vibtra” from Space 2?",
      summary:
        "Space 2's conversations for this project will be deleted, and it leaves Space 2's sidebar. The folder on disk stays put.",
      keeps: [
        "Space 1 keeps its conversations, and the project's milestones, logo and settings stay.",
      ],
      count: "11 saved conversations in Space 2 will be removed.",
      location: "/somewhere/vibtra",
      confirm: "Delete from Space 2",
    });
  });

  it("T10 reads in the plural for two other Spaces", () => {
    expect(
      projectDeleteText({
        ...base,
        preview: shared,
        others: ["Space 1", "Space 3"],
      }).keeps,
    ).toEqual([
      "Space 1 and Space 3 keep their conversations, and the project's milestones, logo and settings stay.",
    ]);
  });

  it("T11: keeps the line naming milestones, logo and settings", () => {
    expect(projectDeleteText({ ...base, others: [], preview: lastSpace })).toEqual({
      title: "Delete “vibtra”?",
      summary:
        "Space 2 is the last Space with this project. Space 2's conversations and the project's milestones, logo and settings will be deleted. It also leaves the sidebar. The folder on disk stays put.",
      keeps: [],
      count: "11 saved conversations in Space 2 will be removed.",
      location: "/somewhere/vibtra",
      confirm: "Delete",
    });
  });

  it("T11 shows the hidden chats line only when such chats exist", () => {
    const text = (hiddenSessionCount: number) =>
      projectDeleteText({
        ...base,
        others: [],
        preview: { ...lastSpace, hiddenSessionCount },
      }).keeps;
    expect(text(0)).toEqual([]);
    expect(text(959)).toEqual([
      "Conversations hidden in other Spaces stay, and come back if it is added there again.",
    ]);
  });

  it("T11 does not show the hidden chats line for a missing field", () => {
    const old = { lastSpace: true, sessionCount: 1, otherSpaces: [] };
    expect(
      projectDeleteText({
        ...base,
        others: [],
        preview: old as unknown as typeof lastSpace,
      }).keeps,
    ).toEqual([]);
  });

  it("counts one chat in the singular, and none with no line", () => {
    const count = (sessionCount: number) =>
      projectDeleteText({ ...base, preview: { ...shared, sessionCount } }).count;
    expect(count(1)).toBe("1 saved conversation in Space 2 will be removed.");
    expect(count(0)).toBeNull();
  });

  const remote = {
    ...base,
    name: "example-remote",
    path: "remote://env/code/example-remote",
    machine: "example-machine",
  };

  it("T16 shared: no count, and nothing on the machine changes", () => {
    expect(projectDeleteText({ ...remote, preview: shared })).toEqual({
      title: "Delete “example-remote” from Space 2?",
      summary:
        "It leaves Space 2's sidebar, and nothing on example-machine changes. Space 1 keeps the project, and its milestones, logo and settings stay.",
      keeps: [],
      count: null,
      location: "/code/example-remote on example-machine",
      confirm: "Delete from Space 2",
    });
  });

  it("T16 last Space: T11's shared data sentence, the same machine sentence", () => {
    expect(
      projectDeleteText({
        ...remote,
        others: [],
        preview: { ...lastSpace, hiddenSessionCount: 3 },
      }),
    ).toEqual({
      title: "Delete “example-remote”?",
      summary:
        "Space 2 is the last Space with this project. The project's milestones, logo and settings will be deleted. It leaves Space 2's sidebar, and nothing on example-machine changes.",
      keeps: [],
      count: null,
      location: "/code/example-remote on example-machine",
      confirm: "Delete",
    });
  });

  it("never overclaims when the preview cannot be read", () => {
    expect(projectDeleteText({ ...base, others: [], preview: null })).toEqual({
      title: "Delete “vibtra” from Space 2?",
      summary: "All conversations for this project in Space 2 will be deleted.",
      keeps: [],
      count: null,
      location: "/somewhere/vibtra",
      confirm: "Delete from Space 2",
    });
  });
});
