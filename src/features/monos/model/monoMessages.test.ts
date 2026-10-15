import { expect, it } from "vitest";
import {
  parseHandle,
  parseMessageId,
  parseMonoId,
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
