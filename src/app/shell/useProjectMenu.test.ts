// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { checkProjectArchive } from "../../features/projects/model/projectDeleteCheck";
import { setHomeDir } from "../../shared/lib/paths";
import { useProjectMenu } from "./useProjectMenu";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => path,
}));
vi.mock("../../platform/tauri/fs", async (original) => ({
  ...(await original<object>()),
  listExternalEditors: vi.fn(async () => [{ id: "code", name: "Code" }]),
  openInExternalEditor: vi.fn(async () => undefined),
}));
vi.mock("../../features/projects/model/projectDeleteCheck", async (original) => ({
  ...(await original<object>()),
  checkProjectArchive: vi.fn(),
  checkProjectDelete: vi.fn(async () => null),
}));

const P = "/work/client";
const onRemoveProject = vi.fn();
let menu: ReturnType<typeof useProjectMenu>;
let root: Root;
let answer: (refusal: string | null) => void;

function Harness() {
  menu = useProjectMenu({ onRemoveProject });
  return menu.element;
}

const alertText = () => document.querySelector('[role="alert"]')?.textContent;

async function pick(label: string) {
  const row = [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')]
    .find((item) => item.textContent === label)!;
  await act(async () => row.click());
}

async function archiveFromMenu() {
  await act(async () => menu.open(P, 10, 10));
  await pick("Archive");
}

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  onRemoveProject.mockReset();
  localStorage.clear();
  setHomeDir("/home/u");
  vi.mocked(checkProjectArchive).mockReturnValue(
    new Promise((done) => (answer = done)),
  );
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(createElement(Harness)));
});

afterEach(() => {
  act(() => root.unmount());
  document.body.replaceChildren();
  setHomeDir(undefined);
  vi.unstubAllGlobals();
});

describe("an Archive whose menu goes away during the check", () => {
  it("archives when the menu stays open", async () => {
    await archiveFromMenu();
    await act(async () => answer(null));
    expect(onRemoveProject).toHaveBeenCalledWith(P, { purgeData: false });
  });

  const now = (leave: () => void) => () => act(async () => leave());

  it.each([
    ["the menu closes", now(() => menu.close()), false],
    ["the menu is dismissed", now(() => menu.dismiss()), false],
    ["another project's menu opens", now(() => menu.open("/work/other", 20, 20)), true],
    ["a group's menu opens", now(() => menu.openGroupMenu("group", 20, 20)), false],
    [
      "Escape closes the menu",
      now(() =>
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })),
      ),
      false,
    ],
    [
      "the menu closes once an editor opens",
      async () => {
        await pick("Open in editor");
        await pick("Code");
      },
      false,
    ],
  ])("archives nothing when %s", async (_, leave, stillOpen) => {
    await archiveFromMenu();
    await leave();
    expect(menu.isOpen).toBe(stillOpen);
    await act(async () => answer(null));
    expect(onRemoveProject).not.toHaveBeenCalled();
  });

  it("shows no refusal in the menu that opened instead", async () => {
    await archiveFromMenu();
    await act(async () => menu.open("/work/other", 20, 20));
    await act(async () => answer("A session is running in client here."));
    expect(alertText()).toBeUndefined();
  });
});

describe("the menu's footer error", () => {
  it("shows a refusal in the menu it belongs to", async () => {
    await archiveFromMenu();
    await act(async () => answer("A session is running in client here."));
    expect(alertText()).toBe("A session is running in client here.");
    expect(menu.isOpen).toBe(true);
  });

  it("is gone when another menu opens", async () => {
    await archiveFromMenu();
    await act(async () => answer("A session is running in client here."));
    await act(async () => menu.open("/work/other", 20, 20));
    expect(menu.isOpen).toBe(true);
    expect(alertText()).toBeUndefined();
  });

  it("is not shown on a menu with no error", async () => {
    await act(async () => menu.open(P, 10, 10));
    expect(document.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });
});
