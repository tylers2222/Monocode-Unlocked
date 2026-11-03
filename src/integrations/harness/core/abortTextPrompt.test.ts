import { describe, expect, it, vi } from "vitest";
import { abortTextPromptRace, btwTextCancelled } from "./abortTextPrompt";
import { textTurns } from "./acp";

describe("abortTextPromptRace", () => {
  it("rejects with a BTW cancellation error and runs the abort hook", async () => {
    const controller = new AbortController();
    const onAbort = vi.fn();
    const race = abortTextPromptRace(controller.signal, onAbort);
    controller.abort();
    await expect(race.promise).rejects.toEqual(btwTextCancelled());
    expect(onAbort).toHaveBeenCalledOnce();
    race.detach();
  });
});

// ADE-192: the turn queue the text adapters share runs their calls one at a
// time, in call order: a prompt waits for the warm-up before it.
describe("textTurns", () => {
  it("runs a text adapter's calls one at a time, in call order", async () => {
    const order: string[] = [];
    let release: (() => void) | undefined;
    const turns = textTurns(
      async (cwd) => {
        order.push(`warm ${cwd}`);
        await new Promise<void>((resolve) => (release = resolve));
      },
      async (input) => {
        order.push(`prompt ${input.prompt}`);
        return input.prompt;
      },
    );
    const warm = turns.warmup("/repo");
    const answer = turns.run({ cwd: "/repo", prompt: "q" });
    await vi.waitFor(() => expect(release).toBeDefined());
    expect(order).toEqual(["warm /repo"]);
    release?.();
    await expect(answer).resolves.toBe("q");
    await warm;
    expect(order).toEqual(["warm /repo", "prompt q"]);
  });
});
