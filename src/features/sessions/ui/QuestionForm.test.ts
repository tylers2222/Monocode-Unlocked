// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { HabitQuestion } from "../../monos/model/habitQuestion";
import { HabitQuestionCard } from "./HabitQuestionCard";
import { QuestionForm } from "./QuestionForm";
import type {
  UserQuestionPrompt,
  UserQuestionReply,
} from "../model/userQuestion";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function prompt(multiSelect = false): UserQuestionPrompt {
  return {
    requestId: 7,
    questions: [
      {
        id: "colour",
        prompt: "Pick a colour",
        multiSelect,
        allowCustom: false,
        options: [
          { id: "red", label: "Red" },
          { id: "green", label: "Green" },
          { id: "blue", label: "Blue" },
        ],
      },
    ],
  };
}

function renderQuestion(
  onReply: (requestId: number, reply: UserQuestionReply) => void = vi.fn(),
  multiSelect = false,
) {
  act(() =>
    root.render(
      createElement(QuestionForm, {
        prompt: prompt(multiSelect),
        onReply,
      }),
    ),
  );
  return Array.from(
    container.querySelectorAll<HTMLButtonElement>("button[aria-pressed]"),
  );
}

function keyDown(target: Element, key: string) {
  act(() => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
    );
  });
}

describe("QuestionForm keyboard navigation", () => {
  it("moves the highlighted option with arrow keys and selects it with Enter", () => {
    const onReply = vi.fn();
    const options = renderQuestion(onReply);

    expect(options[0].tabIndex).toBe(0);
    expect(options[1].tabIndex).toBe(-1);
    act(() => options[0].focus());

    keyDown(options[0], "ArrowDown");
    expect(document.activeElement).toBe(options[1]);
    expect(options[1].dataset.highlighted).toBe("true");
    expect(options[1].tabIndex).toBe(0);

    keyDown(options[1], "Enter");
    expect(options[1].getAttribute("aria-pressed")).toBe("true");

    act(() =>
      container
        .querySelector<HTMLButtonElement>('button[type="submit"]')!
        .click(),
    );
    expect(onReply).toHaveBeenCalledWith(7, {
      kind: "answered",
      answers: { colour: ["green"] },
    });
  });

  it("wraps arrow navigation and supports Home and End", () => {
    const options = renderQuestion();
    act(() => options[0].focus());

    keyDown(options[0], "ArrowUp");
    expect(document.activeElement).toBe(options[2]);

    keyDown(options[2], "Home");
    expect(document.activeElement).toBe(options[0]);

    keyDown(options[0], "End");
    expect(document.activeElement).toBe(options[2]);
  });

  it("uses number keys to focus and select the matching option", () => {
    const options = renderQuestion();
    act(() => options[0].focus());

    keyDown(options[0], "3");

    expect(document.activeElement).toBe(options[2]);
    expect(options[2].getAttribute("aria-pressed")).toBe("true");
    expect(options[2].getAttribute("aria-keyshortcuts")).toBe("3");
  });

  it("toggles highlighted options for multi-select questions", () => {
    const options = renderQuestion(vi.fn(), true);
    act(() => options[0].focus());

    keyDown(options[0], "Enter");
    keyDown(options[0], "ArrowDown");
    keyDown(options[1], " ");

    expect(options[0].getAttribute("aria-pressed")).toBe("true");
    expect(options[1].getAttribute("aria-pressed")).toBe("true");

    keyDown(options[1], " ");
    expect(options[1].getAttribute("aria-pressed")).toBe("false");
  });
});

describe("QuestionForm steps", () => {
  function twoQuestions(): UserQuestionPrompt {
    return {
      requestId: 9,
      questions: [
        {
          id: "colour",
          prompt: "Pick a colour",
          multiSelect: false,
          allowCustom: false,
          options: [
            { id: "red", label: "Red" },
            { id: "green", label: "Green" },
          ],
        },
        {
          id: "size",
          prompt: "Pick a size",
          multiSelect: false,
          allowCustom: false,
          options: [
            { id: "small", label: "Small" },
            { id: "large", label: "Large" },
          ],
        },
      ],
    };
  }

  function button(label: string): HTMLButtonElement {
    const match = Array.from(container.querySelectorAll("button")).find(
      (item) => item.textContent?.trim() === label,
    );
    if (!match) throw new Error(`No button labelled ${label}`);
    return match;
  }

  it("returns to the previous question with its answer and submits the change", () => {
    const onReply = vi.fn();
    act(() =>
      root.render(createElement(QuestionForm, { prompt: twoQuestions(), onReply })),
    );
    expect(container.textContent).not.toContain("Back");

    act(() => button("Red").click());
    act(() => button("Continue").click());
    expect(container.textContent).toContain("Pick a size");

    act(() => button("Back").click());
    expect(container.textContent).toContain("Pick a colour");
    expect(button("Red").getAttribute("aria-pressed")).toBe("true");

    act(() => button("Green").click());
    act(() => button("Continue").click());
    act(() => button("Large").click());
    act(() => button("Continue").click());

    expect(onReply).toHaveBeenCalledWith(9, {
      kind: "answered",
      answers: { colour: ["green"], size: ["large"] },
    });
  });
});

describe("HabitQuestionCard", () => {
  function asked(end?: HabitQuestion["end"]): HabitQuestion {
    return {
      runId: "run-42",
      runAt: 0,
      end,
      prompt: {
        requestId: 5,
        questions: [
          {
            id: "water",
            prompt: "Water the plants today?",
            multiSelect: false,
            allowCustom: false,
            options: [
              { id: "yes", label: "Yes", description: "Do it now." },
              { id: "no", label: "No", description: "Leave it for later." },
            ],
          },
        ],
      },
    };
  }

  function renderCard(
    end?: HabitQuestion["end"],
    handlers: {
      onReply?: (requestId: number, reply: UserQuestionReply) => void;
      onOpenRun?: (runId: string) => void;
    } = {},
  ) {
    act(() =>
      root.render(
        createElement(HabitQuestionCard, {
          asked: asked(end),
          habit: "Garden round",
          runFrom: "7:30 am",
          onReply: handlers.onReply ?? vi.fn(),
          onOpenRun: handlers.onOpenRun,
        }),
      ),
    );
  }

  function card(): HTMLElement {
    return container.querySelector<HTMLElement>("[data-habit-question]")!;
  }

  function button(label: string): HTMLButtonElement | undefined {
    return Array.from(container.querySelectorAll("button")).find(
      (item) => item.textContent?.trim() === label,
    );
  }

  function topLine(): string | undefined {
    return container
      .querySelector("form > div > span")
      ?.textContent?.replace(/\s+/g, " ")
      .trim();
  }

  it("an open card names the habit and the run, opens the run, and answers", () => {
    const onReply = vi.fn();
    const onOpenRun = vi.fn();
    renderCard(undefined, { onReply, onOpenRun });

    expect(card().dataset.habitQuestion).toBe("open");
    expect(topLine()).toBe("Garden round · run from 7:30 am");
    expect(container.querySelector("[inert]")).toBeNull();
    expect(button("Skip")!.disabled).toBe(false);

    act(() => button("Open run")!.click());
    expect(onOpenRun).toHaveBeenCalledWith("run-42");

    act(() =>
      container
        .querySelector<HTMLButtonElement>("button[aria-pressed]")!
        .click(),
    );
    expect(button("Continue")!.disabled).toBe(false);
    act(() => button("Continue")!.click());
    expect(onReply).toHaveBeenCalledWith(5, {
      kind: "answered",
      answers: { water: ["yes"] },
    });
  });

  it("an open card has no Open run when nothing can open the run", () => {
    renderCard();

    expect(card().dataset.habitQuestion).toBe("open");
    expect(button("Open run")).toBeUndefined();
    expect(button("Continue")).toBeDefined();
  });

  it("an answered or skipped card folds to one line with the habit and run", () => {
    renderCard({ kind: "answered", answer: "Yes, Tomorrow" });

    expect(card().dataset.habitQuestion).toBe("answered");
    expect(container.querySelector("form")).toBeNull();
    expect(card().textContent).toBe(
      "Water the plants today?Yes, Tomorrow·Garden round, 7:30 am run",
    );

    renderCard({ kind: "skipped" });

    expect(card().dataset.habitQuestion).toBe("skipped");
    expect(container.querySelector("form")).toBeNull();
    expect(card().textContent).toBe(
      "Water the plants today?Skipped·Garden round, 7:30 am run",
    );
  });

  it("a card whose run ended stays, dimmed, with the reason and nothing to press", () => {
    const onOpenRun = vi.fn();
    renderCard(undefined, { onOpenRun });
    act(() =>
      container
        .querySelector<HTMLButtonElement>("button[aria-pressed]")!
        .click(),
    );
    expect(button("Continue")!.disabled).toBe(false);

    renderCard({ kind: "stopped" }, { onOpenRun });

    expect(card().dataset.habitQuestion).toBe("stopped");
    expect(topLine()).toBe("Garden round · run from 7:30 am");
    expect(card().textContent).toContain(
      "You stopped the run. Nothing to answer.",
    );
    expect(button("Open run")).toBeUndefined();
    expect(button("Skip")!.disabled).toBe(true);
    expect(button("Continue")!.disabled).toBe(true);
    expect(container.querySelector("[inert] fieldset")).not.toBeNull();

    renderCard({ kind: "timed-out" }, { onOpenRun });

    expect(card().dataset.habitQuestion).toBe("timed-out");
    expect(card().textContent).toContain(
      "No answer in 12 h. The run was stopped.",
    );
    expect(card().textContent).not.toContain("You stopped the run.");
    expect(button("Continue")!.disabled).toBe(true);
  });
});
