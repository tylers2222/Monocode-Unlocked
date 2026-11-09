// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  readProjectDeletePrompt,
  type ProjectDeletePrompt,
} from "../model/projectDeleteCheck";
import { RemoveProjectDialog } from "./RemoveProjectDialog";

vi.mock("../model/projectDeleteCheck", () => ({
  readProjectDeletePrompt: vi.fn(),
}));

let root: Root;
let renders = 0;

async function dialogText(path: string, prompt: ProjectDeletePrompt | "pending") {
  vi.mocked(readProjectDeletePrompt).mockReturnValue(
    prompt === "pending" ? new Promise(() => {}) : Promise.resolve(prompt),
  );
  // A dialog reads its prompt once, when it opens.
  renders += 1;
  await act(async () =>
    root.render(
      createElement(RemoveProjectDialog, {
        key: renders,
        name: "vibtra",
        path,
        onCancel: vi.fn(),
        onConfirm: vi.fn(),
      }),
    ),
  );
  return document.querySelector('[role="dialog"]')!.textContent;
}

const LOCAL = "/code/vibtra";
const REMOTE = "remote://box/code/vibtra";
const scoped = { sessions: 11, scoped: true, spaceName: "Space 2" };

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("what the Delete dialog says", () => {
  it("names the Space on a scoped delete, with no unscoped sentence", async () => {
    const text = await dialogText(LOCAL, scoped);
    expect(text).toContain(
      "All conversations for this project in Space 2 will be deleted. It also leaves the sidebar.",
    );
    expect(text).not.toContain("All conversations for this project will be deleted.");
  });

  it("keeps the old sentence only when this Space is the only one", async () => {
    const text = await dialogText(LOCAL, { ...scoped, scoped: false });
    expect(text).toContain(
      "All conversations for this project will be deleted. It also leaves the sidebar.",
    );
    expect(text).not.toContain("in Space 2");
  });

  it("stays scoped, with no count, until it has read", async () => {
    const text = await dialogText(LOCAL, "pending");
    expect(text).toContain(
      "All conversations for this project in this Space will be deleted.",
    );
    expect(text).not.toContain("saved conversation");
  });

  it("counts this Space's chats", async () => {
    expect(await dialogText(LOCAL, scoped)).toContain(
      "11 saved conversations will be removed.",
    );
    expect(await dialogText(LOCAL, { ...scoped, sessions: 1 })).toContain(
      "1 saved conversation will be removed.",
    );
    expect(await dialogText(LOCAL, { ...scoped, sessions: 0 })).not.toContain(
      "saved conversation",
    );
  });

  it("gives a remote project no count, and says nothing on the machine changes", async () => {
    const text = await dialogText(REMOTE, scoped);
    expect(text).toContain("It leaves Space 2’s sidebar, and nothing on the machine changes.");
    expect(text).not.toContain("saved conversation");
    expect(text).not.toContain("All conversations");
    expect(await dialogText(REMOTE, "pending")).toContain(
      "It leaves this Space’s sidebar",
    );
  });

  it("ignores an answer for a project the dialog has moved off", async () => {
    let answerOld: (prompt: ProjectDeletePrompt) => void = () => {};
    vi.mocked(readProjectDeletePrompt).mockReturnValueOnce(
      new Promise((done) => (answerOld = done)),
    );
    const show = (path: string) =>
      act(async () =>
        root.render(
          createElement(RemoveProjectDialog, {
            name: "vibtra",
            path,
            onCancel: vi.fn(),
            onConfirm: vi.fn(),
          }),
        ),
      );
    await show("/code/old");
    vi.mocked(readProjectDeletePrompt).mockResolvedValueOnce(scoped);
    await show(LOCAL);
    await act(async () => answerOld({ ...scoped, spaceName: "Stale Space" }));
    expect(document.querySelector('[role="dialog"]')!.textContent).toContain("Space 2");
    expect(document.querySelector('[role="dialog"]')!.textContent).not.toContain("Stale");
  });
});
