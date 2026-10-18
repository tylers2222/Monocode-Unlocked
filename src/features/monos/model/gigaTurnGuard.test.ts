// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.reject(new Error("no invoke in turn guard tests")),
}));

import {
  TURN_GUARD_LIMIT_MS,
  TURN_GUARD_RETRY_MS,
  TURN_GUARD_STEER,
  turnGuardRequestId,
} from "./gigaWorkspace";
import * as gigaRuntime from "./gigaWorkspaceRuntime";
import { createMono, saveMonoGiga, saveMonoSessionId } from "./mono";
import { habitRunSessionId } from "./monoHabits";
import type { TurnId } from "./monoMessages";

const T1 = "t1" as TurnId;
const T2 = "t2" as TurnId;
const T3 = "t3" as TurnId;

type Runtime = Pick<
  typeof gigaRuntime,
  "setTurnGuardHost" | "armTurnGuard" | "disarmTurnGuard"
>;
/** A chat as the guard sees it: its running turn, if any, and its blocks. */
type Chat = {
  turnId?: TurnId;
  canSteer: boolean;
  blocks: { appRequestId?: string }[];
};

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
  let steerFails = false;
  runtime.setTurnGuardHost({
    now: () => now,
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
      if (steerFails) throw new Error("Turn not ready");
      steers.push({ sessionId, turnId, text, requestId });
      chats.get(sessionId)?.blocks.push({ appRequestId: requestId });
    },
  });
  return {
    steers,
    chats,
    failSteers: (fail: boolean) => {
      steerFails = fail;
    },
    /** Starts `turnId` in `sessionId` the way submitSession does. */
    start(sessionId: string, turnId: TurnId, canSteer = true) {
      const blocks = chats.get(sessionId)?.blocks ?? [];
      chats.set(sessionId, { turnId, canSteer, blocks });
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

let runtime: Runtime = gigaRuntime;
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
  runtime = gigaRuntime;
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
      text: TURN_GUARD_STEER,
      requestId: turnGuardRequestId(T1),
    },
  ]);
});

// Defect: the guard steers Mono chats or sessions that are no Mono's chat
// (project sessions, habit runs, orchestration workers).
it("leaves a Mono chat and other sessions alone past 3 minutes", async () => {
  const mono = chat(false);
  app.start(mono, T1);
  app.start("project-session", T2);
  app.start(habitRunSessionId(chat(true)), T3);
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

// Defect: a second check of the same turn steers it again, whether the guard
// is armed again in this run or after a reload, where the steer's block is
// the only record left.
it("never steers the same turn twice, even after a reload", async () => {
  const giga = chat(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS);
  runtime.armTurnGuard(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS * 3);
  expect(app.steers).toHaveLength(1);

  const saved = app.chats.get(giga)!;
  // A reload is a fresh copy of the module: only the session's blocks remain.
  vi.resetModules();
  runtime = await import("./gigaWorkspaceRuntime");
  app = fakeApp(runtime);
  app.chats.set(giga, saved);
  runtime.armTurnGuard(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS * 3);
  expect(app.steers).toEqual([]);
});

// Defect: the first turn's steer marks the next turn steered, the next turn
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

// Defect: a steer the harness rejects is retried in a loop.
it("tries a rejected steer once more, then gives up", async () => {
  const giga = chat(true);
  app.failSteers(true);
  app.start(giga, T1);
  await app.advance(TURN_GUARD_LIMIT_MS + TURN_GUARD_RETRY_MS * 10);
  expect(app.pending()).toBe(0);

  app.end(giga, T1);
  app.start(giga, T2);
  await app.advance(TURN_GUARD_LIMIT_MS);
  app.failSteers(false);
  await app.advance(TURN_GUARD_RETRY_MS);
  expect(app.steers.map((steer) => steer.turnId)).toEqual([T2]);
});
