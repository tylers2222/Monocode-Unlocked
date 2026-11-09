// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import { useProjectRemoval } from "./useProjectRemoval";

vi.mock("../../features/projects/model/projectDeleteCheck", () => ({
  checkProjectArchive: vi.fn(),
  checkProjectDelete: vi.fn(),
  readProjectDeletePrompt: vi.fn(async () => ({
    sessions: 0,
    scoped: true,
    spaceName: "Space 2",
  })),
}));

const P = "/code/vibtra";
const TARGET = { x: 4, y: 8, path: P, projectKey: P, name: "vibtra" };

let root: Root;
let removal: ReturnType<typeof useProjectRemoval>;
const spies = {
  closeMenu: vi.fn(),
  reopenMenu: vi.fn(),
  showRefusal: vi.fn(),
  restoreFocus: vi.fn(),
  onRemoveProject: vi.fn(),
};

function Harness({ menuPath }: { menuPath: string | undefined }) {
  removal = useProjectRemoval({ menuPath, ...spies });
  return removal.dialog;
}

const render = (menuPath: string | undefined) =>
  act(async () => root.render(createElement(Harness, { menuPath })));

const dialog = () => document.querySelector('[role="dialog"]');
const press = (label: string) =>
  act(async () =>
    [...document.querySelectorAll("button")]
      .find((button) => button.textContent === label)!
      .click(),
  );

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  for (const spy of Object.values(spies)) spy.mockReset();
  vi.mocked(checkProjectArchive).mockResolvedValue(null);
  vi.mocked(checkProjectDelete).mockResolvedValue(null);
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await render(P);
});

afterEach(() => {
  act(() => root.unmount());
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("Archive (T9)", () => {
  it("archives once the check passes, closing the menu first", async () => {
    await act(async () => removal.archive(P));
    expect(checkProjectArchive).toHaveBeenCalledWith(P);
    expect(spies.closeMenu).toHaveBeenCalled();
    expect(spies.restoreFocus).toHaveBeenCalled();
    expect(spies.onRemoveProject).toHaveBeenCalledWith(P, { purgeData: false });
    expect(spies.showRefusal).not.toHaveBeenCalled();
  });

  it("shows a refusal and archives nothing", async () => {
    vi.mocked(checkProjectArchive).mockResolvedValue("A session is running.");
    await act(async () => removal.archive(P));
    expect(spies.showRefusal).toHaveBeenCalledWith("A session is running.");
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
    expect(spies.closeMenu).not.toHaveBeenCalled();
  });

  it("does nothing if the menu closed or moved while it checked", async () => {
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    act(() => removal.archive(P));
    await render("/code/other");
    await act(async () => answer(null));
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
    expect(spies.showRefusal).not.toHaveBeenCalled();
  });
});

describe("Delete", () => {
  it("opens no dialog on a refusal, and shows it in the footer", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue("Take it off that Mono first.");
    await act(async () => removal.askToDelete(TARGET));
    expect(spies.showRefusal).toHaveBeenCalledWith("Take it off that Mono first.");
    expect(dialog()).toBeNull();
    expect(removal.isRemoving).toBe(false);
  });

  it("closes the menu and opens the dialog once the check passes", async () => {
    await act(async () => removal.askToDelete(TARGET));
    expect(spies.closeMenu).toHaveBeenCalled();
    expect(dialog()).not.toBeNull();
    expect(removal.isRemoving).toBe(true);
  });

  it("deletes on confirm and reopens nothing when it succeeds", async () => {
    spies.onRemoveProject.mockResolvedValue(null);
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    expect(spies.onRemoveProject).toHaveBeenCalledWith(P, { purgeData: true });
    expect(dialog()).toBeNull();
    expect(spies.restoreFocus).toHaveBeenCalled();
    expect(spies.reopenMenu).not.toHaveBeenCalled();
  });

  it("reopens the menu where it was, with a refusal that comes after confirm", async () => {
    spies.onRemoveProject.mockResolvedValue("A session is running.");
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    expect(dialog()).toBeNull();
    expect(spies.reopenMenu).toHaveBeenCalledWith(
      { x: 4, y: 8, path: P, projectKey: P },
      "A session is running.",
    );
  });

  it("deletes nothing on cancel, and gives focus back", async () => {
    await act(async () => removal.askToDelete(TARGET));
    await press("Cancel");
    expect(dialog()).toBeNull();
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
    expect(spies.restoreFocus).toHaveBeenCalled();
  });

  it("closes the dialog on dismiss", async () => {
    await act(async () => removal.askToDelete(TARGET));
    await act(async () => removal.dismiss());
    expect(dialog()).toBeNull();
    expect(removal.isRemoving).toBe(false);
  });
});
