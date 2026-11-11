import { describe, expect, it, vi } from "vitest";
import {
  alarmSettingsView,
  alarmSurfaces,
  holdsCounting,
  parseLastActivity,
  sessionStallMinutes,
} from "./alarmsMap";
import { parseAlarmsSnapshot } from "./alarmsWire";
import {
  DIALOG_EPISODE,
  GIGAS,
  IDLE_EPISODE,
  READ_WALL_MS,
  WAITING_HOLD,
  snapshotWire,
} from "../test/alarmsFixtures";

const snapshot = (overrides: Record<string, unknown> = {}) =>
  parseAlarmsSnapshot(snapshotWire(overrides));

const surfaces = (
  overrides: Record<string, unknown> = {},
  questionOptions?: (sessionId: string) => readonly string[] | undefined,
) =>
  alarmSurfaces(snapshot(overrides), {
    nowWallMs: READ_WALL_MS,
    questionOptions,
  });

describe("alarms_view -> stall markers", () => {
  it("marks a stalled session by its id and a stalled Mono by its Mono id", () => {
    // Defect: a Mono's episode marks its chat session in the sidebar, or a session's marks nothing.
    const { sessionStalls, monoStalls } = surfaces();
    expect([...sessionStalls.keys()].sort()).toEqual(["77aa", "9f3c", "a71e"]);
    expect([...monoStalls.keys()]).toEqual(["fork"]);
    expect(monoStalls.get("fork")).toMatchObject({
      id: "al_M0n",
      cause: "hung",
      stalled: { name: "Fork Orchestrator", actor: "mono:fork", role: "mono" },
      minutes: 31,
      acted: { how: "nudge", by: "Monocode General", at: "13:50" },
    });
  });

  it("reads Rust's last activity line into its parts", () => {
    // Defect: the card shows the backticks, or "()" when Rust sent no state.
    expect(
      parseLastActivity(
        "13:48 Bash `cargo test -p monocode alarms` (completed)",
      ),
    ).toEqual({
      at: "13:48",
      tool: "Bash",
      command: "cargo test -p monocode alarms",
      state: "completed",
    });
    expect(parseLastActivity("13:29 output")).toEqual({
      at: "13:29",
      tool: "output",
    });
    expect(parseLastActivity("Read src/main.rs (completed)")).toEqual({
      at: "",
      tool: "Read src/main.rs",
      state: "completed",
    });
  });
});

describe("alarms_view -> alarm cards", () => {
  it("puts a level-1 card in the lead's chat only, counting down to the Mono", () => {
    // Defect: the card lands in the stalled worker's own chat, or names no next level.
    const { cards } = surfaces();
    expect(cards.get("9f3c")).toBeUndefined();
    expect(cards.get("51aa")?.get("al_7Q2")).toEqual({
      id: "al_7Q2",
      cause: "idle_open_work",
      stalled: { name: "PR S1 builder", actor: "session:9f3c", role: "worker" },
      stalledSince: "14:05",
      minutes: 17,
      openWork: ['ask m_tngc "Build S1 stall detector" (due in 12 min)'],
      lastActivity: {
        at: "13:48",
        tool: "Bash",
        command: "cargo test -p monocode alarms",
        state: "completed",
      },
      sends: [{ recipient: "PR-S1", at: "14:20", level: 1 }],
      audience: "provisioner",
      level: 1,
      next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 12 },
    });
  });

  it("escalates in the Mono's chat, naming the lead that did not act", () => {
    // Defect: level 2 shows as level 1, names the worker as the one that missed it, or counts down a past level.
    const { cards } = surfaces();
    const escalated = cards.get("fork-chat")?.get("al_9KD");
    expect(escalated).toMatchObject({
      audience: "provisioner",
      level: 2,
      escalation: {
        target: { name: "PR-S1", actor: "session:51aa", role: "lead" },
        missed: "its worker's stall",
        minutes: 15,
      },
      next: { kind: "tyler", minutes: 9 },
      sends: [
        { recipient: "PR-S1", at: "14:04", level: 1 },
        { recipient: "Fork Orchestrator", at: "14:19", level: 2 },
      ],
    });
    // The lead's own level-1 card is past: it no longer counts down.
    expect(cards.get("51aa")?.get("al_9KD")).toMatchObject({
      level: 1,
      next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 0 },
    });
  });

  it("gives a permission dialog Allow and Deny, and a question its session's choices", () => {
    // Defect: Answer offers nothing for a permission, or invents choices for a question this window cannot see.
    const { cards } = surfaces();
    expect(cards.get("fork-chat")?.get("al_9KD")?.dialog).toEqual({
      kind: "permission",
      minutes: 19,
      text: 'Allow Bash "rm -rf target"?',
      options: ["Allow", "Deny"],
    });
    const tyler = (options?: (id: string) => readonly string[] | undefined) =>
      surfaces({}, options).cards.get("51aa")?.get("al_TyL")?.dialog?.options;
    expect(tyler()).toEqual([]);
    expect(
      tyler((sessionId) =>
        sessionId === "77aa" ? ["main", "dev"] : undefined,
      ),
    ).toEqual(["main", "dev"]);
    const plan = surfaces({
      episodes: [
        {
          ...DIALOG_EPISODE,
          dialog: { ...DIALOG_EPISODE.dialog, kind: "plan", requestId: null },
        },
      ],
    });
    expect(plan.cards.get("51aa")?.get("al_9KD")?.dialog?.options).toEqual([
      "Approve",
    ]);
  });

  it("shows who acted, and keeps the act word a card knows", () => {
    // Defect: an act by message fails to map, and the card keeps counting down after someone acted.
    const { cards } = surfaces({
      episodes: [
        {
          ...IDLE_EPISODE,
          acted: { how: "message", by: "PR-S1", at: "14:22" },
        },
      ],
    });
    expect(cards.get("51aa")?.get("al_7Q2")?.acted).toEqual({
      how: "steer",
      by: "PR-S1",
      at: "14:22",
    });
  });
});

describe("alarms_view -> Tyler's landing", () => {
  it("docks only in the stalled agent's own session, with the lead to open", () => {
    // Defect: Tyler's landing docks in the lead's or the Mono's composer too.
    const { landings } = surfaces();
    expect([...landings.keys()]).toEqual(["77aa"]);
    expect(landings.get("77aa")).toMatchObject({
      id: "al_TyL",
      audience: "tyler",
      level: 3,
      lead: { name: "Fork Orchestrator", actor: "mono:fork", role: "mono" },
      sends: [
        { recipient: "PR-S1", at: "13:13", level: 1 },
        { recipient: "Fork Orchestrator", at: "13:28", level: 2 },
        { recipient: "Tyler", at: "13:43", level: 3 },
      ],
    });
  });
});

describe("alarms_view -> sleep holds", () => {
  it("puts each hold under its tool call, in its own session only", () => {
    // Defect: a hold shows in another session's transcript, or keys on the hold id instead of the tool call.
    const { holds } = surfaces();
    expect([...holds.keys()].sort()).toEqual(["51aa", "9f3c"]);
    expect([...(holds.get("9f3c")?.keys() ?? [])]).toEqual(["toolu_sleep1"]);
    expect(holds.get("9f3c")?.get("toolu_sleep1")).toEqual({
      id: "sh_K1",
      callId: "toolu_sleep1",
      agent: { name: "PR S1 builder", actor: "session:9f3c", role: "worker" },
      command: "sleep 300 && gh pr checks",
      howLong: "5 min",
      reason: "waiting for CI",
      remainingMs: 102_000,
      windowMs: 115_000,
    });
    expect(holds.get("51aa")?.get("toolu_watch2")?.decision).toEqual({
      kind: "deny",
      by: "Tyler",
      at: "14:31",
      via: "app",
    });
  });

  it("counts a waiting hold down from the read time, and stops at zero", () => {
    // Defect: the countdown stays frozen between snapshots, or runs below zero.
    const shot = snapshot({ holds: [WAITING_HOLD] });
    const at = (ms: number) =>
      alarmSurfaces(shot, { nowWallMs: READ_WALL_MS + ms })
        .holds.get("9f3c")
        ?.get("toolu_sleep1")?.remainingMs;
    expect(at(2_000)).toBe(100_000);
    expect(at(200_000)).toBe(0);
    expect(holdsCounting(shot, READ_WALL_MS + 2_000)).toBe(true);
    expect(holdsCounting(shot, READ_WALL_MS + 102_000)).toBe(false);
  });
});

describe("alarms_view -> settings", () => {
  it("names the root Giga by id, among the real Gigas", () => {
    // Defect: the page shows Rust's settings under the wrong keys.
    expect(alarmSettingsView(snapshot())).toEqual({
      stallMinutes: 15,
      rootGiga: "general",
      gigas: GIGAS,
      holdForegroundWaits: true,
      holdTylerSleeps: true,
      shortSleepSkipSeconds: 10,
    });
  });

  it("shows no root Giga when the one named is not a real Giga (#543)", () => {
    // Defect: a deleted Giga's id shows in the Root Giga select as raw text with an empty menu.
    const settings = snapshotWire().settings;
    const view = alarmSettingsView(
      snapshot({
        settings: { ...settings, rootGiga: { id: "gone", name: "Old Giga" } },
      }),
    );
    expect(view.rootGiga).toBeUndefined();
    expect(view.gigas).toEqual(GIGAS);
  });

  it("gives each session its own stall time, and the default to the rest", () => {
    // Defect: the session menu shows no Stall alarm item for a session without its own row.
    expect(sessionStallMinutes(snapshot())).toEqual({
      bySession: new Map([["9f3c", { minutes: 30, defaultMinutes: 15 }]]),
      fallback: { minutes: null, defaultMinutes: 15 },
    });
  });
});

describe("alarms_view boundary", () => {
  it("drops a row that breaks the contract and keeps the rest", () => {
    // Defect: one malformed episode blanks every alarm, or a dialog wait with no dialog reaches the card.
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const parsed = parseAlarmsSnapshot(
      snapshotWire({
        episodes: [
          { ...DIALOG_EPISODE, dialog: null },
          { ...IDLE_EPISODE, id: "al:colon" },
          IDLE_EPISODE,
        ],
      }),
    );
    expect(parsed.episodes.map((episode) => episode.id)).toEqual(["al_7Q2"]);
    expect(error).toHaveBeenCalledTimes(2);
    expect(() => parseAlarmsSnapshot({ episodes: [] })).toThrow(/readWallMs/);
    error.mockRestore();
  });
});
