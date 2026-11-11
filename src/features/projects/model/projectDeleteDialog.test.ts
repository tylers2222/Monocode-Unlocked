import { describe, expect, it, vi } from "vitest";
import { previewProjectDelete } from "../../spaces/model/spaces";
import { remoteMachineName } from "./projectRowActions";
import { projectDeleteText, readProjectDeleteText } from "./projectDeleteDialog";

vi.mock("../../spaces/model/spaces", () => ({ previewProjectDelete: vi.fn() }));
vi.mock("../../spaces/model/activeSpace", () => ({
  spacesView: () => ({
    spaces: [
      { id: "space-1", name: "Space 1" },
      { id: "space-2", name: "Space 2" },
    ],
    activeSpaceId: "space-2",
  }),
}));
vi.mock("./projectRowActions", () => ({
  activeSpaceName: () => "Space 2",
  remoteMachineName: vi.fn(async () => "example-machine"),
}));

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

  it("T10 lists three other Spaces with commas", () => {
    expect(
      projectDeleteText({
        ...base,
        preview: shared,
        others: ["Space 1", "Space 3", "Space 4"],
      }).keeps,
    ).toEqual([
      "Space 1, Space 3 and Space 4 keep their conversations, and the project's milestones, logo and settings stay.",
    ]);
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
});

describe("readProjectDeleteText", () => {
  it("names the other Spaces from the preview, and an unknown id as it is", async () => {
    vi.mocked(previewProjectDelete).mockResolvedValue({
      ...shared,
      otherSpaces: ["space-1", "space-9"],
    });
    const text = await readProjectDeleteText("vibtra", "/somewhere/vibtra");
    expect(previewProjectDelete).toHaveBeenCalledWith("/somewhere/vibtra");
    expect(text?.title).toBe("Delete “vibtra” from Space 2?");
    expect(text?.keeps).toEqual([
      "Space 1 and space-9 keep their conversations, and the project's milestones, logo and settings stay.",
    ]);
  });

  it("has no text when the preview fails, so the dialog offers no Delete (decision 7)", async () => {
    vi.mocked(previewProjectDelete).mockRejectedValue(new Error("no database"));
    await expect(
      readProjectDeleteText("vibtra", "/somewhere/vibtra"),
    ).resolves.toBeNull();
  });

  it("reads the machine's name only for a remote project", async () => {
    vi.mocked(previewProjectDelete).mockResolvedValue(shared);
    vi.mocked(remoteMachineName).mockClear();
    const local = await readProjectDeleteText("vibtra", "/somewhere/vibtra");
    expect(remoteMachineName).not.toHaveBeenCalled();
    expect(local?.summary).not.toContain("example-machine");
    const remote = await readProjectDeleteText("vibtra", "remote://env/code/vibtra");
    expect(remoteMachineName).toHaveBeenCalledWith("remote://env/code/vibtra");
    expect(remote?.summary).toContain("nothing on example-machine changes");
  });
});
