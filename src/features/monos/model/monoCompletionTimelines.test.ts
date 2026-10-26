import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlOutcome } from "../../orchestration/model/orchestration";
import {
  newSession,
  type QueuedMessage,
  type Session,
} from "../../sessions/model/session";
import {
  enqueueMonoSessionCompletion,
  MonoSessionCompletionBatches,
} from "./monoSessionCompletion";

// Read from a .backup copy of the MonoCode 2 database (com.monocode.second):
// launches from the Mono turn's monoSpawnedSessions, the Mono going idle from
// that turn's durationMs, and each member's end from its own turn's durationMs.
// A sessions.send to a busy session joins its running turn, so it ends with it.
// Session IDs are the last 8 characters.
type Event =
  | { at: string; kind: "start" | "send" | "end"; session: string }
  | { at: string; kind: "monoTurn" | "monoIdle" };

type Timeline = {
  name: string;
  events: Event[];
  /** When each notice reaches the Mono's queue, and the sessions it covers. */
  notices: [string, string[]][];
};

const TIMELINES: Timeline[] = [
  {
    name: "ADE-8 review r1 d2bacdcb reports without waiting for 87c4cc75",
    events: [
      { at: "10-18 23:32:08", kind: "start", session: "d2bacdcb" },
      { at: "10-18 23:32:40", kind: "start", session: "87c4cc75" },
      { at: "10-18 23:32:55", kind: "monoIdle" },
      { at: "10-18 23:38:51", kind: "end", session: "d2bacdcb" },
      { at: "10-19 00:30:34", kind: "end", session: "87c4cc75" },
    ],
    notices: [
      ["10-18 23:38:51", ["d2bacdcb"]],
      ["10-19 00:30:34", ["87c4cc75"]],
    ],
  },
  {
    name: "#93 fix r1 7a1eab69 reports without waiting for 102e462a",
    events: [
      { at: "10-19 14:44:28", kind: "start", session: "102e462a" },
      { at: "10-19 14:45:00", kind: "start", session: "7a1eab69" },
      { at: "10-19 14:45:16", kind: "monoIdle" },
      { at: "10-19 15:14:01", kind: "end", session: "7a1eab69" },
      { at: "10-19 15:44:26", kind: "end", session: "102e462a" },
    ],
    notices: [
      ["10-19 15:14:01", ["7a1eab69"]],
      ["10-19 15:44:26", ["102e462a"]],
    ],
  },
  {
    name: "four 18:22 launches report before the gate builder 30f5be8b, which reports alone",
    events: [
      { at: "10-19 18:17:11", kind: "start", session: "281d65ca" },
      { at: "10-19 18:21:27", kind: "start", session: "ef8f4c9c" },
      { at: "10-19 18:21:31", kind: "start", session: "8b05c54b" },
      { at: "10-19 18:22:02", kind: "start", session: "960dacc8" },
      { at: "10-19 18:22:02", kind: "start", session: "30f5be8b" },
      { at: "10-19 18:22:42", kind: "monoIdle" },
      { at: "10-19 18:38:56", kind: "end", session: "960dacc8" },
      { at: "10-19 18:43:24", kind: "end", session: "281d65ca" },
      { at: "10-19 18:43:50", kind: "end", session: "8b05c54b" },
      { at: "10-19 18:46:11", kind: "end", session: "ef8f4c9c" },
      { at: "10-19 19:27:31", kind: "end", session: "30f5be8b" },
    ],
    notices: [
      ["10-19 18:38:56", ["960dacc8"]],
      ["10-19 18:43:24", ["281d65ca"]],
      ["10-19 18:43:50", ["8b05c54b"]],
      ["10-19 18:46:11", ["ef8f4c9c"]],
      ["10-19 19:27:31", ["30f5be8b"]],
    ],
  },
  {
    name: "6918d65a, done during the turn, reports 60 s after it ends; the long runner 4abb60aa reports alone",
    events: [
      { at: "10-19 09:51:55", kind: "start", session: "6918d65a" },
      { at: "10-19 09:53:27", kind: "end", session: "6918d65a" },
      { at: "10-19 10:10:43", kind: "start", session: "4abb60aa" },
      { at: "10-19 10:10:58", kind: "monoIdle" },
      { at: "10-19 10:49:03", kind: "end", session: "4abb60aa" },
    ],
    notices: [
      ["10-19 10:11:58", ["6918d65a"]],
      ["10-19 10:49:03", ["4abb60aa"]],
    ],
  },
  {
    name: "sessions.send group 1839 steers 30f5be8b while group 1790 still watches it",
    events: [
      { at: "10-19 18:17:11", kind: "start", session: "281d65ca" },
      { at: "10-19 18:21:27", kind: "start", session: "ef8f4c9c" },
      { at: "10-19 18:21:31", kind: "start", session: "8b05c54b" },
      { at: "10-19 18:22:02", kind: "start", session: "960dacc8" },
      { at: "10-19 18:22:02", kind: "start", session: "30f5be8b" },
      { at: "10-19 18:22:42", kind: "monoIdle" },
      { at: "10-19 18:38:56", kind: "end", session: "960dacc8" },
      { at: "10-19 18:43:24", kind: "end", session: "281d65ca" },
      { at: "10-19 18:43:50", kind: "end", session: "8b05c54b" },
      { at: "10-19 18:46:11", kind: "end", session: "ef8f4c9c" },
      { at: "10-19 19:16:50", kind: "monoTurn" },
      { at: "10-19 19:17:32", kind: "send", session: "7a1eab69" },
      { at: "10-19 19:17:32", kind: "send", session: "4d93f1f2" },
      { at: "10-19 19:17:33", kind: "send", session: "30f5be8b" },
      { at: "10-19 19:18:20", kind: "monoIdle" },
      { at: "10-19 19:27:31", kind: "end", session: "30f5be8b" },
      { at: "10-19 19:43:26", kind: "end", session: "7a1eab69" },
      { at: "10-19 19:52:39", kind: "end", session: "4d93f1f2" },
    ],
    // Both requests watch 30f5be8b's one turn end. tyler/main reported it to
    // each (seq 1892 and 1914); the Mono now gets it once (ADE-47).
    notices: [
      ["10-19 18:38:56", ["960dacc8"]],
      ["10-19 18:43:24", ["281d65ca"]],
      ["10-19 18:43:50", ["8b05c54b"]],
      ["10-19 18:46:11", ["ef8f4c9c"]],
      ["10-19 19:27:31", ["30f5be8b"]],
      ["10-19 19:43:26", ["7a1eab69"]],
      ["10-19 19:52:39", ["4d93f1f2"]],
    ],
  },
];

const time = (at: string) => new Date(`2026-${at.replace(" ", "T")}`).getTime();
const two = (value: number) => String(value).padStart(2, "0");
function stamp(ms: number) {
  const date = new Date(ms);
  return `${two(date.getMonth() + 1)}-${two(date.getDate())} ${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

function reportedSessions(message: QueuedMessage): string[] {
  const completion = message.monoSessionCompletion!;
  if (!completion.sessionCount) return [completion.sessionId];
  const payload = JSON.parse(
    message.text.slice(message.text.indexOf("\n\n") + 2),
  );
  return payload.sessions.map(
    (entry: { sessionId: string }) => entry.sessionId,
  );
}

/** Replays a timeline through the batches and queue App.tsx uses. */
function replay(events: Event[]) {
  vi.setSystemTime(time(events[0].at));
  let mono: Session = { ...newSession("omp", "/Users/me"), id: "mono" };
  const notices: [string, string[]][] = [];
  const batches = new MonoSessionCompletionBatches(
    (_, message) => {
      const queued = mono.queuedMessages?.length ?? 0;
      mono = enqueueMonoSessionCompletion(mono, message);
      for (const added of mono.queuedMessages!.slice(queued))
        notices.push([stamp(Date.now()), reportedSessions(added)]);
    },
    (run, ms) => setTimeout(run, ms),
    () => undefined,
    { hold() {}, dismiss() {} },
  );
  /** Every request that watches a session's running turn. */
  const watchers = new Map<string, ((outcome: ControlOutcome) => void)[]>();
  let turn = 1;
  let monoBusy = true;
  for (const event of events) {
    vi.advanceTimersByTime(time(event.at) - Date.now());
    if (event.kind === "start" || event.kind === "send") {
      const settle = batches.watch(
        { monoId: mono.id, turn },
        {
          requestId: `${event.kind}-${event.session}`,
          sessionId: event.session,
          project: "/Users/me/monocode",
          prompt: `Brief for ${event.session}`,
        },
        { awaitAcceptance: true },
      );
      settle.accept();
      watchers.set(event.session, [
        ...(watchers.get(event.session) ?? []),
        settle,
      ]);
    } else if (event.kind === "monoTurn") {
      turn += 1;
      monoBusy = true;
    } else if (event.kind === "monoIdle") {
      monoBusy = false;
    } else {
      // The turn's one outcome reaches every watcher, as in `submitSession`.
      const outcome: ControlOutcome = {
        status: "completed",
        text: `Done ${event.session}`,
      };
      for (const settle of watchers.get(event.session)!) settle(outcome);
      watchers.delete(event.session);
    }
    // App.tsx runs closeInactive on every sessions change, with the same rule.
    batches.closeInactive((origin) => monoBusy && origin.turn === turn);
  }
  vi.runAllTimers();
  return notices;
}

describe("real Mono completion timelines", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it.each(TIMELINES)("$name", ({ events, notices }) => {
    expect(replay(events)).toEqual(notices);
  });
});
