// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AlarmsProvider,
  useAlarmsFeed,
  useSessionStalls,
  useTranscriptAlarms,
  type AlarmsWindowHost,
} from "./AlarmsProvider";
import { ALARMS_CHANGED, type AlarmsFeed } from "../model/alarmsFeed";
import {
  DIALOG_EPISODE,
  IDLE_EPISODE,
  READ_WALL_MS,
  WAITING_HOLD,
  snapshotWire,
} from "../test/alarmsFixtures";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, () => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => tauri.invoke(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: () => void) => {
    tauri.listeners.set(event, handler);
    return () => tauri.listeners.delete(event);
  },
}));

const HOST: AlarmsWindowHost = {
  openAgent: () => {},
  answerDialog: () => {},
  sendAsUser: () => {},
  refused: () => {},
};

let root: Root;
let container: HTMLDivElement;
let answer: unknown;
let feed: AlarmsFeed | undefined;
let stallRenders = 0;

function Stalls() {
  const stalls = useSessionStalls();
  stallRenders += 1;
  return createElement(
    "p",
    { "data-stalls": "" },
    [...(stalls?.values() ?? [])]
      .map((stall) => `${stall.id}:${stall.minutes}`)
      .join(","),
  );
}

function Window() {
  feed = useAlarmsFeed(HOST);
  return createElement(AlarmsProvider, { feed }, createElement(Stalls));
}

const viewCalls = () =>
  tauri.invoke.mock.calls.filter(([command]) => command === "alarms_view")
    .length;

async function changed() {
  await act(async () => tauri.listeners.get(ALARMS_CHANGED)?.());
}

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  stallRenders = 0;
  tauri.listeners.clear();
  answer = snapshotWire({ episodes: [IDLE_EPISODE], holds: [] });
  tauri.invoke.mockReset();
  tauri.invoke.mockImplementation(async (command: string) =>
    command === "alarms_view" ? answer : null,
  );
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => root.render(createElement(Window)));
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const shown = () => container.querySelector("[data-stalls]")?.textContent ?? "";

describe("alarms feed", () => {
  it("fetches on mount and again on each alarms-changed event", async () => {
    // Defect: a stall Rust reports after mount never reaches the sidebar.
    expect(viewCalls()).toBe(1);
    expect(shown()).toBe("al_7Q2:17");
    answer = snapshotWire({
      episodes: [{ ...IDLE_EPISODE, minutes: 18 }, DIALOG_EPISODE],
      holds: [],
    });
    await changed();
    expect(viewCalls()).toBe(2);
    expect(shown()).toBe("al_7Q2:18,al_9KD:19");
  });

  it("keeps one fetch in flight, and runs exactly one more for events during it", async () => {
    // Defect: a burst of events starts a fetch each, and an older answer lands last.
    let release: (() => void) | undefined;
    tauri.invoke.mockImplementation(
      () =>
        new Promise((resolve) => {
          release = () => resolve(answer);
        }),
    );
    void feed!.refresh();
    await changed();
    await changed();
    await changed();
    expect(viewCalls()).toBe(2);
    await act(async () => release?.());
    expect(viewCalls()).toBe(3);
    await act(async () => release?.());
    expect(viewCalls()).toBe(3);
  });

  it("re-renders nothing when a fetch brings the same views", async () => {
    // Defect: every 5 s event re-renders the sidebar and every transcript row.
    const before = feed!.surfaces();
    const renders = stallRenders;
    await changed();
    expect(viewCalls()).toBe(2);
    expect(feed!.surfaces()).toBe(before);
    expect(stallRenders).toBe(renders);
  });

  it("wakes only the transcript row whose card changed", async () => {
    // Defect: one card's new minute replaces every card object, so every row re-renders.
    answer = snapshotWire({
      episodes: [IDLE_EPISODE, DIALOG_EPISODE],
      holds: [],
    });
    await changed();
    const lead = feed!.transcript("51aa");
    expect(feed!.transcript("51aa")).toBe(lead);
    const idle = lead.cards.get("al_7Q2");
    const woke: string[] = [];
    lead.cards.subscribe("al_7Q2", () => woke.push("al_7Q2"));
    lead.cards.subscribe("al_9KD", () => woke.push("al_9KD"));
    answer = snapshotWire({
      episodes: [IDLE_EPISODE, { ...DIALOG_EPISODE, minutes: 20 }],
      holds: [],
    });
    await changed();
    expect(woke).toEqual(["al_9KD"]);
    expect(lead.cards.get("al_7Q2")).toBe(idle);
    expect(lead.cards.get("al_9KD")?.minutes).toBe(20);
  });

  it("names a transcript's alarm in the read, and keeps its card once it closed", async () => {
    // Defect: the read lists open episodes only, so a delivered alarm's card
    // vanishes when its episode closes and the raw tag shows in its place.
    const closed = {
      ...IDLE_EPISODE,
      nextMinutes: null,
      closed: { reason: "progress", at: "14:30" },
    };
    tauri.invoke.mockImplementation(
      async (command: string, args?: { alarms?: string[] }) =>
        command === "alarms_view"
          ? snapshotWire({
              episodes: [],
              closed: args?.alarms?.includes(closed.id) ? [closed] : [],
              holds: [],
            })
          : null,
    );
    await changed();
    const lead = feed!.transcript("51aa");
    expect(lead.cards.get(closed.id)).toBeUndefined();
    const reads = viewCalls();
    const woke: string[] = [];
    await act(async () => {
      lead.cards.subscribe(closed.id, () => woke.push(closed.id));
      lead.cards.subscribe("al_unknown", () => woke.push("al_unknown"));
    });
    expect(viewCalls()).toBe(reads + 1);
    expect(tauri.invoke.mock.calls.at(-1)).toEqual([
      "alarms_view",
      { alarms: [closed.id, "al_unknown"] },
    ]);
    expect(woke).toEqual([closed.id]);
    expect(lead.cards.get(closed.id)?.closed).toEqual({
      reason: "progress",
      at: "14:30",
    });
    expect(lead.cards.get("al_unknown")).toBeUndefined();

    await act(async () => {
      lead.cards.subscribe(closed.id, () => {});
    });
    expect(viewCalls()).toBe(reads + 1);
  });

  it("ticks a waiting hold's countdown each second, and stops once none waits", async () => {
    // Defect: the countdown freezes between snapshots, or a timer runs with nothing to count.
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    vi.setSystemTime(READ_WALL_MS);
    answer = snapshotWire({ episodes: [], holds: [WAITING_HOLD] });
    await changed();
    const worker = feed!.transcript("9f3c");
    const left = () => worker.holds.get("toolu_sleep1")?.remainingMs;
    expect(left()).toBe(102_000);
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(left()).toBe(101_000);
    answer = snapshotWire({
      episodes: [],
      holds: [
        {
          ...WAITING_HOLD,
          remainingMs: 0,
          decision: {
            kind: "allow",
            by: "Tyler",
            at: "14:31",
            via: "app",
            reason: null,
          },
        },
      ],
    });
    await changed();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("gives a transcript nothing without a provider", () => {
    // Defect: a host outside the provider throws instead of rendering as before.
    let alarms: unknown = "unset";
    function Bare() {
      alarms = useTranscriptAlarms("51aa");
      return null;
    }
    const bare = createRoot(document.createElement("div"));
    act(() => bare.render(createElement(Bare)));
    expect(alarms).toBeUndefined();
    act(() => bare.unmount());
  });
});
