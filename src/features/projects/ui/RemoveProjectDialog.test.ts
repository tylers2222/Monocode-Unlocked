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

let root: Root;
const onCancel = vi.fn();
const onConfirm = vi.fn();
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');

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
  it("opens only once it has read what the delete would do", async () => {
    vi.mocked(readProjectDeleteText).mockReturnValue(new Promise(() => {}));
    await show();
    expect(readProjectDeleteText).toHaveBeenCalledWith("vibtra", LOCAL);
    expect(dialog()).toBeNull();
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
    vi.mocked(readProjectDeleteText).mockReturnValueOnce(
      new Promise((done) => (answerOld = done)),
    );
    await show("/code/old", "old");
    await show();
    await act(async () => answerOld({ ...T10, title: "Stale" }));
    expect(readProjectDeleteText).toHaveBeenLastCalledWith("vibtra", LOCAL);
    expect(dialog()!.textContent).toContain(T10.title);
    expect(dialog()!.textContent).not.toContain("Stale");
  });
});
