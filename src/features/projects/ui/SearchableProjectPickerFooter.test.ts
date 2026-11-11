// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SearchableProjectPicker } from "./SearchableProjectPicker";

let root: Root;
let container: HTMLElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function open(props: object) {
  act(() =>
    root.render(
      createElement(SearchableProjectPicker, {
        cwd: "/code/app",
        recents: [{ path: "/code/app", openedAt: 1 }],
        compact: true,
        onSelectProject: vi.fn(),
        ...props,
      }),
    ),
  );
  act(() => container.querySelector("button")!.click());
  return document.querySelector('[aria-label="Project picker"]')!;
}

const footer = (picker: Element) => picker.querySelector('[class~="border-t"]');

it("has no footer when it offers nothing to add", () => {
  expect(footer(open({}))).toBeNull();
});

it("has a footer with New project alone", () => {
  const row = footer(open({ onOpenProject: vi.fn() }));
  expect(row?.textContent).toBe("New project");
});

it("has a footer with a footer action alone", () => {
  const row = footer(
    open({ footerAction: () => createElement("button", null, "Extra") }),
  );
  expect(row?.textContent).toBe("Extra");
});
