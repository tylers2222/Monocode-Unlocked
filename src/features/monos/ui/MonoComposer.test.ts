// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MonoComposer } from "./MonoComposer";
import { clearComposerDraft } from "../../sessions/model/draftCache";
import { saveComposerAutocorrect } from "../../settings/model/displayPrefs";
import type { BarChoice } from "../../sessions/ui/composerIntent";

const { pick } = vi.hoisted(() => ({ pick: vi.fn() }));
vi.mock("../../sessions/model/attachments", async (original) => ({
  ...(await original<typeof import("../../sessions/model/attachments")>()),
  pickAttachments: pick,
}));
vi.mock("../../sessions/hooks/useFileDrop", () => ({
  useFileDrop: () => false,
}));
// The picker itself is covered on its own; here it just hands over a pick.
vi.mock("../../../shared/ui/EmojiPicker", async () => {
  const { createElement } = await import("react");
  return {
    EmojiPickerPopover: ({ onPick }: { onPick: (emoji: string) => void }) =>
      createElement(
        "button",
        { type: "button", "data-pick": "", onClick: () => onPick("😀") },
        "pick",
      ),
  };
});

let container: HTMLDivElement;
let root: Root;
const file = {
  id: "file",
  name: "note.txt",
  kind: "file" as const,
  mimeType: "text/plain",
  size: 12,
  path: "/tmp/note.txt",
};
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  clearComposerDraft("chat");
  pick.mockReset().mockResolvedValue([file]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  clearComposerDraft("chat");
  vi.unstubAllGlobals();
});
function render(extra: Partial<Parameters<typeof MonoComposer>[0]> = {}) {
  act(() =>
    root.render(
      createElement(MonoComposer, {
        sessionId: "chat",
        name: "Captain",
        onSubmit: () => true,
        ...extra,
      }),
    ),
  );
}
function field() {
  return container.querySelector<HTMLTextAreaElement>(
    '[aria-label="Message Captain"]',
  )!;
}
function type(text: string) {
  const input = field();
  const props = Object.entries(input).find(([key]) =>
    key.startsWith("__reactProps"),
  )![1];
  act(() => props.onChange({ target: { value: text } }));
}
function submit() {
  container
    .querySelector("form")!
    .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}

it("consumes quotes once and adds them to the current draft", () => {
  const consumed = vi.fn();
  render();
  type("My question");
  const quoteRequest = { id: 1, text: "Selected answer" };
  render({ quoteRequest, onQuoteRequestConsumed: consumed });
  expect(field().value).toBe("My question\n\n> Selected answer\n\n");
  render({ quoteRequest, onQuoteRequestConsumed: consumed, enabled: false });
  expect(field().value.match(/Selected answer/g)).toHaveLength(1);
  expect(consumed).toHaveBeenCalledWith(1);
});

it("sends consecutive messages without duplicating rapid Enter presses", () => {
  const onSubmit = vi.fn(() => true);
  render({ onSubmit });
  type("First");
  act(() => {
    submit();
    submit();
  });
  type("Second");
  act(submit);
  expect(onSubmit.mock.calls).toEqual([
    ["First", [], {}],
    ["Second", [], {}],
  ]);
  expect(field().value).toBe("");
});

it("retains text and attachments if the message is rejected", async () => {
  const onSubmit = vi.fn(() => false);
  render({ onSubmit });
  type("Keep these");
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Attach files"]')!
      .click(),
  );
  act(submit);
  expect(onSubmit).toHaveBeenCalledWith("Keep these", [file], {});
  expect(field().value).toBe("Keep these");
  expect(container.querySelector('[title="/tmp/note.txt"]')).not.toBeNull();
});

it("waits for attachment reads so they cannot leak into the next message", async () => {
  let accept!: (files: (typeof file)[]) => void;
  pick.mockReturnValue(
    new Promise((resolve) => {
      accept = resolve;
    }),
  );
  const onSubmit = vi.fn(() => true);
  render({ onSubmit });
  type("With this file");
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Attach files"]')!
      .click(),
  );
  expect(
    container.querySelector<HTMLButtonElement>('[aria-label="Send"]')!.disabled,
  ).toBe(true);
  act(submit);
  expect(onSubmit).not.toHaveBeenCalled();
  await act(async () => accept([file]));
  act(submit);
  expect(onSubmit).toHaveBeenCalledExactlyOnceWith(
    "With this file",
    [file],
    {},
  );
  expect(container.querySelector('[title="/tmp/note.txt"]')).toBeNull();
});

it("waits for delivery and keeps a rejected message available to retry", async () => {
  let acknowledge!: (accepted: boolean) => void;
  const onSubmit = vi.fn(
    () =>
      new Promise<boolean>((resolve) => {
        acknowledge = resolve;
      }),
  );
  render({ onSubmit });
  type("Keep this draft");
  act(() => {
    submit();
    submit();
  });
  expect(onSubmit).toHaveBeenCalledTimes(1);
  expect(field().value).toBe("Keep this draft");
  expect(field().disabled).toBe(true);
  await act(async () => {
    acknowledge(false);
  });
  expect(field().value).toBe("Keep this draft");
  expect(field().disabled).toBe(false);
  act(() => submit());
  await act(async () => {
    acknowledge(true);
  });
  expect(field().value).toBe("");
  expect(onSubmit).toHaveBeenCalledTimes(2);
});

it("follows the composer autocorrect setting", () => {
  render();
  expect(field().getAttribute("spellcheck")).toBe("true");
  expect(field().getAttribute("autocorrect")).toBe("on");

  act(() => saveComposerAutocorrect(false));
  expect(field().getAttribute("spellcheck")).toBe("false");
  expect(field().getAttribute("autocorrect")).toBe("off");

  act(() => saveComposerAutocorrect(true));
  expect(field().getAttribute("spellcheck")).toBe("true");
});

it("inserts a picked emoji where the cursor was, beside attach", async () => {
  render({ controls: createElement("span", null, "Model") });
  const field = container.querySelector("textarea")!;
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!;
    setter.call(field, "hi there");
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
  field.setSelectionRange(2, 2);
  const button = container.querySelector<HTMLButtonElement>(
    'button[aria-label="Add emoji"]',
  )!;
  expect(button.previousElementSibling?.getAttribute("aria-label")).toBe(
    "Attach files",
  );
  // The picker loads on first open.
  await act(async () => button.click());
  await vi.waitFor(() =>
    expect(container.querySelector("[data-pick]")).not.toBeNull(),
  );
  expect(button.getAttribute("aria-expanded")).toBe("true");
  act(() => container.querySelector<HTMLButtonElement>("[data-pick]")!.click());
  expect(field.value).toBe("hi😀 there");
  expect(container.querySelector("[data-pick]")).toBeNull();

  // The floating chat keeps its inline input, without the extra row.
  render();
  expect(container.querySelector('button[aria-label="Add emoji"]')).toBeNull();
});

// Doop E10, decision 536. Defects: text typed as a whole-turn steer opens a
// new turn on its own once the turn ends, Enter clears it unsent, or Send
// looks ready though the bar blocks it. PR I6 review r2 m1: the bar says
// "Nothing running" while a turn still runs, as when the queue starts holding
// the text.
it("holds a whole-turn steer whose turn ended until the user sends it as new", () => {
  const onSubmit = vi.fn(() => true);
  const intent = {
    running: [],
    busy: true,
    steering: true,
    newWaits: false,
    choice: { kind: "default" as const },
    onChoiceChange: vi.fn(),
    monoName: "Captain",
  };
  const send = () =>
    container.querySelector<HTMLButtonElement>('[aria-label="Send"]')!;
  render({ onSubmit, intent });
  type("Stop after the gate rows");
  expect(container.textContent).toContain("everything Captain is doing");

  render({ onSubmit, intent: { ...intent, steering: false } });
  expect(container.querySelector("[data-composer-intent]")).toBeNull();
  expect(send().disabled).toBe(false);

  render({ onSubmit, intent: { ...intent, steering: false, busy: false } });
  expect(container.textContent).toContain("Nothing running");
  expect(send().disabled).toBe(true);
  act(submit);
  expect(onSubmit).not.toHaveBeenCalled();
  expect(field().value).toBe("Stop after the gate rows");

  const asNew = [...container.querySelectorAll("button")].find(
    (button) => button.textContent === "Send as new",
  )!;
  act(() => asNew.click());
  expect(onSubmit).toHaveBeenCalledExactlyOnceWith(
    "Stop after the gate rows",
    [],
    {},
  );
  expect(field().value).toBe("");
});

// PR I6 review r2 M1. Defect: the Mono bar says "Steering" for a typed
// /operator, which the queue holds until the turn ends. /plan and
// /orchestrator are plain text in a Mono chat and steer, so they keep it.
// PR I6 review r3 n3: or it misses the /mono and /monocode aliases.
it("names no running turn for a typed /operator or its aliases, and keeps it for /plan", () => {
  const onSubmit = vi.fn(() => true);
  render({
    onSubmit,
    intent: {
      running: [],
      busy: true,
      steering: true,
      newWaits: false,
      choice: { kind: "default" },
      onChoiceChange: vi.fn(),
      monoName: "Captain",
    },
  });
  for (const command of ["/operator", "/mono", "/monocode"]) {
    type(`${command} list sessions`);
    expect(container.querySelector("[data-composer-intent]")).toBeNull();
    act(submit);
    expect(onSubmit).toHaveBeenLastCalledWith(
      `${command} list sessions`,
      [],
      {},
    );
  }

  for (const text of ["/plan the docs", "/orchestrator the docs"]) {
    type(text);
    expect(
      container.querySelector('[data-composer-intent="turn"]'),
    ).not.toBeNull();
  }
});

// PR I6 review r2 n2 (c96f01e). Defect: ✕ outlives the text it cleared, so
// the next message waits for the turn end though the user never chose that.
it("drops ✕ once its text is emptied", () => {
  const onChoiceChange = vi.fn();
  const intent = {
    running: [],
    busy: true,
    steering: true,
    newWaits: false,
    choice: { kind: "default" } as BarChoice,
    onChoiceChange,
  };
  render({ intent });
  type("Not this one");
  render({ intent: { ...intent, choice: { kind: "cleared" } } });
  expect(onChoiceChange).not.toHaveBeenCalled();
  type("");
  expect(onChoiceChange).toHaveBeenCalledExactlyOnceWith({ kind: "default" });
});
