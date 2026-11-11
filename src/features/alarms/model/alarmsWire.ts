/**
 * What `invoke("alarms_view")` returns (U2 contract, "Read"), and the one
 * place it is checked. Everything past `parseAlarmsSnapshot` trusts these
 * types; a row that breaks them is dropped here, not drawn half right.
 */

import type { AlarmAgentRole, AlarmCause, AlarmDialogKind } from "./alarmViews";

export type AgentWire = {
  /** `session:<id>` or `mono:<id>`. */
  actor: string;
  name: string;
  role: AlarmAgentRole;
  /** The agent's session; a Mono's current chat. */
  sessionId: string | null;
};

export type SendNextWire =
  { kind: "agent"; agent: AgentWire } | { kind: "tyler" } | null;

export type SendWire = {
  level: number;
  /** null is Tyler. */
  recipient: AgentWire | null;
  /** "HH:MM"; null while unsent. */
  at: string | null;
  /** Level 2 and up: what the recipient one level down did not act on. */
  missed: string | null;
  next: SendNextWire;
};

export type DialogWire = {
  kind: AlarmDialogKind;
  minutes: number;
  text: string;
  /** `alarm_dialogs.request_id`: the harness's id for the dialog. */
  requestId: string | null;
};

export type ActedHowWire = "answer" | "steer" | "nudge" | "dismiss" | "message";

/** An open episode. Only a dialog wait has a dialog. */
export type EpisodeWire = {
  id: string;
  agent: AgentWire;
  level: number;
  stalledSince: string;
  minutes: number;
  openWork: string[];
  /** "13:48 Bash `cargo test` (completed)". */
  lastActivity: string | null;
  holdMissing: string | null;
  /** By level. */
  sends: SendWire[];
  nextMinutes: number | null;
  acted: { how: ActedHowWire; by: string; at: string } | null;
} & (
  | { cause: Exclude<AlarmCause, "dialog">; dialog: null }
  | { cause: "dialog"; dialog: DialogWire }
);

export type HoldDecisionWire =
  | {
      kind: "allow" | "deny";
      by: string;
      at: string;
      via: "app" | "notification";
      reason: string | null;
    }
  | { kind: "timeout" };

export type HoldWire = {
  id: string;
  sessionId: string;
  /** `Block.tool.callId` of the held tool row. */
  toolUseId: string;
  agent: AgentWire;
  command: string;
  howLong: string;
  reason: string | null;
  remainingMs: number;
  windowMs: number;
  decision: HoldDecisionWire | null;
};

export type GigaWire = { id: string; name: string };

export type AlarmSettingsWire = {
  stallMinutes: number;
  escalateMinutes: number;
  sleepHold: boolean;
  tylerSleepHold: boolean;
  shortSleepSkipSeconds: number;
  /** Always one of `gigas` once parsed. */
  rootGiga: GigaWire | null;
  gigas: GigaWire[];
};

export type StallMinutesWire = {
  actor: string;
  sessionId: string | null;
  minutes: number;
};

export type AlarmsSnapshot = {
  readWallMs: number;
  episodes: EpisodeWire[];
  holds: HoldWire[];
  settings: AlarmSettingsWire;
  stallMinutes: StallMinutesWire[];
};

type Fields = Record<string, unknown>;

class WireError extends Error {}

function fields(value: unknown, what: string): Fields {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new WireError(`${what} is not an object`);
  return value as Fields;
}

function str(value: unknown, what: string): string {
  if (typeof value !== "string") throw new WireError(`${what} is not a string`);
  return value;
}

function num(value: unknown, what: string): number {
  if (typeof value !== "number" || !Number.isFinite(value))
    throw new WireError(`${what} is not a number`);
  return value;
}

function bool(value: unknown, what: string): boolean {
  if (typeof value !== "boolean")
    throw new WireError(`${what} is not a boolean`);
  return value;
}

function nullable<T>(
  value: unknown,
  parse: (value: unknown, what: string) => T,
  what: string,
): T | null {
  return value === null || value === undefined ? null : parse(value, what);
}

function oneOf<T extends string>(
  value: unknown,
  allowed: readonly T[],
  what: string,
): T {
  if (!allowed.includes(value as T))
    throw new WireError(`${what} is not one of ${allowed.join(", ")}`);
  return value as T;
}

function list(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new WireError(`${what} is not a list`);
  return value;
}

/** The rows of `value` that parse; one bad row is dropped, not the snapshot. */
function rows<T>(
  value: unknown,
  parse: (value: unknown) => T,
  what: string,
): T[] {
  const parsed: T[] = [];
  for (const row of list(value, what)) {
    try {
      parsed.push(parse(row));
    } catch (error) {
      if (!(error instanceof WireError)) throw error;
      console.error(`alarms_view: dropped a ${what} row: ${error.message}`);
    }
  }
  return parsed;
}

const ROLES = ["worker", "lead", "mono", "giga", "session"] as const;
const CAUSES = [
  "idle_open_work",
  "dialog",
  "hung",
  "wake_missed",
  "hold_missing",
] as const;
const DIALOG_KINDS = ["permission", "question", "plan"] as const;
const ACTED = ["answer", "steer", "nudge", "dismiss", "message"] as const;
/** sessionStore keeps only these ids; any other would not bind after reload. */
const ID = /^[A-Za-z0-9_-]+$/;

function id(value: unknown, what: string): string {
  const text = str(value, what);
  if (!ID.test(text)) throw new WireError(`${what} is not an app id`);
  return text;
}

function agent(value: unknown, what = "agent"): AgentWire {
  const row = fields(value, what);
  return {
    actor: str(row.actor, `${what}.actor`),
    name: str(row.name, `${what}.name`),
    role: oneOf(row.role, ROLES, `${what}.role`),
    sessionId: nullable(row.sessionId, str, `${what}.sessionId`),
  };
}

function sendNext(value: unknown): SendNextWire {
  if (value === null || value === undefined) return null;
  const row = fields(value, "send.next");
  if (row.kind === "tyler") return { kind: "tyler" };
  if (row.kind === "agent")
    return { kind: "agent", agent: agent(row.agent, "send.next.agent") };
  throw new WireError("send.next.kind is not agent or tyler");
}

function send(value: unknown): SendWire {
  const row = fields(value, "send");
  return {
    level: num(row.level, "send.level"),
    recipient: nullable(row.recipient, agent, "send.recipient"),
    at: nullable(row.at, str, "send.at"),
    missed: nullable(row.missed, str, "send.missed"),
    next: sendNext(row.next),
  };
}

function dialog(value: unknown): DialogWire {
  const row = fields(value, "dialog");
  return {
    kind: oneOf(row.kind, DIALOG_KINDS, "dialog.kind"),
    minutes: num(row.minutes, "dialog.minutes"),
    text: str(row.text, "dialog.text"),
    requestId: nullable(row.requestId, str, "dialog.requestId"),
  };
}

function acted(value: unknown): EpisodeWire["acted"] {
  if (value === null || value === undefined) return null;
  const row = fields(value, "acted");
  return {
    how: oneOf(row.how, ACTED, "acted.how"),
    by: str(row.by, "acted.by"),
    at: str(row.at, "acted.at"),
  };
}

function episode(value: unknown): EpisodeWire {
  const row = fields(value, "episode");
  const base = {
    id: id(row.id, "episode.id"),
    agent: agent(row.agent),
    level: num(row.level, "episode.level"),
    stalledSince: str(row.stalledSince, "episode.stalledSince"),
    minutes: num(row.minutes, "episode.minutes"),
    openWork: list(row.openWork, "episode.openWork").map((line) =>
      str(line, "episode.openWork[]"),
    ),
    lastActivity: nullable(row.lastActivity, str, "episode.lastActivity"),
    holdMissing: nullable(row.holdMissing, str, "episode.holdMissing"),
    sends: list(row.sends, "episode.sends")
      .map(send)
      .sort((a, b) => a.level - b.level),
    nextMinutes: nullable(row.nextMinutes, num, "episode.nextMinutes"),
    acted: acted(row.acted),
  };
  const cause = oneOf(row.cause, CAUSES, "episode.cause");
  const shown = nullable(row.dialog, dialog, "episode.dialog");
  if (cause !== "dialog") return { ...base, cause, dialog: null };
  if (!shown) throw new WireError("a dialog episode has no dialog");
  return { ...base, cause, dialog: shown };
}

function holdDecision(value: unknown): HoldDecisionWire | null {
  if (value === null || value === undefined) return null;
  const row = fields(value, "hold.decision");
  if (row.kind === "timeout") return { kind: "timeout" };
  return {
    kind: oneOf(row.kind, ["allow", "deny"] as const, "hold.decision.kind"),
    by: str(row.by, "hold.decision.by"),
    at: str(row.at, "hold.decision.at"),
    via: oneOf(row.via, ["app", "notification"] as const, "hold.decision.via"),
    reason: nullable(row.reason, str, "hold.decision.reason"),
  };
}

function hold(value: unknown): HoldWire {
  const row = fields(value, "hold");
  return {
    id: id(row.id, "hold.id"),
    sessionId: str(row.sessionId, "hold.sessionId"),
    toolUseId: str(row.toolUseId, "hold.toolUseId"),
    agent: agent(row.agent, "hold.agent"),
    command: str(row.command, "hold.command"),
    howLong: str(row.howLong, "hold.howLong"),
    reason: nullable(row.reason, str, "hold.reason"),
    remainingMs: num(row.remainingMs, "hold.remainingMs"),
    windowMs: num(row.windowMs, "hold.windowMs"),
    decision: holdDecision(row.decision),
  };
}

function giga(value: unknown): GigaWire {
  const row = fields(value, "giga");
  return { id: str(row.id, "giga.id"), name: str(row.name, "giga.name") };
}

function settings(value: unknown): AlarmSettingsWire {
  const row = fields(value, "settings");
  const gigas = rows(row.gigas, giga, "giga");
  const named = nullable(row.rootGiga, giga, "settings.rootGiga");
  return {
    stallMinutes: num(row.stallMinutes, "settings.stallMinutes"),
    escalateMinutes: num(row.escalateMinutes, "settings.escalateMinutes"),
    sleepHold: bool(row.sleepHold, "settings.sleepHold"),
    tylerSleepHold: bool(row.tylerSleepHold, "settings.tylerSleepHold"),
    shortSleepSkipSeconds: num(
      row.shortSleepSkipSeconds,
      "settings.shortSleepSkipSeconds",
    ),
    // #543: a root Giga that is no real Giga shows as none, never as raw text.
    rootGiga: gigas.find((entry) => entry.id === named?.id) ?? null,
    gigas,
  };
}

function stallMinutesRow(value: unknown): StallMinutesWire {
  const row = fields(value, "stallMinutes");
  return {
    actor: str(row.actor, "stallMinutes.actor"),
    sessionId: nullable(row.sessionId, str, "stallMinutes.sessionId"),
    minutes: num(row.minutes, "stallMinutes.minutes"),
  };
}

/**
 * Checks `alarms_view`'s answer. Throws when the snapshot itself is not one
 * (the caller keeps what it had); drops single rows that do not parse.
 */
export function parseAlarmsSnapshot(value: unknown): AlarmsSnapshot {
  try {
    const row = fields(value, "snapshot");
    return {
      readWallMs: num(row.readWallMs, "readWallMs"),
      episodes: rows(row.episodes, episode, "episode"),
      holds: rows(row.holds, hold, "hold"),
      settings: settings(row.settings),
      stallMinutes: rows(row.stallMinutes, stallMinutesRow, "stallMinutes"),
    };
  } catch (error) {
    if (error instanceof WireError)
      throw new Error(`alarms_view: ${error.message}`);
    throw error;
  }
}

/** The session or Mono id in an actor (`session:<id>`, `mono:<id>`). */
export function actorId(
  actor: string,
  kind: "session" | "mono",
): string | undefined {
  const prefix = `${kind}:`;
  return actor.startsWith(prefix) ? actor.slice(prefix.length) : undefined;
}
