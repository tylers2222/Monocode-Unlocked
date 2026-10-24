// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.reject(new Error("no invoke in turn guard tests")),
}));

import {
  guardedTurn,
  TURN_GUARD_LIMIT_MS,
  TURN_GUARD_RETRY_MS,
  turnGuardRequestId,
} from "./gigaWorkspace";
import * as gigaRuntime from "./gigaWorkspaceRuntime";
import { createMono, saveMonoGiga, saveMonoSessionId } from "./mono";
import { habitRunSessionId } from "./monoHabits";
import type { TurnId } from "./monoMessages";
import type { Session } from "../../sessions/model/session";

const T1 = "t1" as TurnId;
const T2 = "t2" as TurnId;
const T3 = "t3" as TurnId;

type Runtime = Pick<
  typeof gigaRuntime,
  "setTurnGuardHost" | "armTurnGuard" | "disarmTurnGuard"
>;
/** A chat as the guard sees it: its running turn, if any. */
type Chat = { turnId?: TurnId; canSteer: boolean };

/** A fake app: a clock moved by hand, its chats, the steers it got. */
function fakeApp(runtime: Runtime) {
  let now = 1_000;
  let timers: { at: number; run: () => void }[] = [];
  const chats = new Map<string, Chat>();
  const steers: {
    sessionId: string;
    turnId: TurnId;
    text: string;
    requestId: string;
  }[] = [];
  /** The turn of every steer the guard tried, accepted or not. */
  const attempts: TurnId[] = [];
  let steerFails = false;
  let steerHeld = false;
  let rejectHeld: (() => void) | undefined;
  runtime.setTurnGuardHost({
    schedule: (run, ms) => {
      const timer = { at: now + ms, run };
      timers.push(timer);
      return () => {
        timers = timers.filter((entry) => entry !== timer);
      };
    },
    turn: (sessionId, turnId) => {
      const chat = chats.get(sessionId);
      return chat?.turnId === turnId ? chat : undefined;
    },
    steer: async (sessionId, turnId, text, requestId) => {
      attempts.push(turnId);
      if (steerHeld)
        return new Promise<void>((_, reject) => {
          rejectHeld = () => reject(new Error("Turn not ready"));
        });
      if (steerFails) throw new Error("Turn not ready");
      steers.push({ sessionId, turnId, text, requestId });
    },
  });
  return {
    steers,
    attempts,
    chats,
    failSteers: (fail: boolean) => {
      steerFails = fail;
    },
    /** Steers stay pending until `rejectHeldSteer` rejects the last one. */
    holdSteers: () => {
      steerHeld = true;
    },
    rejectHeldSteer: async () => {
      rejectHeld?.();
      for (let i = 0; i < 5; i += 1) await Promise.resolve();
    },
    /** Starts `turnId` in `sessionId` the way submitSession does. */
    start(sessionId: string, turnId: TurnId, canSteer = true) {
      chats.set(sessionId, { turnId, canSteer });
      runtime.armTurnGuard(sessionId, turnId);
    },
    /** Ends the running turn; `finished` runs submitSession's finally. */
    end(sessionId: string, turnId: TurnId, finished = true) {
      const chat = chats.get(sessionId);
      if (chat?.turnId === turnId) chat.turnId = undefined;
      if (finished) runtime.disarmTurnGuard(sessionId, turnId);
    },
    async advance(ms: number) {
      const until = now + ms;
      for (;;) {
        const next = timers
          .filter((timer) => timer.at <= until)
          .sort((a, b) => a.at - b.at)[0];
        if (!next) break;
        timers = timers.filter((timer) => timer !== next);
        now = next.at;
        next.run();
        await Promise.resolve();
        await Promise.resolve();
      }
      now = until;
    },
    pending: () => timers.length,
  };
}

const runtime: Runtime = gigaRuntime;
let app = fakeApp(runtime);

function chat(giga: boolean): string {
  const mono = createMono();
  const sessionId = `chat-${mono.id}`;
  saveMonoSessionId(mono.id, sessionId);
  if (giga) saveMonoGiga(mono.id, true, "/home/u/MonoCode");
  return sessionId;
}

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
  app = fakeApp(runtime);
});
afterEach(() => vi.unstubAllGlobals());

// Defect: a long Giga turn is never told to move its work, gets the wrong
// text, or the steer is not tagged with the turn it belongs to.
it("steers a Giga chat turn once it runs 3 minutes", async () => {
  const giga = chat(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS - 1);
  expect(app.steers).toEqual([]);
  await app.advance(1);
  expect(app.steers).toEqual([
    {
      sessionId: giga,
      turnId: T1,
      text: "Move this work to a session in the MonoCode project and end your turn.",
      requestId: turnGuardRequestId(T1),
    },
  ]);
});

// Defect: a long Mono turn (not a Giga) is never told to move its work, so
// the notices of its monitored sessions wait behind it, or it is steered
// more than once.
it("steers a Mono chat turn once it runs 3 minutes", async () => {
  const mono = chat(false);
  app.start(mono, T1);
  await app.advance(TURN_GUARD_LIMIT_MS - 1);
  expect(app.steers).toEqual([]);
  await app.advance(1);
  expect(app.steers).toEqual([
    {
      sessionId: mono,
      turnId: T1,
      text: "Move this work to a session in the MonoCode project and end your turn.",
      requestId: turnGuardRequestId(T1),
    },
  ]);
  await app.advance(TURN_GUARD_LIMIT_MS * 3);
  expect(app.steers).toHaveLength(1);
  expect(app.pending()).toBe(0);
});

// Defect: the guard steers sessions that are no Mono's own chat (project
// sessions, habit runs of Monos and Gigas, orchestration workers).
it("leaves project sessions and habit runs alone past 3 minutes", async () => {
  app.start("project-session", T1);
  app.start(habitRunSessionId(chat(true)), T2);
  app.start(habitRunSessionId(chat(false)), T3);
  expect(app.pending()).toBe(0);
  await app.advance(TURN_GUARD_LIMIT_MS * 3);
  expect(app.steers).toEqual([]);
});

// Defect: a turn that ends before the limit still gets the steer or leaves
// its timer behind, also when it ends by a path that never disarms the guard.
it("gives a turn that ends before 3 minutes nothing", async () => {
  const giga = chat(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS - 1_000);
  app.end(giga, T1);
  expect(app.pending()).toBe(0);
  await app.advance(TURN_GUARD_LIMIT_MS);

  app.start(giga, T2);
  app.end(giga, T2, false);
  await app.advance(TURN_GUARD_LIMIT_MS);
  expect(app.steers).toEqual([]);
  expect(app.pending()).toBe(0);
});

// Defect: the first turn's guard steers the next turn, the next turn
// keeps the old guard, or the old turn's late finally clears the new guard.
it("re-arms for the next turn", async () => {
  const giga = chat(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS);
  app.end(giga, T1);
  app.start(giga, T2);
  await app.advance(60_000);
  app.end(giga, T2, false);
  app.start(giga, T3);
  runtime.disarmTurnGuard(giga, T2);
  await app.advance(TURN_GUARD_LIMIT_MS);
  expect(app.steers.map((steer) => steer.turnId)).toEqual([T1, T3]);
});

// Defect: a turn that cannot take a steer at the limit is polled forever,
// or never gets the steer once it can.
it("checks a turn that cannot take the steer once more, then gives up", async () => {
  const giga = chat(true);
  app.start(giga, T1, false);
  await app.advance(TURN_GUARD_LIMIT_MS + TURN_GUARD_RETRY_MS * 10);
  expect(app.steers).toEqual([]);
  expect(app.pending()).toBe(0);

  app.end(giga, T1);
  app.start(giga, T2, false);
  await app.advance(TURN_GUARD_LIMIT_MS);
  app.chats.get(giga)!.canSteer = true;
  await app.advance(TURN_GUARD_RETRY_MS);
  expect(app.steers.map((steer) => steer.turnId)).toEqual([T2]);
});

// Defect: a steer the harness rejects is retried in a loop, or more than once.
it("tries a rejected steer once more, then gives up", async () => {
  const giga = chat(true);
  app.failSteers(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS + TURN_GUARD_RETRY_MS * 10);
  expect(app.attempts).toEqual([T1, T1]);
  expect(app.pending()).toBe(0);

  app.end(giga, T1);
  app.start(giga, T2);
  await app.advance(TURN_GUARD_LIMIT_MS);
  app.failSteers(false);
  await app.advance(TURN_GUARD_RETRY_MS);
  expect(app.steers.map((steer) => steer.turnId)).toEqual([T2]);
});

// Defect: a late rejection of turn 1's steer clears turn 2's guard, so
// turn 2 is never steered.
it("leaves the next turn's guard alone when the last turn's steer rejects late", async () => {
  const giga = chat(true);
  app.holdSteers();
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS);
  app.end(giga, T1);
  app.start(giga, T2);
  await app.rejectHeldSteer();
  await app.advance(TURN_GUARD_LIMIT_MS);
  expect(app.attempts).toEqual([T1, T2]);
});

function session(fields: Partial<Session>): Session {
  return {
    id: "chat",
    harness: "claude",
    busy: true,
    turnReadiness: "ready",
    blocks: [],
    ...fields,
  } as Session;
}

// Defect: the guard steers a turn that is no longer the running one, or a
// turn held by a pending question.
it("sees only the running turn, and steers it only when it is ready", () => {
  const steers = () => true;
  expect(guardedTurn(session({}), T2, T1, steers)).toBeUndefined();
  expect(
    guardedTurn(
      session({
        pendingQuestion: { autoResolveAt: null } as Session["pendingQuestion"],
      }),
      T1,
      T1,
      steers,
    ),
  ).toEqual({ canSteer: false });
  expect(guardedTurn(session({}), T1, T1, steers)).toEqual({ canSteer: true });
});
