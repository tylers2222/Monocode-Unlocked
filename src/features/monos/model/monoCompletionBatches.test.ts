import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TurnId } from "./monoMessages";
import {
  createTurnWatchers,
  deliverAppSteer,
} from "../../../app/model/appSessionSend";
import { submitWithSettlement } from "../../../app/model/managedSubmission";
import type { ControlOutcome } from "../../orchestration/model/orchestration";
import {
  canDispatchQueuedHead,
  steerableQueuedMessage,
} from "../../sessions/model/messageQueue";
import {
  newSession,
  type QueuedMessage,
  type Session,
} from "../../sessions/model/session";
import {
  enqueueMonoSessionCompletion,
  MONO_COMPLETION_HOLD_MS,
  MonoSessionCompletionBatches,
} from "./monoSessionCompletion";

const origin = { monoId: "mono", turn: 1 };
const schedule = (run: () => void, ms: number) => setTimeout(run, ms);
/** Batches as App.tsx builds them, with no open session to read replies from. */
function batchesOf(
  ready: ConstructorParameters<typeof MonoSessionCompletionBatches>[0],
  later: (run: () => void, ms: number) => void = schedule,
) {
  return new MonoSessionCompletionBatches(ready, later, () => undefined, {
    save() {},
    deliver() {},
    remove() {},
  });
}
function request(requestId: string, sessionId = requestId) {
  return {
    requestId,
    sessionId,
    project: "/code/project",
    prompt: `Review ${sessionId}`,
  };
}
/** A turn end. Each call is a new end, as each turn's `finally` builds one. */
function ended(
  id: string,
  status: "completed" | "failed" | "cancelled" = "completed",
): ControlOutcome {
  return {
    status,
    text: `Result ${id}`,
    ...(status === "failed" ? { error: "Launch failed" } : {}),
  };
}

describe("Mono completion batches", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("waits for the launching turn, then sends results that stop within the hold together", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const a = batches.watch(origin, request("a"));
    a(ended("a"));
    batches.closeInactive(() => true);
    expect(ready).not.toHaveBeenCalled();
    const b = batches.watch(origin, request("b"));
    const c = batches.watch(origin, request("c"));
    c(ended("c"));
    batches.closeInactive(() => false);
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS - 1);
    expect(ready).not.toHaveBeenCalled();
    b(ended("b"));
    expect(ready).toHaveBeenCalledOnce();
    const [monoId, message] = ready.mock.calls[0];
    expect(monoId).toBe("mono");
    expect(message.monoSessionCompletion).toMatchObject({
      sessionCount: 3,
      status: "completed",
    });
    const payload = JSON.parse(message.text.split("\n\n")[1]);
    expect(
      payload.sessions.map((entry: { sessionId: string }) => entry.sessionId),
    ).toEqual(["a", "b", "c"]);
    expect(message.text).toContain("one consolidated update");
    for (const id of ["a", "b", "c"])
      expect(message.text).toContain(`Result ${id}`);
    a(ended("a"));
    b(ended("b"));
    batches.closeInactive(() => false);
    vi.runAllTimers();
    expect(ready).toHaveBeenCalledOnce();
  });

  it("sends failures and cancellations when the hold ends, then a later result alone", async () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const failed = batches.watch(origin, request("failed"));
    const cancelled = batches.watch(origin, request("cancelled"));
    const succeeded = batches.watch(origin, request("success"));
    batches.closeInactive(() => false);
    await submitWithSettlement({
      submit: () => false,
      onSettled: (outcome) => failed(outcome),
      rejectionMessage: "Launch failed",
    });
    cancelled(ended("cancelled", "cancelled"));
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS - 1);
    expect(ready).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(ready).toHaveBeenCalledOnce();
    const message = ready.mock.calls[0][1];
    expect(message.monoSessionCompletion).toMatchObject({
      status: "failed",
      sessionCount: 2,
    });
    expect(message.text).toContain("Launch failed");
    expect(message.text).toContain('"status":"cancelled"');
    expect(message.text).not.toContain('"sessionId":"success"');
    succeeded(ended("success"));
    expect(ready).toHaveBeenCalledTimes(2);
    expect(ready.mock.calls[1][1].monoSessionCompletion).toEqual({
      sessionId: "success",
      title: "Agent session",
      status: "completed",
    });
  });

  it("keeps later requests and other Monos independent of a slow earlier group", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const slow = batches.watch(origin, request("slow"));
    batches.watch(origin, request("also-slow"));
    const later = batches.watch({ ...origin, turn: 2 }, request("later"));
    const other = batches.watch(
      { monoId: "another", turn: 1 },
      request("other"),
    );
    batches.closeInactive(() => false);
    later(ended("later"));
    other(ended("other"));
    expect(ready.mock.calls.map(([monoId]) => monoId)).toEqual([
      "mono",
      "another",
    ]);
    slow(ended("slow"));
    expect(ready).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
    expect(ready).toHaveBeenCalledTimes(3);
    expect(ready.mock.calls[2][1].monoSessionCompletion.sessionId).toBe("slow");
  });

  it("queues one consolidated report behind chat and never steers a busy Mono", () => {
    let mono: Session = {
      ...newSession("codex", "/code/project"),
      busy: true,
      turnReadiness: "ready",
      queuedMessages: [
        { id: "chat", text: "Another question", attachments: [] },
      ],
    };
    const batches = batchesOf((_, message) => {
      mono = enqueueMonoSessionCompletion(mono, message);
    });
    const a = batches.watch(origin, request("a"));
    const b = batches.watch(origin, request("b"));
    batches.closeInactive(() => false);
    b(ended("b"));
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS - 1);
    expect(mono.queuedMessages).toHaveLength(1);
    a(ended("a"));
    expect(mono.queuedMessages).toHaveLength(2);
    expect(mono.queuedMessages![1].monoSessionCompletion?.sessionCount).toBe(2);
    const notification = {
      ...mono,
      queuedMessages: mono.queuedMessages!.slice(1),
    };
    expect(canDispatchQueuedHead(notification)).toBe(false);
    expect(
      steerableQueuedMessage(notification, "turn-1" as TurnId),
    ).toBeUndefined();
    expect(canDispatchQueuedHead({ ...notification, busy: false })).toBe(true);
  });

  it("keeps single-session reports compatible", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const done = batches.watch(origin, request("request", "worker"));
    done(ended("worker"));
    expect(ready).not.toHaveBeenCalled();
    batches.closeInactive(() => false);
    expect(ready.mock.calls[0][1]).toMatchObject({
      id: "mono-completion-request",
      monoSessionCompletion: {
        sessionId: "worker",
        title: "Agent session",
        status: "completed",
      },
    });
    expect(ready.mock.calls[0][1].monoSessionCompletion).not.toHaveProperty(
      "sessionCount",
    );
  });

  it("waits for a retry of a rejected launch and ignores its old settlement callback", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const rejected = batches.watch(origin, request("request"));
    rejected(ended("worker", "failed"));
    const retry = batches.watch(origin, request("request"));
    batches.closeInactive(() => false);
    rejected(ended("worker", "failed"));
    expect(ready).not.toHaveBeenCalled();
    retry(ended("worker"));
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion.status).toBe(
      "completed",
    );
  });

  it("dismisses the calling Mono's pending cancellation without losing other session results", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const stopped = batches.watch(
      origin,
      request("stop-request", "stopped-session"),
    );
    const other = batches.watch(
      origin,
      request("other-request", "other-session"),
    );
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "stopped-session");
    stopped(ended("stopped-session", "cancelled"));
    expect(ready).not.toHaveBeenCalled();
    other(ended("other-session"));
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion).toEqual({
      sessionId: "other-session",
      title: "Agent session",
      status: "completed",
    });
    expect(ready.mock.calls[0][1].text).not.toContain("stopped-session");
  });

  it("keeps other Monos and future monitored turns on the same session independent", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const cancelled = batches.watch(origin, request("request", "worker"));
    const other = batches.watch(
      { monoId: "another", turn: 1 },
      request("other", "worker"),
    );
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "worker");
    cancelled(ended("worker", "cancelled"));
    other(ended("worker", "cancelled"));
    expect(ready.mock.calls.map(([monoId]) => monoId)).toEqual(["another"]);

    const followUp = batches.watch(
      { ...origin, turn: 2 },
      request("follow-up", "worker"),
    );
    batches.closeInactive(() => false);
    cancelled(ended("worker", "cancelled"));
    expect(ready).toHaveBeenCalledOnce();
    followUp(ended("worker"));
    expect(ready.mock.calls.map(([monoId]) => monoId)).toEqual([
      "another",
      "mono",
    ]);
    expect(ready.mock.calls[1][1].monoSessionCompletion.status).toBe(
      "completed",
    );
  });

  it("does not repeat a rejected launch after the successfully launched session is archived", async () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const rejected = batches.watch(
      origin,
      request("rejected-request", "failed-session"),
    );
    const accepted = await submitWithSettlement({
      submit: () => false,
      onSettled: (outcome) => rejected(outcome),
      rejectionMessage: "Session was not open",
    });
    expect(accepted).toBe(false);
    rejected.discard();
    const launched = batches.watch(
      origin,
      request("launched-request", "worker"),
    );
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "worker");
    launched(ended("worker", "cancelled"));
    expect(ready).not.toHaveBeenCalled();
  });

  it.each([false, true])(
    "does not deliver a rejected submission when the Mono becomes idle before acceptance (throws: %s)",
    async (throws) => {
      const ready = vi.fn();
      const batches = batchesOf(ready);
      const rejected = batches.watch(origin, request("request", "worker"), {
        awaitAcceptance: true,
      });
      let finish!: (accepted: boolean) => void;
      let fail!: (error: Error) => void;
      const acceptance = new Promise<boolean>((resolve, reject) => {
        finish = resolve;
        fail = reject;
      });
      const submission = submitWithSettlement({
        submit: () => acceptance,
        onSettled: (outcome) => rejected(outcome),
        rejectionMessage: "Could not start",
      });
      batches.closeInactive(() => false);
      if (throws) fail(new Error("Session was not open"));
      else finish(false);
      expect(await submission).toBe(false);
      // No delivery may start in the gap before the caller handles rejection.
      expect(ready).not.toHaveBeenCalled();
      rejected.discard();
      rejected.accept();
      rejected(ended("worker", "failed"));
      batches.closeInactive(() => false);
      expect(ready).not.toHaveBeenCalled();
    },
  );

  it.each(["completed", "failed", "cancelled"] as const)(
    "keeps an early %s event from an accepted submission and releases it after acceptance",
    (status) => {
      const ready = vi.fn();
      const batches = batchesOf(ready);
      const watched = batches.watch(origin, request("request", "worker"), {
        awaitAcceptance: true,
      });
      batches.closeInactive(() => false);
      watched(ended("worker", status));
      expect(ready).not.toHaveBeenCalled();
      watched.accept();
      expect(ready).toHaveBeenCalledOnce();
      expect(ready.mock.calls[0][1].monoSessionCompletion.status).toBe(status);
      watched.accept();
      expect(ready).toHaveBeenCalledOnce();
    },
  );

  it("releases an accepted sibling when an undecided submission is rejected", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const rejected = batches.watch(
      origin,
      request("rejected-request", "rejected"),
      {
        awaitAcceptance: true,
      },
    );
    batches.watch(
      origin,
      request("accepted-request", "accepted"),
    )(ended("accepted"));
    batches.closeInactive(() => false);
    rejected(ended("rejected", "failed"));
    expect(ready).not.toHaveBeenCalled();
    rejected.discard();
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion.sessionId).toBe(
      "accepted",
    );
    expect(ready.mock.calls[0][1].text).not.toContain('"sessionId":"rejected"');
  });

  it("sends an accepted sibling when the hold ends, then an undecided submission once accepted", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const undecided = batches.watch(
      origin,
      request("undecided-request", "undecided"),
      {
        awaitAcceptance: true,
      },
    );
    batches.watch(
      origin,
      request("accepted-request", "accepted"),
    )(ended("accepted"));
    batches.closeInactive(() => false);
    undecided(ended("undecided", "failed"));
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion.sessionId).toBe(
      "accepted",
    );
    undecided.accept();
    expect(ready).toHaveBeenCalledTimes(2);
    expect(ready.mock.calls[1][1].monoSessionCompletion).toMatchObject({
      sessionId: "undecided",
      status: "failed",
    });
  });

  it("lets a discarded request retry while ignoring the old callback and discard", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    const rejected = batches.watch(origin, request("request", "worker"));
    rejected.discard();
    const retry = batches.watch(origin, request("request", "worker"));
    rejected(ended("worker", "failed"));
    rejected.discard();
    batches.closeInactive(() => false);
    expect(ready).not.toHaveBeenCalled();
    retry(ended("worker"));
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion.status).toBe(
      "completed",
    );
  });

  it.each([false, true])(
    "updates a report dismissed during asynchronous delivery (retain another session: %s)",
    async (retainOther) => {
      let resume!: () => void;
      const waiting = new Promise<void>((resolve) => {
        resume = resolve;
      });
      const delivered = vi.fn();
      const batches = batchesOf(async (_, _message, current) => {
        await waiting;
        const message = current();
        if (message) delivered(message);
      });
      batches.watch(
        origin,
        request("request", "worker"),
      )(ended("worker", "cancelled"));
      if (retainOther)
        batches.watch(origin, request("other", "other"))(ended("other"));
      batches.closeInactive(() => false);
      batches.dismissSession(origin.monoId, "worker");
      resume();
      await waiting;
      if (retainOther) {
        expect(delivered).toHaveBeenCalledOnce();
        expect(delivered.mock.calls[0][0].monoSessionCompletion).toEqual({
          sessionId: "other",
          title: "Agent session",
          status: "completed",
        });
        expect(delivered.mock.calls[0][0].text).not.toContain(
          '"sessionId":"worker"',
        );
      } else {
        expect(delivered).not.toHaveBeenCalled();
      }
    },
  );

  it("bounds a large group's reports while retaining every session's identity and outcome", () => {
    const ready = vi.fn();
    const batches = batchesOf(ready);
    for (let index = 0; index < 40; index++) {
      const id = `worker-${index}`;
      batches.watch(origin, {
        ...request(id),
        prompt: "y".repeat(12_000),
      })({ ...ended(id), text: "x".repeat(12_000) });
    }
    batches.closeInactive(() => false);
    const message = ready.mock.calls[0][1];
    const payload = JSON.parse(message.text.split("\n\n")[1]);
    expect(payload.sessions).toHaveLength(40);
    expect(
      payload.sessions.every(
        (entry: { truncated: boolean }) => entry.truncated,
      ),
    ).toBe(true);
    expect(message.text.length).toBeLessThan(60_000);
  });

  describe("a session that a start and a later steer both watch (ADE-47)", () => {
    function reportedSessions(message: QueuedMessage): string[] {
      const payload = JSON.parse(message.text.split("\n\n")[1]);
      return (payload.sessions ?? [payload]).map(
        (entry: { sessionId: string }) => entry.sessionId,
      );
    }

    /** One Mono request, watched as `monitorCompletion` in App.tsx does. */
    function monitor(
      batches: MonoSessionCompletionBatches,
      mono: { monoId: string; turn: number },
      requestId: string,
      sessionId: string,
    ) {
      return batches.watch(mono, request(requestId, sessionId), {
        awaitAcceptance: true,
      });
    }

    /** `sessions.send` steers `s` into its running turn 1. */
    function steer(
      batches: MonoSessionCompletionBatches,
      turnWatchers: ReturnType<typeof createTurnWatchers>,
      mono: { monoId: string; turn: number },
      requestId: string,
      delivered: () => Promise<void> = async () => undefined,
    ) {
      return deliverAppSteer({
        watchers: turnWatchers,
        sessionId: "s",
        gen: 1,
        watcher: monitor(batches, mono, requestId, "s"),
        record: () => undefined,
        steer: delivered,
        forget: () => undefined,
      });
    }

    /**
     * Mono turn 1 starts `s` with `sessions.start`. Mono turn 2 starts `x`
     * and steers `s` with `sessions.send` while turn 1's request still
     * watches it, so `s`'s turn 1 has two watchers.
     */
    async function startThenSteer(
      batches: MonoSessionCompletionBatches,
      turnWatchers: ReturnType<typeof createTurnWatchers>,
    ) {
      const start = monitor(batches, origin, "s", "s");
      start.accept();
      batches.closeInactive(() => false);
      vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
      const x = monitor(batches, { ...origin, turn: 2 }, "x", "x");
      x.accept();
      await steer(batches, turnWatchers, { ...origin, turn: 2 }, "send-s");
      // How `submitSession` settles the end of `s`'s turn 1.
      const endTurn = (outcome: ControlOutcome) => {
        start(outcome);
        turnWatchers.settle("s", 1, outcome);
      };
      return { endTurn, x };
    }

    it("reports the end once while the Mono is idle, and a later end again", async () => {
      const reports: string[] = [];
      const batches = batchesOf((_, message) => {
        reports.push(...reportedSessions(message));
      });
      const turnWatchers = createTurnWatchers();
      const { endTurn } = await startThenSteer(batches, turnWatchers);
      batches.closeInactive(() => false);
      vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
      endTurn(ended("s"));
      expect(reports).toEqual(["s"]);
      vi.runAllTimers();
      expect(reports).toEqual(["s"]);

      const again = monitor(batches, { ...origin, turn: 3 }, "send-s-2", "s");
      again.accept();
      batches.closeInactive(() => false);
      again(ended("s"));
      expect(reports).toEqual(["s", "s"]);
    });

    it("reports the end once while the Mono is mid-turn, and the request's other session once", async () => {
      let mono: Session = {
        ...newSession("codex", "/code/project"),
        busy: true,
        turnReadiness: "ready",
      };
      const batches = batchesOf((_, message) => {
        mono = enqueueMonoSessionCompletion(mono, message);
      });
      const turnWatchers = createTurnWatchers();
      const { endTurn, x } = await startThenSteer(batches, turnWatchers);
      batches.closeInactive(({ turn }) => turn === 2);
      endTurn(ended("s"));
      batches.closeInactive(() => false);
      // Turn 2's group no longer waits on the steer once `x` stops.
      x(ended("x"));
      expect(mono.queuedMessages?.map(reportedSessions)).toEqual([
        ["s"],
        ["x"],
      ]);
      vi.runAllTimers();
      expect(mono.queuedMessages).toHaveLength(2);
    });

    it("gives each Mono that watches the end its own single report", async () => {
      const reports: [string, string[]][] = [];
      const batches = batchesOf((monoId, message) => {
        reports.push([monoId, reportedSessions(message)]);
      });
      const turnWatchers = createTurnWatchers();
      const { endTurn } = await startThenSteer(batches, turnWatchers);
      await steer(batches, turnWatchers, { monoId: "other", turn: 1 }, "o-s");
      batches.closeInactive(() => false);
      vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
      endTurn(ended("s"));
      vi.runAllTimers();
      expect(reports).toEqual([
        ["mono", ["s"]],
        ["other", ["s"]],
      ]);
    });

    it("reports the steered turn's final answer from the open session", async () => {
      const s: Session = {
        ...newSession("codex", "/code/project"),
        id: "s",
        blocks: [
          { id: "u", role: "user", text: "Open the PR", startedAt: 1 },
          { id: "early", role: "assistant", text: "Now open the PR." },
          {
            id: "steer",
            role: "user",
            text: "Also state the size",
            appRequestId: "send-s",
            sentAt: 2,
          },
          { id: "final", role: "assistant", text: "PR #103 is open." },
          { id: "next", role: "user", text: "New task", startedAt: 3 },
          { id: "later", role: "assistant", text: "Unrelated answer" },
        ],
      };
      const results: string[] = [];
      const batches = new MonoSessionCompletionBatches(
        (_, message) => {
          const payload = JSON.parse(message.text.split("\n\n")[1]);
          results.push(payload.result);
        },
        schedule,
        (sessionId) => (sessionId === s.id ? s : undefined),
        { save() {}, deliver() {}, remove() {} },
      );
      const turnWatchers = createTurnWatchers();
      // The steer's receipt `send-s` is not its session id `s`.
      await steer(batches, turnWatchers, origin, "send-s");
      batches.closeInactive(() => false);
      turnWatchers.settle("s", 1, { status: "completed", text: "control" });
      expect(results).toEqual(["PR #103 is open."]);
    });

    it.each([true, false])(
      "reports two in-flight steers of one end once (first steer fails: %s)",
      async (firstFails) => {
        const reports: string[] = [];
        const batches = batchesOf(
          (_, message) => {
            reports.push(...reportedSessions(message));
          },
          () => undefined,
        );
        const turnWatchers = createTurnWatchers();
        const mono = { ...origin, turn: 2 };
        let finishFirst!: () => void;
        let acceptSecond!: () => void;
        // Two parallel `sessions.send` calls steer `s`; the turn ends while
        // both steers are in flight, then the first fails and the second lands.
        const first = steer(
          batches,
          turnWatchers,
          mono,
          "w2",
          () =>
            new Promise<void>((resolve, reject) => {
              finishFirst = () =>
                firstFails ? reject(new Error("Turn finished")) : resolve();
            }),
        );
        batches.closeInactive(() => false);
        const second = steer(
          batches,
          turnWatchers,
          mono,
          "w3",
          () => new Promise<void>((resolve) => (acceptSecond = resolve)),
        );
        turnWatchers.settle("s", 1, ended("s"));
        finishFirst();
        if (firstFails) await expect(first).rejects.toThrow("Turn finished");
        else await first;
        acceptSecond();
        expect((await second).delivery).toBe("steered");
        expect(reports).toEqual(["s"]);
      },
    );
  });
});
