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

function Harness() {
  removal = useProjectRemoval(spies);
  return removal.dialog;
}

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
  await act(async () => root.render(createElement(Harness)));
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
    expect(spies.closeMenu).toHaveBeenCalledWith(P);
    expect(spies.restoreFocus).toHaveBeenCalled();
    expect(spies.onRemoveProject).toHaveBeenCalledWith(P, { purgeData: false });
    expect(spies.showRefusal).not.toHaveBeenCalled();
  });

  it("shows a refusal and archives nothing", async () => {
    vi.mocked(checkProjectArchive).mockResolvedValue("A session is running.");
    await act(async () => removal.archive(P));
    expect(spies.showRefusal).toHaveBeenCalledWith(P, "A session is running.");
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
    expect(spies.closeMenu).not.toHaveBeenCalled();
  });

  it("treats an empty refusal text as a refusal", async () => {
    vi.mocked(checkProjectArchive).mockResolvedValue("");
    await act(async () => removal.archive(P));
    expect(spies.showRefusal).toHaveBeenCalledWith(P, "");
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
  });
});

describe("Delete", () => {
  it("opens no dialog on a refusal, and shows it in the footer", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue("Take it off that Mono first.");
    await act(async () => removal.askToDelete(TARGET));
    expect(spies.showRefusal).toHaveBeenCalledWith(P, "Take it off that Mono first.");
    expect(dialog()).toBeNull();
    expect(removal.isRemoving).toBe(false);
  });

  it("closes the menu and opens the dialog once the check passes", async () => {
    await act(async () => removal.askToDelete(TARGET));
    expect(spies.closeMenu).toHaveBeenCalledWith(P);
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

  it("reopens the menu with the message when the delete itself rejects", async () => {
    spies.onRemoveProject.mockRejectedValue({ code: "failed", message: "disk full" });
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    expect(spies.reopenMenu).toHaveBeenCalledWith(
      { x: 4, y: 8, path: P, projectKey: P },
      "disk full",
    );
  });

  it("treats an empty refusal after confirm as a refusal", async () => {
    spies.onRemoveProject.mockResolvedValue("");
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    expect(spies.reopenMenu).toHaveBeenCalledWith(
      { x: 4, y: 8, path: P, projectKey: P },
      "",
    );
  });

  it("reopens nothing when the handler answers with no refusal text", async () => {
    spies.onRemoveProject.mockReturnValue(undefined);
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    expect(spies.reopenMenu).not.toHaveBeenCalled();
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
