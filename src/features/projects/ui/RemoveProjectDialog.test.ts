// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  readProjectDeleteText,
  type ProjectDeleteText,
} from "../model/projectDeleteDialog";
import { RemoveProjectDialog } from "./RemoveProjectDialog";

vi.mock("../model/projectDeleteDialog", () => ({
  readProjectDeleteText: vi.fn(),
}));

const LOCAL = "/code/vibtra";
const T10: ProjectDeleteText = {
  title: "Delete “vibtra” from Space 2?",
  summary: "Space 2's conversations for this project will be deleted.",
  keeps: ["Space 1 keeps its conversations."],
  count: "11 saved conversations in Space 2 will be removed.",
  location: "~/code/vibtra",
  confirm: "Delete from Space 2",
};

const T11: ProjectDeleteText = {
  title: "Delete “vibtra”?",
  summary:
    "Space 2 is the last Space with this project. Space 2's conversations and the project's milestones, logo and settings will be deleted. It also leaves the sidebar. The folder on disk stays put.",
  keeps: [],
  count: null,
  location: "~/code/vibtra",
  confirm: "Delete",
};

let root: Root;
const onCancel = vi.fn();
const onConfirm = vi.fn();
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');
const buttons = () =>
  [...dialog()!.querySelectorAll<HTMLButtonElement>("button")].map(
    (button) => button.textContent,
  );
const press = (label: string) =>
  act(async () =>
    [...dialog()!.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent === label)!
      .click(),
  );

const show = (path = LOCAL, name = "vibtra") =>
  act(async () =>
    root.render(
      createElement(RemoveProjectDialog, { name, path, onCancel, onConfirm }),
    ),
  );

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  onCancel.mockReset();
  onConfirm.mockReset();
  vi.mocked(readProjectDeleteText).mockReset();
  vi.mocked(readProjectDeleteText).mockResolvedValue(T10);
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("the Delete dialog (frame 22)", () => {
  it("is visible at once, reading, with no Delete until it has read (decision 8)", async () => {
    vi.mocked(readProjectDeleteText).mockReturnValue(new Promise(() => {}));
    await show();
    expect(readProjectDeleteText).toHaveBeenCalledWith("vibtra", LOCAL);
    expect(dialog()!.textContent).toContain("Delete “vibtra”");
    expect(dialog()!.textContent).toContain("Checking what this delete would do…");
    expect(buttons()).toEqual(["Cancel"]);
    expect(document.activeElement?.textContent).toBe("Cancel");
  });

  it("offers no Delete on a failed read, only Retry (decision 7)", async () => {
    vi.mocked(readProjectDeleteText).mockResolvedValue(null);
    await show();
    expect(dialog()!.querySelector('[role="alert"]')?.textContent).toBe(
      "Could not read what this delete would do. Nothing was deleted.",
    );
    expect(buttons()).toEqual(["Cancel", "Retry"]);
    expect(dialog()!.textContent).not.toContain("will be deleted");
  });

  it("reads again on Retry, and then shows the last-Space text and its Delete", async () => {
    vi.mocked(readProjectDeleteText).mockResolvedValueOnce(null);
    await show();
    let answer: (text: ProjectDeleteText) => void = () => {};
    vi.mocked(readProjectDeleteText).mockReturnValueOnce(
      new Promise((done) => (answer = done)),
    );
    await press("Retry");
    expect(readProjectDeleteText).toHaveBeenCalledTimes(2);
    expect(buttons()).toEqual(["Cancel"]);
    await act(async () => answer(T11));
    expect(dialog()!.textContent).toContain(
      "the project's milestones, logo and settings will be deleted",
    );
    expect(buttons()).toEqual(["Cancel", "Delete"]);
    await press("Delete");
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("cancels while it reads", async () => {
    vi.mocked(readProjectDeleteText).mockReturnValue(new Promise(() => {}));
    await show();
    await press("Cancel");
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("shows the title, what goes, what stays, the count and where it is", async () => {
    await show();
    const lines = [...dialog()!.querySelectorAll("h2, p")].map(
      (line) => line.textContent,
    );
    expect(lines).toEqual([
      T10.title,
      T10.summary,
      ...T10.keeps,
      T10.count,
      T10.location,
    ]);
  });

  it("has no count line when there is no count", async () => {
    vi.mocked(readProjectDeleteText).mockResolvedValue({ ...T10, count: null, keeps: [] });
    await show();
    const lines = [...dialog()!.querySelectorAll("p")].map((line) => line.textContent);
    expect(lines).toEqual([T10.summary, T10.location]);
  });

  it("confirms with the button the text names, and focuses Cancel first", async () => {
    await show();
    const buttons = [...dialog()!.querySelectorAll<HTMLButtonElement>("button")];
    expect(document.activeElement?.textContent).toBe("Cancel");
    const confirm = buttons.find((button) => button.textContent === T10.confirm)!;
    await act(async () => confirm.click());
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("reads again for another project, and drops the old answer", async () => {
    let answerOld: (text: ProjectDeleteText) => void = () => {};
    let answerNew: (text: ProjectDeleteText) => void = () => {};
    vi.mocked(readProjectDeleteText)
      .mockReturnValueOnce(new Promise((done) => (answerOld = done)))
      .mockReturnValueOnce(new Promise((done) => (answerNew = done)));
    await show("/code/old", "old");
    await show();
    expect(readProjectDeleteText).toHaveBeenLastCalledWith("vibtra", LOCAL);
    await act(async () => answerOld({ ...T10, title: "Stale" }));
    expect(dialog()!.textContent).not.toContain("Stale");
    expect(buttons()).toEqual(["Cancel"]);
    await act(async () => answerNew(T10));
    expect(dialog()!.textContent).toContain(T10.title);
    expect(buttons()).toEqual(["Cancel", T10.confirm]);
  });

  it("shows a project it moved to as reading, never with the last one's Delete", async () => {
    await show("/code/old", "old");
    expect(buttons()).toEqual(["Cancel", T10.confirm]);
    vi.mocked(readProjectDeleteText).mockReturnValueOnce(new Promise(() => {}));
    await show();
    expect(buttons()).toEqual(["Cancel"]);
  });
});
