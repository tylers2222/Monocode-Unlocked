import { expect, test } from "vitest";
import { throwawayIsPositive } from "./sessionStore";

test("throwawayIsPositive", () => {
  expect(throwawayIsPositive(1)).toBe(true);
  expect(throwawayIsPositive(0)).toBe(false);
  expect(throwawayIsPositive(-1)).toBe(false);
});
