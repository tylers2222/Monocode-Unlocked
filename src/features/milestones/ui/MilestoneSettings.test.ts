// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { MilestoneSettings as Settings } from "../model/milestones";
import { MilestoneSettings } from "./MilestoneSettings";

let root: Root;
let anchor: HTMLButtonElement;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  anchor = document.createElement("button");
  document.body.append(anchor);
  root = createRoot(document.createElement("div"));
});

afterEach(async () => {
  await act(async () => root.unmount());
  anchor.remove();
  vi.unstubAllGlobals();
});

/** Renders the settings and returns each patch the controls send. */
async function patchesFrom(settings: Settings, click: string[]) {
  const onChange = vi.fn();
  await act(async () =>
    root.render(
      createElement(MilestoneSettings, {
        anchor,
        name: "orbit",
        settings,
        milestones: [],
        onChange,
        onDismiss: vi.fn(),
      }),
    ),
  );
  for (const label of click)
    await act(async () =>
      document.querySelector<HTMLElement>(`[aria-label="${label}"]`)!.click(),
    );
  return onChange.mock.calls.map(([patch]) => patch);
}

it("the switch sends only the flipped enabled flag", async () => {
  expect(
    await patchesFrom({ enabled: true, hiddenStatuses: ["done"] }, [
      "Milestones for this project",
    ]),
  ).toEqual([{ enabled: false }]);
  expect(
    await patchesFrom({ enabled: false, hiddenStatuses: [] }, [
      "Milestones for this project",
    ]),
  ).toEqual([{ enabled: true }]);
});

it("each status box sends the whole hidden list with that status hidden or shown", async () => {
  expect(
    await patchesFrom({ enabled: true, hiddenStatuses: ["idea"] }, [
      "In progress",
      "Planned",
      "Idea",
      "Done",
    ]),
  ).toEqual([
    { hiddenStatuses: ["idea", "in_progress"] },
    { hiddenStatuses: ["idea", "planned"] },
    { hiddenStatuses: [] },
    { hiddenStatuses: ["idea", "done"] },
  ]);
});
