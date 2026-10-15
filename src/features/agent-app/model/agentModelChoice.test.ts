// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  resetHarnessModelOverlays,
  saveLastModelChoice,
  setHarnessModels,
} from "../../sessions/model/models";
import { requireHarnessModel } from "./agentModelChoice";

vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex" || id === "omp",
}));

beforeEach(() => {
  localStorage.clear();
  setHarnessModels("codex", [
    { id: "codex:test", harness: "codex", name: "Test model" },
    { id: "codex:other", harness: "codex", name: "Other model" },
  ]);
});

afterEach(() => {
  resetHarnessModelOverlays();
});

describe("requireHarnessModel", () => {
  it.each([
    [{}],
    [{ harness: "codex" }],
    [{ model: "codex:test" }],
    [{ harness: null, model: "codex:test" }],
    [{ harness: "codex", model: null }],
  ])("requires both harness and model: %j", (input) => {
    expect(() => requireHarnessModel(input)).toThrow(
      "harness and model are required; run models.list for exact IDs",
    );
  });

  it.each([
    [{ harness: "nope", model: "codex:test" }, "Unknown harness"],
    [{ harness: 7, model: "codex:test" }, "Unknown harness"],
    [{ harness: "claude", model: "claude:opus-5" }, "claude is not available"],
    [{ harness: "codex", model: "codex:ghost" }, "Unknown model for codex"],
    [{ harness: "codex", model: 7 }, "Unknown model for codex"],
    [{ harness: "omp", model: "codex:test" }, "Unknown model for omp"],
  ])("rejects %j and names models.list", (input, error) => {
    expect(() => requireHarnessModel(input)).toThrow(error);
    expect(() => requireHarnessModel(input)).toThrow("run models.list");
  });

  it("returns exactly the named pair, whatever the composer last used", () => {
    saveLastModelChoice("codex", "codex:other");
    expect(
      requireHarnessModel({ harness: "codex", model: "codex:test" }),
    ).toEqual({
      harness: "codex",
      model: expect.objectContaining({ id: "codex:test", harness: "codex" }),
    });
  });
});
