import { expect, it } from "vitest";
import type { Block } from "../../sessions/model/session";
import type { UserQuestionPrompt } from "../../sessions/model/userQuestion";
import {
  answerHabitQuestion,
  endHabitQuestion,
  type HabitQuestion,
} from "./habitQuestion";

const prompt: UserQuestionPrompt = {
  requestId: 7,
  questions: [
    {
      id: "steps",
      prompt: "Which steps should the run take?",
      multiSelect: true,
      allowCustom: false,
      options: [
        { id: "build", label: "Build" },
        { id: "test", label: "Test" },
        { id: "lint", label: "Lint" },
      ],
    },
    {
      id: "note",
      prompt: "Anything else?",
      multiSelect: false,
      allowCustom: true,
      options: [{ id: "none", label: "Nothing" }],
    },
  ],
};

const asked: HabitQuestion = { runId: "run-1", runAt: 1_000, prompt };

const card = (habitQuestion?: HabitQuestion): Block => ({
  id: "b1",
  role: "assistant",
  text: "",
  monoHabit: { id: "h1", name: "Morning check", at: 1_000 },
  ...(habitQuestion ? { habitQuestion } : {}),
});

it("an answer ends the open card with the chosen labels, once", () => {
  const answered = answerHabitQuestion(card(asked), {
    kind: "answered",
    answers: { steps: ["build", "test"], note: [] },
    custom: { note: "  run it twice " },
  });
  expect(answered).toEqual({
    ...card(asked),
    habitQuestion: {
      ...asked,
      end: { kind: "answered", answer: "Build, Test, run it twice" },
    },
  });

  // The other card answering later changes nothing.
  expect(answerHabitQuestion(answered, { kind: "skipped" })).toBe(answered);
  // A block with no question is not a card.
  const plain = card();
  expect(answerHabitQuestion(plain, { kind: "skipped" })).toBe(plain);
});

it("a skip ends the open card as skipped", () => {
  expect(answerHabitQuestion(card(asked), { kind: "skipped" })).toEqual({
    ...card(asked),
    habitQuestion: { ...asked, end: { kind: "skipped" } },
  });
});

it("a run that ends first closes only a card still open", () => {
  expect(endHabitQuestion(card(asked), { kind: "stopped" })).toEqual({
    ...card(asked),
    habitQuestion: { ...asked, end: { kind: "stopped" } },
  });
  expect(endHabitQuestion(card(asked), { kind: "timed-out" })).toEqual({
    ...card(asked),
    habitQuestion: { ...asked, end: { kind: "timed-out" } },
  });

  const skipped = card({ ...asked, end: { kind: "skipped" } });
  expect(endHabitQuestion(skipped, { kind: "stopped" })).toBe(skipped);
  const plain = card();
  expect(endHabitQuestion(plain, { kind: "timed-out" })).toBe(plain);
});
