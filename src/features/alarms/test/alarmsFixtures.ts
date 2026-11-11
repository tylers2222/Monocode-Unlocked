/**
 * For tests: `alarms_view` answers shaped as the U2 contract's camelCase JSON,
 * as Rust would send them, before any parse.
 */

export const WORKER = {
  actor: "session:9f3c",
  name: "PR S1 builder",
  role: "worker",
  sessionId: "9f3c",
};
export const LEAD = {
  actor: "session:51aa",
  name: "PR-S1",
  role: "lead",
  sessionId: "51aa",
};
export const MONO = {
  actor: "mono:fork",
  name: "Fork Orchestrator",
  role: "mono",
  sessionId: "fork-chat",
};
export const GIGA = {
  actor: "mono:general",
  name: "Monocode General",
  role: "giga",
  sessionId: "general-chat",
};
export const DIALOG_WORKER = {
  actor: "session:a71e",
  name: "PR S2 reviewer",
  role: "worker",
  sessionId: "a71e",
};
export const TYLER_WORKER = {
  actor: "session:77aa",
  name: "U2 builder",
  role: "session",
  sessionId: "77aa",
};

/** Level 1: the worker's lead was told, and the Mono is next in 12 min. */
export const IDLE_EPISODE = {
  id: "al_7Q2",
  agent: WORKER,
  cause: "idle_open_work",
  level: 1,
  stalledSince: "14:05",
  minutes: 17,
  openWork: ['ask m_tngc "Build S1 stall detector" (due in 12 min)'],
  lastActivity: "13:48 Bash `cargo test -p monocode alarms` (completed)",
  dialog: null,
  holdMissing: null,
  sends: [
    {
      level: 1,
      recipient: LEAD,
      at: "14:20",
      missed: null,
      next: { kind: "agent", agent: MONO },
    },
  ],
  nextMinutes: 12,
  acted: null,
};

/** Level 2 on a permission dialog: the lead did not act, so the Mono was told. */
export const DIALOG_EPISODE = {
  id: "al_9KD",
  agent: DIALOG_WORKER,
  cause: "dialog",
  level: 2,
  stalledSince: "14:01",
  minutes: 19,
  openWork: [],
  lastActivity: "14:16 Bash `rm -rf target` (waiting for permission)",
  dialog: {
    kind: "permission",
    minutes: 19,
    text: 'Allow Bash "rm -rf target"?',
    requestId: "toolu_01RmRf",
  },
  holdMissing: null,
  sends: [
    {
      level: 2,
      recipient: MONO,
      at: "14:19",
      missed: "its worker's stall",
      next: { kind: "tyler" },
    },
    {
      level: 1,
      recipient: LEAD,
      at: "14:04",
      missed: null,
      next: { kind: "agent", agent: MONO },
    },
  ],
  nextMinutes: 9,
  acted: null,
};

/** A Mono stalled; its Giga was told. */
export const MONO_EPISODE = {
  id: "al_M0n",
  agent: MONO,
  cause: "hung",
  level: 1,
  stalledSince: "13:30",
  minutes: 31,
  openWork: ["giga ask g_51 (due in 4 min)"],
  lastActivity: "13:29 output",
  dialog: null,
  holdMissing: null,
  sends: [
    {
      level: 1,
      recipient: GIGA,
      at: "13:45",
      missed: null,
      next: { kind: "tyler" },
    },
  ],
  nextMinutes: 0,
  acted: { how: "nudge", by: "Monocode General", at: "13:50" },
};

/** The last level: Tyler, after the lead and the Mono. */
export const TYLER_EPISODE = {
  id: "al_TyL",
  agent: TYLER_WORKER,
  cause: "dialog",
  level: 3,
  stalledSince: "13:10",
  minutes: 50,
  openWork: [],
  lastActivity: null,
  dialog: {
    kind: "question",
    minutes: 50,
    text: "Which branch should I base on?",
    requestId: "q-77aa-4",
  },
  holdMissing: null,
  sends: [
    {
      level: 1,
      recipient: LEAD,
      at: "13:13",
      missed: null,
      next: { kind: "agent", agent: MONO },
    },
    {
      level: 2,
      recipient: MONO,
      at: "13:28",
      missed: "its worker's stall",
      next: { kind: "tyler" },
    },
    {
      level: 3,
      recipient: null,
      at: "13:43",
      missed: "its lead's alarm",
      next: null,
    },
  ],
  nextMinutes: null,
  acted: null,
};

export const WAITING_HOLD = {
  id: "sh_K1",
  sessionId: "9f3c",
  toolUseId: "toolu_sleep1",
  agent: WORKER,
  command: "sleep 300 && gh pr checks",
  howLong: "5 min",
  reason: "waiting for CI",
  remainingMs: 102_000,
  windowMs: 115_000,
  decision: null,
};

export const DECIDED_HOLD = {
  id: "sh_K2",
  sessionId: "51aa",
  toolUseId: "toolu_watch2",
  agent: LEAD,
  command: "gh run watch",
  howLong: "until checks finish (no bound)",
  reason: null,
  remainingMs: 0,
  windowMs: 115_000,
  decision: {
    kind: "deny",
    by: "Tyler",
    at: "14:31",
    via: "app",
    reason: null,
  },
};

export const GIGAS = [
  { id: "general", name: "Monocode General" },
  { id: "fork-orch", name: "Fork Orchestrator" },
];

export const READ_WALL_MS = Date.UTC(2026, 10, 8, 3, 30);

export function snapshotWire(overrides: Record<string, unknown> = {}) {
  return {
    readWallMs: READ_WALL_MS,
    episodes: [IDLE_EPISODE, DIALOG_EPISODE, MONO_EPISODE, TYLER_EPISODE],
    holds: [WAITING_HOLD, DECIDED_HOLD],
    settings: {
      stallMinutes: 15,
      escalateMinutes: 15,
      sleepHold: true,
      tylerSleepHold: true,
      shortSleepSkipSeconds: 10,
      rootGiga: GIGAS[0],
      gigas: GIGAS,
    },
    stallMinutes: [{ actor: "session:9f3c", sessionId: "9f3c", minutes: 30 }],
    ...overrides,
  };
}
