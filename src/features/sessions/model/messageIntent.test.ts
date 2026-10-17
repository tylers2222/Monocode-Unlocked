import { expect, it } from "vitest";
import type {
  Handle,
  MessageId,
  SessionId,
  TurnId,
} from "../../monos/model/monoMessages";
import {
  excerpt,
  intentLine,
  targetRef,
  type ReplyTarget,
} from "./messageIntent";

// Design 16.7 step 2 and 5. Defect: a ninth word reaches the line, or a text
// that was not cut gets a false cut mark.
it("keeps the first 8 words and marks a cut only when one was made", () => {
  expect(excerpt("one two three four five six seven eight nine")).toBe(
    "one two three four five six seven eight…",
  );
  expect(excerpt("one two three four five six seven eight")).toBe(
    "one two three four five six seven eight",
  );
});

// Design 16.7 step 1. Defect: a line break inside the line, which ends it
// early for the agent that reads it.
it("puts newlines, tabs, U+2028 and other control characters on one line", () => {
  expect(excerpt("  Fix\nthe\r\n\tlogin\u2028bug\u00a0now\u0007please  ")).toBe(
    "Fix the login bug now please",
  );
});

// Design 16.7 step 3. Defect: a code unit cut splits a ZWJ sequence, so the
// line ends in half an emoji.
it("cuts at 60 grapheme clusters and keeps an emoji at the cut whole", () => {
  const family = "👨‍👩‍👧";
  expect(excerpt(`${"a".repeat(59)}${family}bbb`)).toBe(
    `${"a".repeat(59)}${family}…`,
  );
});

// Design 16.7 step 3. Defect: a text with no spaces is never cut.
it("cuts a text with no spaces at 60 clusters", () => {
  expect(excerpt("漢".repeat(200))).toBe(`${"漢".repeat(60)}…`);
});

// Design 16.7 step 3 (intents review r1). Defect: 60 clusters of many code
// points each make a quote longer than parseMessageLink keeps, so the link
// is dropped on the next load.
it("keeps a quote within 200 code points when its clusters are long", () => {
  const letter = `e${"\u0301".repeat(20)}`;
  const quote = excerpt(letter.repeat(60));
  expect([...quote].length).toBeLessThanOrEqual(200);
  expect(quote).toBe(`${letter.repeat(9)}…`);
  // 50 clusters of 4 code points fill 200 exactly, so the `…` must fit too.
  expect([...excerpt(`a\u0301\u0302\u0303`.repeat(60))].length).toBe(197);
});

// Design 16.7 step 4 (intents review r1). Defect: the stored quote shows
// backslashes to the user.
it("returns the excerpt as typed, quotes and backslashes included", () => {
  expect(excerpt('Fix the "login" bug in C:\\auth')).toBe(
    'Fix the "login" bug in C:\\auth',
  );
});

// Design 16.7 format table and 16.12.5. Defect: any change to the line the
// rules text teaches the agents to read.
it("writes the exact line for each intent, with and without a ref", () => {
  const quote = "first words…";
  expect(intentLine({ intent: "steer", quote }, "#3")).toBe(
    '[steer → #3 · "first words…"]',
  );
  expect(intentLine({ intent: "steer", quote })).toBe(
    '[steer → "first words…"]',
  );
  expect(intentLine({ intent: "reply", quote }, "m_abcdefgh2345")).toBe(
    '[reply → m_abcdefgh2345 · "first words…"]',
  );
  expect(intentLine({ intent: "reply", quote })).toBe(
    '[reply → "first words…"]',
  );
  expect(intentLine({ intent: "continue", quote }, "#3")).toBe(
    '[re #3 · "first words…"]',
  );
  expect(intentLine({ intent: "continue", quote })).toBe(
    '[re "first words…"]',
  );
  expect(intentLine({ intent: "steer", scope: "turn" }, "#3")).toBe(
    "[steer → whole turn]",
  );
});

// Design 16.7 step 4. Defect: a quote in the target ends the excerpt early,
// or an escape the line adds reads as part of the text.
it("escapes backslashes, then quotes, in the line", () => {
  expect(
    intentLine({ intent: "reply", quote: excerpt('say "hi" to C:\\x\\"y') }),
  ).toBe('[reply → "say \\"hi\\" to C:\\\\x\\\\\\"y"]');
});

// Design 16.7. Defect: a target with no text gives an empty pair of quotes.
it("shows a target with no text as (no text)", () => {
  expect(intentLine({ intent: "steer", quote: excerpt(" \n ") }, "#1")).toBe(
    "[steer → #1 · (no text)]",
  );
});

// Design 16.7 ref table. Defect: a handle of another session or turn names
// another message there, since handles restart in each turn.
it("names a target by its handle only in the reader's own session and turn", () => {
  const target: ReplyTarget = {
    messageId: "m_abcdefgh2345" as MessageId,
    body: "Review PR 12",
    handledBySession: "a-chat" as SessionId,
    slot: { turnId: "turn-1" as TurnId, handle: "#2" as Handle },
  };
  const reader = (sessionId: string, turnId: string) => ({
    sessionId: sessionId as SessionId,
    turnId: turnId as TurnId,
  });
  expect(targetRef(target, reader("a-chat", "turn-1"))).toBe("#2");
  expect(targetRef(target, reader("a-chat", "turn-2"))).toBe("m_abcdefgh2345");
  expect(targetRef(target, reader("giga-chat", "turn-1"))).toBe(
    "m_abcdefgh2345",
  );
  expect(
    targetRef({ ...target, slot: null }, reader("a-chat", "turn-1")),
  ).toBe("m_abcdefgh2345");
});
