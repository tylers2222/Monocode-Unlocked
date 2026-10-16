import { expect, it } from "vitest";
import {
  type Intent,
  parseHandle,
  parseMessageId,
  parseMonoId,
  parseReplyInput,
  parseRowIntent,
  parseSessionId,
} from "./monoMessages";

it("accepts a message id of m_ and 12 base32 characters, nothing near it", () => {
  expect(parseMessageId("m_abcdefgh2345")).toBe("m_abcdefgh2345");
  for (const nearMiss of [
    "m_ABCDEFGH2345",
    "m_abcdefgh2341",
    "m_abcdefgh234",
    "m_abcdefgh23456",
    "x_abcdefgh2345",
    " m_abcdefgh2345",
    42,
  ])
    expect(parseMessageId(nearMiss)).toBeUndefined();
});

it("accepts handles #1 to #99 only", () => {
  expect(parseHandle("#1")).toBe("#1");
  expect(parseHandle("#99")).toBe("#99");
  for (const nearMiss of ["#0", "#100", "#01", "1", "#", "#1 ", 1])
    expect(parseHandle(nearMiss)).toBeUndefined();
});

it("accepts Mono ids by the roster's rule", () => {
  expect(parseMonoId("6f1c-Ab09")).toBe("6f1c-Ab09");
  for (const nearMiss of ["", "a_b", "a/b", "a".repeat(65), null])
    expect(parseMonoId(nearMiss)).toBeUndefined();
});

it("accepts session ids by the session store's rule", () => {
  expect(parseSessionId("app-chat_1")).toBe("app-chat_1");
  for (const nearMiss of ["", "chat#1", "chat 1", "../chat", undefined])
    expect(parseSessionId(nearMiss)).toBeUndefined();
});

// Design 3.3. Defect: Rust gets a `to` it refuses or reads differently, such
// as a repeated handle, more than nine, or none.
it("accepts a reply's to as one handle or 1 to 9 different ones, always as a list", () => {
  const reply = (to?: unknown) => parseReplyInput({ message: "Done", to });
  expect(reply()).toEqual({ message: "Done", wanting_reply: false });
  expect(reply("#2").to).toEqual(["#2"]);
  const nine = Array.from({ length: 9 }, (_, index) => `#${index + 1}`);
  expect(reply(nine).to).toEqual(nine);
  for (const nearMiss of [
    null,
    [],
    ["#1", "#1"],
    [...nine, "#10"],
    "#0",
    ["#1", 2],
    1,
  ])
    expect(() => reply(nearMiss)).toThrow("to must be");
});

// Design 16.1. Defect: the web app trusts a row that is new and names a
// target, or an accepted continue, which the gate never lets through; or an
// accepted row's result has no `Intent` type without a cast.
it("reads an accepted row's intent only when it and replyTo agree", () => {
  const target = "m_abcdefgh2345";
  const row = (status: string, intent: unknown, replyTo: unknown) =>
    parseRowIntent({ status, intent, replyTo });
  const accepted = (status: string, intent: unknown, replyTo: unknown) => {
    const parsed = row(status, intent, replyTo);
    const read: Intent | undefined =
      parsed?.status === "accepted" ? parsed.intent : undefined;
    return read;
  };
  expect(accepted("delivered", "new", null)).toEqual({
    intent: "new",
    replyTo: null,
  });
  expect(accepted("open", "steer", target)).toEqual({
    intent: "steer",
    replyTo: target,
  });
  expect(accepted("answered", "reply", target)).toEqual({
    intent: "reply",
    replyTo: target,
  });
  for (const [intent, replyTo] of [
    ["new", target],
    ["continue", target],
    ["steer", null],
    ["reply", null],
    ["queue", null],
    ["steer", "m_NOTANID0000"],
  ])
    expect(row("delivered", intent, replyTo)).toBeUndefined();
  expect(row("rejected", "continue", null)).toEqual({
    status: "rejected",
    asked: { intent: "continue", replyTo: null },
  });
  expect(row("rejected", "steer", null)).toEqual({
    status: "rejected",
    asked: { intent: "steer", replyTo: null },
  });
});
