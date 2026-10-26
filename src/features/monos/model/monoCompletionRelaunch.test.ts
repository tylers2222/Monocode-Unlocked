import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlOutcome } from "../../orchestration/model/orchestration";
import { INTERRUPT_MESSAGE } from "../../sessions/model/inFlight";
import { newSession, type Session } from "../../sessions/model/session";
import type { TurnId } from "./monoMessages";
import {
  MONO_COMPLETION_HOLD_MS,
  MonoSessionCompletionBatches,
  savedMonoCompletions,
  type SavedMonoCompletion,
} from "./monoSessionCompletion";

// ADE-46: what a quit keeps of the Mono's waiting completion groups, and how
// the relaunch hands each kept result to the Mono once.

const S = "s-872c4aa7";
const request = (requestId: string, sessionId = S) => ({
  requestId,
  sessionId,
  project: "/code/monocode",
  prompt: `Request ${requestId}`,
});
const end = (text: string): ControlOutcome => ({ status: "completed", text });

function batchesOver(sessions: Map<string, Session> = new Map()) {
  const ready = vi.fn();
  const batches = new MonoSessionCompletionBatches(
    ready,
    (run, ms) => setTimeout(run, ms),
    (id) => sessions.get(id),
    { hold() {}, dismiss() {} },
  );
  const reported = () =>
    ready.mock.calls.map(([, message]) =>
      [...message.text.matchAll(/"sessionId":"([^"]+)"/g)].map(([, id]) => id),
    );
  return { batches, ready, reported };
}

describe("Mono completions across a relaunch", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("a quit keeps each result the Mono was not given, once per end, and only accepted requests", () => {
    const { batches, reported } = batchesOver();
    const first = { monoId: "mono", turn: 1 };
    const second = { monoId: "mono", turn: 2 };
    const told = end("ADE-58 paused");
    batches.watch(first, request("send", S))(told);
    batches.closeInactive(({ turn }) => turn === 2);
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
    expect(reported()).toEqual([[S]]);

    // The later turn's steer joined the turn already reported; its new
    // request still runs; two requests share one held end; one launch was
    // never accepted.
    batches.watch(second, request("steer", S))(told);
    batches.watch(second, request("running", "s-7a1eab69"));
    const held = end("M-10 wording fixed");
    batches.watch(second, request("held", "s-held"))(held);
    batches.watch(second, request("held-steer", "s-held"))(held);
    batches.watch(second, request("refused", "s-refused"), {
      awaitAcceptance: true,
    });

    expect(
      batches
        .undelivered()
        .map(({ origin, request, result }) => [
          origin.turn,
          request.requestId,
          result?.result,
        ]),
    ).toEqual([
      [2, "running", undefined],
      [2, "held", "M-10 wording fixed"],
    ]);
  });

  // ADE-60: the relaunch asks for 872c4aa7's Continue twice; the first opens
  // turn t1, the second is refused on the busy session.
  it("only the Continue turn that opened settles a restored result, not a refused copy", () => {
    const session: Session = {
      ...newSession("omp", "/code/monocode"),
      id: S,
      blocks: [
        {
          id: "work",
          role: "user",
          text: "Fix the M-10 wording",
          startedAt: 1,
        },
        {
          id: "quit",
          role: "system",
          text: INTERRUPT_MESSAGE,
          notice: "interrupt",
        },
      ],
    };
    const sessions = new Map([[S, session]]);
    const { batches, ready } = batchesOver(sessions);
    batches.restore(
      [{ origin: { monoId: "mono", turn: 7 }, request: request("3e217e0f") }],
      () => true,
    );
    const opened = batches.resume(S, "t1" as TurnId);
    const refused = batches.resume(S, "t2" as TurnId);
    sessions.set(S, {
      ...session,
      blocks: [
        ...session.blocks,
        {
          id: "continue",
          role: "user",
          text: "Continue",
          turnId: "t1" as TurnId,
          startedAt: 2,
        },
      ],
    });
    refused({
      status: "failed",
      text: "",
      error: "The interrupted turn could not continue.",
    });
    batches.closeInactive(() => false);
    expect(ready).not.toHaveBeenCalled();

    opened(end("M-10 #117 wording fix done."));
    expect(ready).toHaveBeenCalledOnce();
    const [monoId, message] = ready.mock.calls[0];
    expect(monoId).toBe("mono");
    expect(message.monoSessionCompletion).toMatchObject({
      sessionId: S,
      status: "completed",
    });
    expect(message.text).toContain("M-10 #117 wording fix done.");
    expect(batches.undelivered()).toEqual([]);
  });

  it("a session the relaunch cannot continue is reported once as interrupted", () => {
    const { batches, reported, ready } = batchesOver();
    batches.restore(
      [
        {
          origin: { monoId: "mono", turn: 3 },
          request: request("a", "s-offline"),
        },
        {
          origin: { monoId: "mono", turn: 3 },
          request: request("b", "s-offline"),
        },
      ],
      () => false,
    );
    batches.closeInactive(() => false);
    batches.closeInactive(() => false);
    expect(reported()).toEqual([["s-offline"]]);
    expect(ready.mock.calls[0][1].text).toContain(INTERRUPT_MESSAGE);
    expect(batches.undelivered()).toEqual([]);
  });

  it("a second quit keeps what the first kept and the relaunch has not sent", () => {
    const { batches } = batchesOver();
    const result = {
      sessionId: "s-held",
      project: "/code/monocode",
      title: "Held",
      status: "completed" as const,
      originalPrompt: "Request held",
      result: "done",
      truncated: false,
    };
    const saved: SavedMonoCompletion[] = [
      { origin: { monoId: "mono", turn: 4 }, request: request("running") },
      {
        origin: { monoId: "gone", turn: 1 },
        request: request("held", "s-held"),
        result,
      },
    ];
    batches.restore(saved, () => true);
    expect(batches.undelivered()).toEqual(saved);
  });

  it("reads the claimed results and drops malformed ones", () => {
    const good: SavedMonoCompletion = {
      origin: { monoId: "mono", turn: 1 },
      request: request("a"),
    };
    expect(
      savedMonoCompletions([
        good,
        { origin: { monoId: "mono" }, request: request("no-turn") },
        { origin: { monoId: "mono", turn: 1 }, request: { requestId: "x" } },
        { ...good, result: { sessionId: "a" } },
        null,
      ]),
    ).toEqual([good]);
    expect(savedMonoCompletions(null)).toEqual([]);
    expect(savedMonoCompletions({ monoCompletions: [good] })).toEqual([]);
  });

  // ADE-89, for gate/mutation only (one-off unit-test exception, 2026-10-24):
  // the store rows a kill leaves are what the window owes, and each released
  // result names its notice, which the Mono chat's save settles in Rust.
  describe("the store rows a kill leaves", () => {
    const origin = { monoId: "mono", turn: 1 };
    function batchesWithStore(
      ready: ConstructorParameters<typeof MonoSessionCompletionBatches>[0],
    ) {
      const store = { hold: vi.fn(), dismiss: vi.fn() };
      const batches = new MonoSessionCompletionBatches(
        ready,
        (run, ms) => setTimeout(run, ms),
        () => undefined,
        store,
      );
      return { batches, store };
    }
    const owed = (id: string, text: string, notice: string) => ({
      origin,
      request: request(id, `s-${id}`),
      result: expect.objectContaining({ sessionId: `s-${id}`, result: text }),
      notice,
    });

    it("hold each result in the notice named by its group's first request id before it is queued", async () => {
      let queued!: () => void;
      let heldAtReady: unknown;
      const ready = vi.fn<
        ConstructorParameters<typeof MonoSessionCompletionBatches>[0]
      >(() => {
        heldAtReady = store.hold.mock.lastCall;
        return new Promise<void>((resolve) => (queued = resolve));
      });
      const { batches, store } = batchesWithStore(ready);
      const b = batches.watch(origin, request("b", "s-b"));
      const a = batches.watch(origin, request("a", "s-a"));
      expect(store.hold).toHaveBeenLastCalledWith([
        { origin, request: request("b", "s-b") },
        { origin, request: request("a", "s-a") },
      ]);
      b(end("B"));
      a(end("A"));
      batches.closeInactive(() => false);
      const noticed = [
        owed("b", "B", "mono-completion-batch-a"),
        owed("a", "A", "mono-completion-batch-a"),
      ];
      expect(heldAtReady).toEqual([noticed]);
      expect(ready.mock.calls[0][1].id).toBe("mono-completion-batch-a");
      queued();
      await vi.runAllTimersAsync();
      batches.watch(origin, request("c", "s-c"));
      expect(store.hold).toHaveBeenLastCalledWith([
        { origin, request: request("c", "s-c") },
      ]);
    });

    it("a lone result is in the notice named by its own id", () => {
      const ready = vi.fn();
      const { batches } = batchesWithStore(ready);
      batches.watch(origin, request("c", "s-c"))(end("C"));
      batches.closeInactive(() => false);
      expect(ready.mock.calls[0][1].id).toBe("mono-completion-c");
    });

    it("a render that changes nothing owed writes nothing", () => {
      const { batches, store } = batchesWithStore(vi.fn());
      batches.watch(origin, request("c", "s-c"));
      for (let render = 0; render < 3; render++)
        batches.closeInactive(() => true);
      expect(store.hold).toHaveBeenCalledOnce();
    });

    // Review r2 M1: the store deletes a stopped session's rows, also once its
    // notice is queued and the window forgot it, and no later hold writes
    // them back.
    it("a session the Mono stops keeps no row, before or after its notice is queued", async () => {
      let queued!: () => void;
      const { batches, store } = batchesWithStore(
        () => new Promise<void>((resolve) => (queued = resolve)),
      );
      batches.watch(origin, request("a", "s-a"))(end("A"));
      batches.watch(origin, request("b", "s-b"))(end("B"));
      batches.closeInactive(() => false);
      const held = store.hold.mock.calls.length;
      batches.dismissSession("mono", "s-a");
      expect(store.dismiss).toHaveBeenLastCalledWith("mono", "s-a");
      queued();
      await vi.runAllTimersAsync();
      batches.dismissSession("mono", "s-b");
      expect(store.dismiss).toHaveBeenLastCalledWith("mono", "s-b");
      expect(store.hold.mock.calls.slice(held).flat(2)).toEqual([]);
    });
  });
});
