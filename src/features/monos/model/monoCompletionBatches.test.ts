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
  monoSessionCompletionResult,
  MonoSessionCompletionBatches,
} from "./monoSessionCompletion";

const origin = { monoId: "mono", turn: 1 };
const schedule = (run: () => void, ms: number) => setTimeout(run, ms);
function result(
  id: string,
  status: "completed" | "failed" | "cancelled" = "completed",
) {
  return monoSessionCompletionResult({
    requestId: id,
    sessionId: id,
    project: "/code/project",
    prompt: `Review ${id}`,
    outcome: {
      status,
      text: `Result ${id}`,
      ...(status === "failed" ? { error: "Launch failed" } : {}),
    },
  });
}

describe("Mono completion batches", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("waits for the launching turn, then sends results that stop within the hold together", () => {
    const ready = vi.fn();
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const a = batches.watch(origin, "a");
    a(result("a"));
    batches.closeInactive(() => true);
    expect(ready).not.toHaveBeenCalled();
    const b = batches.watch(origin, "b");
    const c = batches.watch(origin, "c");
    c(result("c"));
    batches.closeInactive(() => false);
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS - 1);
    expect(ready).not.toHaveBeenCalled();
    b(result("b"));
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
    a(result("a"));
    b(result("b"));
    batches.closeInactive(() => false);
    vi.runAllTimers();
    expect(ready).toHaveBeenCalledOnce();
  });

  it("sends failures and cancellations when the hold ends, then a later result alone", async () => {
    const ready = vi.fn();
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const failed = batches.watch(origin, "failed");
    const cancelled = batches.watch(origin, "cancelled");
    const succeeded = batches.watch(origin, "success");
    batches.closeInactive(() => false);
    await submitWithSettlement({
      submit: () => false,
      onSettled: (outcome) =>
        failed(
          monoSessionCompletionResult({
            requestId: "failed",
            sessionId: "failed",
            project: "/code/project",
            prompt: "Review",
            outcome,
          }),
        ),
      rejectionMessage: "Launch failed",
    });
    cancelled(result("cancelled", "cancelled"));
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
    succeeded(result("success"));
    expect(ready).toHaveBeenCalledTimes(2);
    expect(ready.mock.calls[1][1].monoSessionCompletion).toEqual({
      sessionId: "success",
      title: "Agent session",
      status: "completed",
    });
  });

  it("keeps later requests and other Monos independent of a slow earlier group", () => {
    const ready = vi.fn();
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const slow = batches.watch(origin, "slow");
    batches.watch(origin, "also-slow");
    const later = batches.watch({ ...origin, turn: 2 }, "later");
    const other = batches.watch({ monoId: "another", turn: 1 }, "other");
    batches.closeInactive(() => false);
    later(result("later"));
    other(result("other"));
    expect(ready.mock.calls.map(([monoId]) => monoId)).toEqual([
      "mono",
      "another",
    ]);
    slow(result("slow"));
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
    const batches = new MonoSessionCompletionBatches((_, message) => {
      mono = enqueueMonoSessionCompletion(mono, message);
    }, schedule);
    const a = batches.watch(origin, "a");
    const b = batches.watch(origin, "b");
    batches.closeInactive(() => false);
    b(result("b"));
    vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS - 1);
    expect(mono.queuedMessages).toHaveLength(1);
    a(result("a"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const done = batches.watch(origin, "request");
    const snapshot = result("worker");
    done(snapshot);
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const rejected = batches.watch(origin, "request");
    rejected(result("worker", "failed"));
    const retry = batches.watch(origin, "request");
    batches.closeInactive(() => false);
    rejected(result("worker", "failed"));
    expect(ready).not.toHaveBeenCalled();
    retry(result("worker"));
    expect(ready).toHaveBeenCalledOnce();
    expect(ready.mock.calls[0][1].monoSessionCompletion.status).toBe(
      "completed",
    );
  });

  it("dismisses the calling Mono's pending cancellation without losing other session results", () => {
    const ready = vi.fn();
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const stopped = batches.watch(origin, "stop-request", "stopped-session");
    const other = batches.watch(origin, "other-request", "other-session");
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "stopped-session");
    stopped(result("stopped-session", "cancelled"));
    expect(ready).not.toHaveBeenCalled();
    other(result("other-session"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const cancelled = batches.watch(origin, "request", "worker");
    const other = batches.watch(
      { monoId: "another", turn: 1 },
      "other",
      "worker",
    );
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "worker");
    cancelled(result("worker", "cancelled"));
    other(result("worker", "cancelled"));
    expect(ready.mock.calls.map(([monoId]) => monoId)).toEqual(["another"]);

    const followUp = batches.watch(
      { ...origin, turn: 2 },
      "follow-up",
      "worker",
    );
    batches.closeInactive(() => false);
    cancelled(result("worker", "cancelled"));
    expect(ready).toHaveBeenCalledOnce();
    followUp(result("worker"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const rejected = batches.watch(
      origin,
      "rejected-request",
      "failed-session",
    );
    const accepted = await submitWithSettlement({
      submit: () => false,
      onSettled: (outcome) =>
        rejected(
          monoSessionCompletionResult({
            requestId: "rejected-request",
            sessionId: "failed-session",
            project: "/code/project",
            prompt: "Review",
            outcome,
          }),
        ),
      rejectionMessage: "Session was not open",
    });
    expect(accepted).toBe(false);
    rejected.discard();
    const launched = batches.watch(origin, "launched-request", "worker");
    batches.closeInactive(() => false);
    batches.dismissSession(origin.monoId, "worker");
    launched(result("worker", "cancelled"));
    expect(ready).not.toHaveBeenCalled();
  });

  it.each([false, true])(
    "does not deliver a rejected submission when the Mono becomes idle before acceptance (throws: %s)",
    async (throws) => {
      const ready = vi.fn();
      const batches = new MonoSessionCompletionBatches(ready, schedule);
      const rejected = batches.watch(origin, "request", "worker", {
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
        onSettled: (outcome) =>
          rejected(
            monoSessionCompletionResult({
              requestId: "request",
              sessionId: "worker",
              project: "/code/project",
              prompt: "Review",
              outcome,
            }),
          ),
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
      rejected(result("worker", "failed"));
      batches.closeInactive(() => false);
      expect(ready).not.toHaveBeenCalled();
    },
  );

  it.each(["completed", "failed", "cancelled"] as const)(
    "keeps an early %s event from an accepted submission and releases it after acceptance",
    (status) => {
      const ready = vi.fn();
      const batches = new MonoSessionCompletionBatches(ready, schedule);
      const watched = batches.watch(origin, "request", "worker", {
        awaitAcceptance: true,
      });
      batches.closeInactive(() => false);
      watched(result("worker", status));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const rejected = batches.watch(origin, "rejected-request", "rejected", {
      awaitAcceptance: true,
    });
    batches.watch(origin, "accepted-request", "accepted")(result("accepted"));
    batches.closeInactive(() => false);
    rejected(result("rejected", "failed"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const undecided = batches.watch(origin, "undecided-request", "undecided", {
      awaitAcceptance: true,
    });
    batches.watch(origin, "accepted-request", "accepted")(result("accepted"));
    batches.closeInactive(() => false);
    undecided(result("undecided", "failed"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    const rejected = batches.watch(origin, "request", "worker");
    rejected.discard();
    const retry = batches.watch(origin, "request", "worker");
    rejected(result("worker", "failed"));
    rejected.discard();
    batches.closeInactive(() => false);
    expect(ready).not.toHaveBeenCalled();
    retry(result("worker"));
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
      const batches = new MonoSessionCompletionBatches(
        async (_, _message, current) => {
          await waiting;
          const message = current();
          if (message) delivered(message);
        },
        schedule,
      );
      batches.watch(origin, "request", "worker")(result("worker", "cancelled"));
      if (retainOther) batches.watch(origin, "other", "other")(result("other"));
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
    const batches = new MonoSessionCompletionBatches(ready, schedule);
    for (let index = 0; index < 40; index++) {
      const id = `worker-${index}`;
      batches.watch(
        origin,
        id,
      )({
        ...result(id),
        result: "x".repeat(12_000),
        originalPrompt: "y".repeat(12_000),
      });
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

    /** One Mono request as `monitorCompletion` in App.tsx wires it. */
    function monitor(
      batches: MonoSessionCompletionBatches,
      mono: { monoId: string; turn: number },
      requestId: string,
      sessionId: string,
    ) {
      const settle = batches.watch(mono, requestId, sessionId, {
        awaitAcceptance: true,
      });
      return Object.assign(
        (outcome: ControlOutcome) =>
          settle(
            monoSessionCompletionResult({
              requestId,
              sessionId,
              project: "/code/project",
              prompt: `Prompt ${requestId}`,
              outcome,
            }),
            outcome,
          ),
        { accept: settle.accept, discard: settle.discard },
      );
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
      await deliverAppSteer({
        watchers: turnWatchers,
        sessionId: "s",
        gen: 1,
        watcher: monitor(batches, { ...origin, turn: 2 }, "send-s", "s"),
        record: () => undefined,
        steer: async () => undefined,
        forget: () => undefined,
      });
      // How `submitSession` settles the end of `s`'s turn 1.
      const endTurn = (outcome: ControlOutcome) => {
        start(outcome);
        turnWatchers.settle("s", 1, outcome);
      };
      return { endTurn, x };
    }

    it("reports the end once while the Mono is idle, and a later end again", async () => {
      const reports: string[] = [];
      const batches = new MonoSessionCompletionBatches((_, message) => {
        reports.push(...reportedSessions(message));
      }, schedule);
      const turnWatchers = createTurnWatchers();
      const { endTurn } = await startThenSteer(batches, turnWatchers);
      batches.closeInactive(() => false);
      vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
      endTurn({ status: "completed", text: "s finished" });
      expect(reports).toEqual(["s"]);
      vi.runAllTimers();
      expect(reports).toEqual(["s"]);

      const again = monitor(batches, { ...origin, turn: 3 }, "send-s-2", "s");
      again.accept();
      batches.closeInactive(() => false);
      again({ status: "completed", text: "s finished again" });
      expect(reports).toEqual(["s", "s"]);
    });

    it("reports the end once while the Mono is mid-turn, and the request's other session once", async () => {
      let mono: Session = {
        ...newSession("codex", "/code/project"),
        busy: true,
        turnReadiness: "ready",
      };
      const batches = new MonoSessionCompletionBatches((_, message) => {
        mono = enqueueMonoSessionCompletion(mono, message);
      }, schedule);
      const turnWatchers = createTurnWatchers();
      const { endTurn, x } = await startThenSteer(batches, turnWatchers);
      batches.closeInactive(({ turn }) => turn === 2);
      endTurn({ status: "completed", text: "s finished" });
      batches.closeInactive(() => false);
      // Turn 2's group no longer waits on the steer once `x` stops.
      x({ status: "completed", text: "x finished" });
      expect(mono.queuedMessages?.map(reportedSessions)).toEqual([
        ["s"],
        ["x"],
      ]);
      vi.runAllTimers();
      expect(mono.queuedMessages).toHaveLength(2);
    });

    it("gives each Mono that watches the end its own single report", async () => {
      const reports: [string, string[]][] = [];
      const batches = new MonoSessionCompletionBatches((monoId, message) => {
        reports.push([monoId, reportedSessions(message)]);
      }, schedule);
      const turnWatchers = createTurnWatchers();
      const { endTurn } = await startThenSteer(batches, turnWatchers);
      const other = { monoId: "other", turn: 1 };
      await deliverAppSteer({
        watchers: turnWatchers,
        sessionId: "s",
        gen: 1,
        watcher: monitor(batches, other, "other-send-s", "s"),
        record: () => undefined,
        steer: async () => undefined,
        forget: () => undefined,
      });
      batches.closeInactive(() => false);
      vi.advanceTimersByTime(MONO_COMPLETION_HOLD_MS);
      endTurn({ status: "completed", text: "s finished" });
      vi.runAllTimers();
      expect(reports).toEqual([
        ["mono", ["s"]],
        ["other", ["s"]],
      ]);
    });
  });
});
