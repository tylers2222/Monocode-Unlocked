import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { claimMonoCompletions, monoCompletionStore } from "./monoCompletionStore";

// ADE-89, for gate/mutation only (one-off unit-test exception, 2026-10-24).
// The real store is proven by scenario::completions and `app.sh ade89`.

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const owed = {
  origin: { monoId: "mono", turn: 1 },
  request: {
    requestId: "a",
    sessionId: "s-a",
    project: "/code/project",
    prompt: "Review a",
  },
};

describe("the Mono completion store commands", () => {
  it("reach Rust in call order, and a failed write does not stop the writes after it", async () => {
    let fail!: (error: Error) => void;
    vi.mocked(invoke)
      .mockImplementationOnce(() => new Promise((_, reject) => (fail = reject)))
      .mockResolvedValue(undefined);
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    monoCompletionStore.hold([owed]);
    monoCompletionStore.dismiss("mono", "s-a");
    monoCompletionStore.hold([]);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledOnce());
    expect(invoke).toHaveBeenCalledWith("mono_completions_hold", {
      saved: [owed],
    });
    fail(new Error("store busy"));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
    expect(vi.mocked(invoke).mock.calls.slice(1)).toEqual([
      ["mono_completions_dismiss", { monoId: "mono", sessionId: "s-a" }],
      ["mono_completions_hold", { saved: [] }],
    ]);
    expect(logged).toHaveBeenCalledWith(new Error("store busy"));
  });

  it("claim the rows no live window holds and drop malformed ones", async () => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "mono_completions_claim" ? [owed, { origin: {} }] : undefined,
    );
    await expect(claimMonoCompletions()).resolves.toEqual([owed]);
  });
});
