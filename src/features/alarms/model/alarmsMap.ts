/**
 * Pure mappers from one `alarms_view` snapshot to what each surface shows
 * (U2 contract, "Front end mapping"). No clock reads: the caller passes the
 * wall time the hold countdowns count down from.
 */

import {
  actorId,
  type AgentWire,
  type AlarmsSnapshot,
  type DialogWire,
  type EpisodeWire,
  type HoldWire,
  type SendWire,
} from "./alarmsWire";
import type {
  AlarmActed,
  AlarmAgent,
  AlarmDialog,
  AlarmEpisodeView,
  AlarmLastActivity,
  AlarmNext,
  AlarmSend,
  AlarmSettingsView,
  ProvisionerAlarmView,
  SleepHoldView,
  StallMarkerView,
  StallMinutesView,
  TylerLandingView,
} from "./alarmViews";

export type AlarmMapOptions = {
  /** A loaded session's pending question choices, when this window has it. */
  questionOptions?: (sessionId: string) => readonly string[] | undefined;
  /** Wall clock now: a hold counts down from the snapshot's read time. */
  nowWallMs: number;
};

/** A session's menu value, and what a session with no row of its own shows. */
export type SessionStallMinutes = {
  bySession: ReadonlyMap<string, StallMinutesView>;
  fallback: StallMinutesView;
};

/** Everything the app draws from one snapshot, by the key each host reads. */
export type AlarmSurfaces = {
  /** By session id. */
  sessionStalls: ReadonlyMap<string, StallMarkerView>;
  /** By Mono id. */
  monoStalls: ReadonlyMap<string, StallMarkerView>;
  /** By transcript session, then alarm id. */
  cards: ReadonlyMap<string, ReadonlyMap<string, ProvisionerAlarmView>>;
  /** By the stalled agent's session. */
  landings: ReadonlyMap<string, TylerLandingView>;
  /** By the held agent's session, then tool_use_id. */
  holds: ReadonlyMap<string, ReadonlyMap<string, SleepHoldView>>;
  settings: AlarmSettingsView;
  stallMinutes: SessionStallMinutes;
};

export function alarmAgent(wire: AgentWire): AlarmAgent {
  return { name: wire.name, actor: wire.actor, role: wire.role };
}

const ACTIVITY = /^(?:(\d{1,2}:\d{2}) )?(.*?)(?: \(([^()]*)\))?$/;
const COMMAND = /^(.*?)\s*`([^`]+)`$/;

/** "13:48 Bash `cargo test` (completed)" -> its parts; any part may be missing. */
export function parseLastActivity(line: string): AlarmLastActivity {
  const [, at = "", middle = "", state] = ACTIVITY.exec(line.trim()) ?? [];
  const command = COMMAND.exec(middle);
  return {
    at,
    tool: command ? command[1] : middle,
    ...(command && { command: command[2] }),
    ...(state && { state }),
  };
}

const FIXED_OPTIONS: Record<
  Exclude<DialogWire["kind"], "question">,
  string[]
> = {
  permission: ["Allow", "Deny"],
  plan: ["Approve"],
};

function dialogView(
  dialog: DialogWire,
  sessionId: string | null,
  options: AlarmMapOptions,
): AlarmDialog {
  return {
    kind: dialog.kind,
    minutes: dialog.minutes,
    text: dialog.text,
    // A question's choices live in the session; without it, a text box.
    options:
      dialog.kind === "question"
        ? [...((sessionId && options.questionOptions?.(sessionId)) || [])]
        : FIXED_OPTIONS[dialog.kind],
  };
}

function sendView(send: SendWire): AlarmSend[] {
  if (send.at === null) return [];
  return [
    {
      recipient: send.recipient?.name ?? "Tyler",
      at: send.at,
      level: send.level,
    },
  ];
}

function actedView(acted: EpisodeWire["acted"]): AlarmActed | undefined {
  if (!acted) return undefined;
  // A message to the agent is the act a steer is; the card has no other word.
  const how = acted.how === "message" ? "steer" : acted.how;
  return { how, by: acted.by, at: acted.at };
}

export function episodeView(
  episode: EpisodeWire,
  options: AlarmMapOptions,
): AlarmEpisodeView {
  const acted = actedView(episode.acted);
  const base = {
    id: episode.id,
    stalled: alarmAgent(episode.agent),
    stalledSince: episode.stalledSince,
    minutes: episode.minutes,
    openWork: episode.openWork,
    ...(episode.lastActivity && {
      lastActivity: parseLastActivity(episode.lastActivity),
    }),
    sends: episode.sends.flatMap(sendView),
    ...(acted && { acted }),
    ...(episode.closed && { closed: episode.closed }),
  };
  return episode.cause === "dialog"
    ? {
        ...base,
        cause: episode.cause,
        dialog: dialogView(episode.dialog, episode.agent.sessionId, options),
      }
    : { ...base, cause: episode.cause };
}

function stallsBy(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
  key: (agent: AgentWire) => string | undefined | null,
): Map<string, StallMarkerView> {
  const stalls = new Map<string, StallMarkerView>();
  for (const episode of snapshot.episodes) {
    const id = key(episode.agent);
    if (id) stalls.set(id, episodeView(episode, options));
  }
  return stalls;
}

/** Frame 1 in the sidebar: a stalled session's episode, by its session id. */
export function sessionStalls(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
): Map<string, StallMarkerView> {
  return stallsBy(snapshot, options, (agent) =>
    actorId(agent.actor, "session") ? agent.sessionId : undefined,
  );
}

/** Frame 1 on the Mono rail: a stalled Mono's episode, by Mono id. */
export function monoStalls(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
): Map<string, StallMarkerView> {
  return stallsBy(snapshot, options, (agent) => actorId(agent.actor, "mono"));
}

function nextView(episode: EpisodeWire, send: SendWire): AlarmNext {
  // Only the level the episode is at counts down; an earlier one is past.
  const minutes = send.level === episode.level ? (episode.nextMinutes ?? 0) : 0;
  return send.next?.kind === "agent"
    ? { kind: "provisioner", name: send.next.agent.name, minutes }
    : { kind: "tyler", minutes };
}

/** Frame 2: the card a send at `send.level` puts in its recipient's chat. */
export function provisionerCard(
  snapshot: AlarmsSnapshot,
  episode: EpisodeWire,
  send: SendWire,
  options: AlarmMapOptions,
): ProvisionerAlarmView | undefined {
  const view = episodeView(episode, options);
  const card = { ...view, audience: "provisioner" as const };
  const next = nextView(episode, send);
  if (send.level <= 1) return { ...card, next, level: 1 };
  const below = episode.sends.find((entry) => entry.level === send.level - 1);
  // An escalation names who did not act; with no one below, there is no card.
  if (!below?.recipient) return undefined;
  return {
    ...card,
    next,
    level: send.level,
    escalation: {
      target: alarmAgent(below.recipient),
      missed: send.missed ?? "the stall",
      minutes: snapshot.settings.escalateMinutes,
    },
  };
}

/**
 * Frame 2, for every chat a send went to: by session, then alarm id. A
 * closed episode the read named keeps its card, in its final state.
 */
export function transcriptCards(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
): Map<string, Map<string, ProvisionerAlarmView>> {
  const bySession = new Map<string, Map<string, ProvisionerAlarmView>>();
  for (const episode of [...snapshot.episodes, ...snapshot.closed])
    // By level, so a chat told twice shows the later level.
    for (const send of episode.sends) {
      const sessionId = send.recipient?.sessionId;
      const card =
        sessionId && provisionerCard(snapshot, episode, send, options);
      if (!sessionId || !card) continue;
      const cards = bySession.get(sessionId) ?? new Map();
      bySession.set(sessionId, cards.set(episode.id, card));
    }
  return bySession;
}

/** Frame 4: Tyler's landing, in the stalled agent's own session only. */
export function tylerLandings(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
): Map<string, TylerLandingView> {
  const landings = new Map<string, TylerLandingView>();
  for (const episode of snapshot.episodes) {
    const sessionId = episode.agent.sessionId;
    const tyler = episode.sends.find((send) => send.recipient === null);
    if (!sessionId || !tyler) continue;
    const lead = episode.sends.find(
      (send) => send.level === tyler.level - 1,
    )?.recipient;
    landings.set(sessionId, {
      ...episodeView(episode, options),
      audience: "tyler",
      level: tyler.level,
      ...(lead && { lead: alarmAgent(lead) }),
    });
  }
  return landings;
}

function holdView(hold: HoldWire, snapshot: AlarmsSnapshot, nowWallMs: number) {
  const view: SleepHoldView = {
    id: hold.id,
    callId: hold.toolUseId,
    agent: alarmAgent(hold.agent),
    command: hold.command,
    howLong: hold.howLong,
    ...(hold.reason !== null && { reason: hold.reason }),
    // Display only: Rust decides the timeout on its own clock.
    remainingMs: hold.decision
      ? hold.remainingMs
      : Math.max(
          0,
          hold.remainingMs - Math.max(0, nowWallMs - snapshot.readWallMs),
        ),
    windowMs: hold.windowMs,
  };
  const decision = hold.decision;
  if (!decision) return view;
  if (decision.kind === "timeout") return { ...view, decision };
  const { reason, ...rest } = decision;
  return { ...view, decision: { ...rest, ...(reason !== null && { reason }) } };
}

/** Frame 3: each Tyler hold under its tool row, by session, then tool_use_id. */
export function sessionHolds(
  snapshot: AlarmsSnapshot,
  nowWallMs: number,
): Map<string, Map<string, SleepHoldView>> {
  const bySession = new Map<string, Map<string, SleepHoldView>>();
  for (const hold of snapshot.holds) {
    const holds = bySession.get(hold.sessionId) ?? new Map();
    holds.set(hold.toolUseId, holdView(hold, snapshot, nowWallMs));
    bySession.set(hold.sessionId, holds);
  }
  return bySession;
}

/** Frame 5: the Settings > Alarms page. */
export function alarmSettingsView(snapshot: AlarmsSnapshot): AlarmSettingsView {
  const settings = snapshot.settings;
  return {
    stallMinutes: settings.stallMinutes,
    ...(settings.rootGiga && { rootGiga: settings.rootGiga.id }),
    gigas: settings.gigas,
    holdForegroundWaits: settings.sleepHold,
    holdTylerSleeps: settings.tylerSleepHold,
    shortSleepSkipSeconds: settings.shortSleepSkipSeconds,
  };
}

/** Frame 5's session menu: each session's own stall time, by session id. */
export function sessionStallMinutes(
  snapshot: AlarmsSnapshot,
): SessionStallMinutes {
  const defaultMinutes = snapshot.settings.stallMinutes;
  const bySession = new Map<string, StallMinutesView>();
  for (const row of snapshot.stallMinutes) {
    const sessionId = row.sessionId ?? actorId(row.actor, "session");
    if (sessionId)
      bySession.set(sessionId, { minutes: row.minutes, defaultMinutes });
  }
  return { bySession, fallback: { minutes: null, defaultMinutes } };
}

export function alarmSurfaces(
  snapshot: AlarmsSnapshot,
  options: AlarmMapOptions,
): AlarmSurfaces {
  return {
    sessionStalls: sessionStalls(snapshot, options),
    monoStalls: monoStalls(snapshot, options),
    cards: transcriptCards(snapshot, options),
    landings: tylerLandings(snapshot, options),
    holds: sessionHolds(snapshot, options.nowWallMs),
    settings: alarmSettingsView(snapshot),
    stallMinutes: sessionStallMinutes(snapshot),
  };
}

/** Whether a Tyler hold still waits at `nowWallMs`, so its countdown needs a tick. */
export function holdsCounting(
  snapshot: AlarmsSnapshot,
  nowWallMs: number,
): boolean {
  const elapsed = Math.max(0, nowWallMs - snapshot.readWallMs);
  return snapshot.holds.some(
    (hold) => !hold.decision && hold.remainingMs > elapsed,
  );
}
