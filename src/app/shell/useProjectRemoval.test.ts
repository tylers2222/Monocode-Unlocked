// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import {
  readProjectRowFacts,
  type ProjectRowFacts,
} from "../../features/projects/model/projectRowActions";
import { REMOVE_FROM_SPACE } from "../../features/projects/ui/projectRemovalItems";
import { useProjectRemoval } from "./useProjectRemoval";

vi.mock("../../features/projects/model/projectRowActions", () => ({
  activeSpaceName: () => "Space 2",
  readProjectRowFacts: vi.fn(),
}));

vi.mock("../../features/projects/model/projectDeleteCheck", () => ({
  checkProjectArchive: vi.fn(),
  checkProjectDelete: vi.fn(),
}));// The dialog's text has its own tests; here it is a last-Space delete.
vi.mock("../../features/projects/model/projectDeleteDialog", () => ({
  readProjectDeleteText: vi.fn(async (name: string) => ({
    title: `Delete “${name}”?`,
    summary: "",
    keeps: [],
    count: null,
    location: "",
    confirm: "Delete",
  })),
}));


const P = "/code/vibtra";
const TARGET = { x: 4, y: 8, path: P, projectKey: P, name: "vibtra" };

let root: Root;
let removal: ReturnType<typeof useProjectRemoval>;
let menuPath: string | undefined;
const spies = {
  closeMenu: vi.fn(),
  reopenMenu: vi.fn(),
  showRefusal: vi.fn(),
  restoreFocus: vi.fn(),
  onRemoveProject: vi.fn(),
};

function Harness() {
  removal = useProjectRemoval({ ...spies, menuPath });
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
  menuPath = undefined;
  vi.mocked(readProjectRowFacts).mockReset();
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

  it("does not archive if the menu closed during the check, even when allowed", async () => {
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    await act(async () => removal.archive(P));
    await act(async () => removal.cancelCheck());
    await act(async () => answer(null));
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
    expect(spies.closeMenu).not.toHaveBeenCalled();
    expect(spies.restoreFocus).not.toHaveBeenCalled();
  });

  it("drops a refusal for a menu that closed during the check", async () => {
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    await act(async () => removal.archive(P));
    await act(async () => removal.cancelCheck());
    await act(async () => answer("A session is running."));
    expect(spies.showRefusal).not.toHaveBeenCalled();
  });

  it("cancels the latest check from a close handler of an older render", async () => {
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    const older = removal;
    await act(async () => removal.archive(P));
    await act(async () => older.cancelCheck());
    await act(async () => answer(null));
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
  });

  it("drops the first check when a second one starts", async () => {
    let first: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValueOnce(
      new Promise((done) => (first = done)),
    );
    await act(async () => removal.archive(P));
    await act(async () => removal.archive("/code/other"));
    expect(spies.onRemoveProject).toHaveBeenCalledTimes(1);
    expect(spies.onRemoveProject).toHaveBeenCalledWith("/code/other", { purgeData: false });
    await act(async () => first(null));
    expect(spies.onRemoveProject).toHaveBeenCalledTimes(1);
  });

  it("has nothing to cancel before any check", async () => {
    await act(async () => removal.cancelCheck());
    expect(spies.onRemoveProject).not.toHaveBeenCalled();
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

  it("opens no dialog if the menu closed during the check, even when allowed", async () => {
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectDelete).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    await act(async () => removal.askToDelete(TARGET));
    await act(async () => removal.cancelCheck());
    await act(async () => answer(null));
    expect(dialog()).toBeNull();
    expect(spies.closeMenu).not.toHaveBeenCalled();
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

  it("drops a check started while the delete ran, when its refusal reopens the menu", async () => {
    let refuse: (refusal: string | null) => void = () => {};
    spies.onRemoveProject.mockReturnValueOnce(
      new Promise((done) => (refuse = done)),
    );
    let answer: (refusal: string | null) => void = () => {};
    vi.mocked(checkProjectArchive).mockReturnValue(
      new Promise((done) => (answer = done)),
    );
    await act(async () => removal.askToDelete(TARGET));
    await press("Delete");
    await act(async () => removal.archive("/code/other"));
    await act(async () => refuse("A session is running."));
    await act(async () => answer(null));
    expect(spies.reopenMenu).toHaveBeenCalledTimes(1);
    expect(spies.onRemoveProject).toHaveBeenCalledTimes(1);
    expect(spies.closeMenu).toHaveBeenCalledTimes(1);
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

describe("Remove from this Space (T5, T6)", () => {
  const remove = () => act(async () => removal.leaveSpace(P));

  it("asks the window to leave the Space, then closes the menu and gives focus back", async () => {
    spies.onRemoveProject.mockResolvedValue(null);
    await remove();
    expect(spies.onRemoveProject).toHaveBeenCalledWith(P, {
      purgeData: false,
      leaveSpace: true,
    });
    expect(spies.closeMenu).toHaveBeenCalledWith(P);
    expect(spies.restoreFocus).toHaveBeenCalled();
    expect(spies.showRefusal).not.toHaveBeenCalled();
  });

  it("closes the menu when the handler answers nothing", async () => {
    await remove();
    expect(spies.closeMenu).toHaveBeenCalledWith(P);
  });

  it("keeps the menu open with the refusal in its footer", async () => {
    spies.onRemoveProject.mockResolvedValue("Stop it first.");
    await remove();
    expect(spies.showRefusal).toHaveBeenCalledWith(P, "Stop it first.");
    expect(spies.closeMenu).not.toHaveBeenCalled();
    expect(spies.restoreFocus).not.toHaveBeenCalled();
  });

  it("shows the message when the handler rejects", async () => {
    spies.onRemoveProject.mockRejectedValue({ code: "failed", message: "database is locked" });
    await remove();
    expect(spies.showRefusal).toHaveBeenCalledWith(P, "database is locked");
    expect(spies.closeMenu).not.toHaveBeenCalled();
  });

  it("drops the answer for a menu that closed while it ran", async () => {
    let answer: (refusal: string | null) => void = () => {};
    spies.onRemoveProject.mockReturnValue(
      new Promise<string | null>((done) => (answer = done)),
    );
    await remove();
    removal.cancelCheck();
    await act(async () => answer("Stop it first."));
    expect(spies.showRefusal).not.toHaveBeenCalled();
    expect(spies.closeMenu).not.toHaveBeenCalled();
  });
});

describe("the menu's rows (frame 20)", () => {
  const ids = (path: string) => removal.menuItems(path).map((item) => item.id);
  const facts = (offersRemove: boolean): Promise<ProjectRowFacts> =>
    Promise.resolve({ offersRemove });

  it("offers Remove once the open menu's row reads as shared", async () => {
    vi.mocked(readProjectRowFacts).mockReturnValue(facts(true));
    menuPath = P;
    await act(async () => root.render(createElement(Harness)));
    expect(readProjectRowFacts).toHaveBeenCalledWith(P);
    expect(ids(P)).toEqual([REMOVE_FROM_SPACE, "archive", "delete"]);
  });

  it("has today's two before any read, and for a row only in this Space", async () => {
    expect(ids(P)).toEqual(["archive", "delete"]);
    vi.mocked(readProjectRowFacts).mockReturnValue(facts(false));
    menuPath = P;
    await act(async () => root.render(createElement(Harness)));
    expect(ids(P)).toEqual(["archive", "delete"]);
  });

  it("never uses the last menu's answer for another row", async () => {
    vi.mocked(readProjectRowFacts).mockReturnValueOnce(facts(true));
    menuPath = P;
    await act(async () => root.render(createElement(Harness)));
    vi.mocked(readProjectRowFacts).mockReturnValueOnce(new Promise(() => {}));
    menuPath = "/code/other";
    await act(async () => root.render(createElement(Harness)));
    expect(ids("/code/other")).toEqual(["archive", "delete"]);
  });

  it("drops an answer that comes after the menu moved on", async () => {
    let answer: (read: ProjectRowFacts) => void = () => {};
    vi.mocked(readProjectRowFacts).mockReturnValueOnce(
      new Promise((done) => (answer = done)),
    );
    menuPath = P;
    await act(async () => root.render(createElement(Harness)));
    vi.mocked(readProjectRowFacts).mockReturnValueOnce(facts(false));
    menuPath = "/code/other";
    await act(async () => root.render(createElement(Harness)));
    await act(async () => answer({ offersRemove: true }));
    expect(ids("/code/other")).toEqual(["archive", "delete"]);
    expect(ids(P)).toEqual(["archive", "delete"]);
  });
});
