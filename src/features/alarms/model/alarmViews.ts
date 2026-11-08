/**
 * What the alarm surfaces show, as props (alarms design section 8). Rust owns
 * every fact and every timer, so minutes and countdowns arrive here already
 * counted on the active clock; nothing in this file reads the time.
 */

/** `alarm_episodes.cause`. */
export type AlarmCause =
  "idle_open_work" | "dialog" | "hung" | "wake_missed" | "hold_missing";

/** `agent_activity.dialog_kind`. */
export type AlarmDialogKind = "permission" | "question" | "plan";

export type AlarmAgentRole = "worker" | "lead" | "mono" | "giga" | "session";

export type AlarmAgent = {
  name: string;
  /** `session:9f3c`, `mono:<id>`. */
  actor: string;
  role: AlarmAgentRole;
};

/** `alarm_episodes.last_activity`: "13:48 Bash `cargo test` (completed)". */
export type AlarmLastActivity = {
  /** Wall clock, "13:48". */
  at: string;
  tool: string;
  command: string;
  /** "completed", "waiting for permission". */
  state: string;
};

export type AlarmDialog = {
  kind: AlarmDialogKind;
  minutes: number;
  /** `alarm_episodes.dialog_text`: `Allow Bash "rm -rf target"?`. */
  text: string;
  /** The dialog's own answers, first one primary: Allow / Deny, or a question's choices. */
  options: string[];
};

/** One `alarm_sends` row: who was told, and when. */
export type AlarmSend = {
  recipient: string;
  /** Wall clock, "14:20". */
  at: string;
  level: number;
};

export type AlarmActKind = "read" | "answer" | "steer" | "nudge" | "dismiss";

/** `acted_by` / `acted_how` once someone acted; escalation stopped. */
export type AlarmActed = {
  how: AlarmActKind;
  by: string;
  /** Wall clock, "14:21". */
  at: string;
};

/** The open `alarm_episodes` row for one agent. Only a dialog wait has a dialog. */
export type AlarmEpisodeView = {
  /** `al_7Q2`. */
  id: string;
  stalled: AlarmAgent;
  /** `stalled_since_wall_ms` as a wall clock, "14:05". */
  stalledSince: string;
  /** Awake minutes since `stalled_since`. */
  minutes: number;
  openWork: string[];
  lastActivity?: AlarmLastActivity;
  sends: AlarmSend[];
  /** The episode stays open after an act until the agent moves. */
  acted?: AlarmActed;
} & (
  | { cause: Exclude<AlarmCause, "dialog">; dialog?: undefined }
  | { cause: "dialog"; dialog: AlarmDialog }
);

/** A sidebar row's marker reads the same episode. */
export type StallMarkerView = AlarmEpisodeView;

/**
 * When a provisioner's card goes up a level (`next_escalate_active_ms`).
 * Tyler is always the last level, so a provisioner's card always has a next.
 */
export type AlarmNext =
  | { kind: "provisioner"; name: string; minutes: number }
  | { kind: "tyler"; minutes: number };

/** Level 2 and up: the recipient one level down that did not act. */
export type AlarmEscalation = {
  target: AlarmAgent;
  /** What it did not act on: "its worker's stall". */
  missed: string;
  minutes: number;
};

/** Frame 2: a delivered `<mono_alarm>` in a provisioner's chat. */
export type ProvisionerAlarmView = AlarmEpisodeView & {
  audience: "provisioner";
  next: AlarmNext;
} & (
    | { level: 1; escalation?: undefined }
    | {
        /** 2 and up. */
        level: number;
        escalation: AlarmEscalation;
      }
  );

/** Frame 4: Tyler's landing in the stalled session; the last level. */
export type TylerLandingView = AlarmEpisodeView & {
  audience: "tyler";
  level: number;
  /** The lead whose turn it was, for the Open button. */
  lead?: AlarmAgent;
};

export type AlarmCardView = ProvisionerAlarmView | TylerLandingView;

/**
 * Who an act lands on. Read and Answer reach the stalled agent, whose dialog it
 * is; the others at level 2 reach the lead that did not act.
 */
export function actTarget(view: AlarmCardView, act: AlarmActKind): AlarmAgent {
  if (act === "read" || act === "answer") return view.stalled;
  return view.audience === "provisioner" && view.escalation
    ? view.escalation.target
    : view.stalled;
}

/** What one click on an alarm or a hold card asks the app to do (design section 7). */
export type AlarmActEvent =
  | (AlarmActOn & { act: "read" | "nudge" })
  | (AlarmActOn & {
      act: "steer";
      /** The one-line Steer box. */
      text: string;
    })
  | (AlarmActOn & {
      act: "dismiss";
      /** The reason Dismiss asks for. */
      text: string;
    })
  | (AlarmActOn & {
      act: "answer";
      /** One of the dialog's own options, or the text box of a question without any. */
      answer:
        { kind: "option"; option: string } | { kind: "text"; text: string };
    })
  | {
      kind: "hold";
      /** `sh_K1`. */
      holdId: string;
      /** `sleep_holds.tool_use_id`, unique per hold. */
      toolUseId: string;
      act: "allow" | "deny";
      target: AlarmAgent;
    };

type AlarmActOn = { kind: "alarm"; alarmId: string; target: AlarmAgent };

/** `sleep_holds.decision` once it has one. */
export type SleepHoldDecision =
  | {
      kind: "allow" | "deny";
      by: string;
      /** Wall clock, "14:31". */
      at: string;
      via: "app" | "notification";
      reason?: string;
    }
  | { kind: "timeout" };

export type SleepHoldView = {
  /** `sh_K1`. */
  id: string;
  /** `sleep_holds.tool_use_id`: the tool row the card sits under. */
  callId: string;
  agent: AlarmAgent;
  command: string;
  /** From `wait_kind` and `wait_seconds`: "until checks finish (no bound)". */
  howLong: string;
  reason?: string;
  /** Awake time left before the hold refuses itself. */
  remainingMs: number;
  windowMs: number;
  decision?: SleepHoldDecision;
};

/** `alarm_settings` and the per-agent `agent_alarm_settings.stall_minutes`. */
export type AlarmSettingsView = {
  stallMinutes: number;
  rootGiga: string;
  gigas: string[];
  holdForegroundWaits: boolean;
  holdTylerSleeps: boolean;
  shortSleepSkipSeconds: number;
};

export const ALARM_SETTINGS_DEFAULTS: Omit<AlarmSettingsView, "gigas"> = {
  stallMinutes: 15,
  rootGiga: "Monocode General",
  holdForegroundWaits: true,
  holdTylerSleeps: true,
  shortSleepSkipSeconds: 10,
};

/** A session's own stall time; `minutes` null follows Settings. */
export type StallMinutesView = {
  minutes: number | null;
  defaultMinutes: number;
  /** A lead that set it with `alarms.set`. */
  setBy?: string;
};

/** 5 to 240 minutes (design section 3). */
export const STALL_MINUTE_PRESETS = [5, 15, 30, 60, 120, 240] as const;

/** The presets, plus a value set between them (a lead's `alarms.set`), in order. */
export function stallMinuteChoices(current?: number | null): number[] {
  const choices: number[] = [...STALL_MINUTE_PRESETS];
  if (current != null && !choices.includes(current)) choices.push(current);
  return choices.sort((a, b) => a - b);
}

export function stallMinutesLabel(minutes: number): string {
  if (minutes < 60 || minutes % 60 !== 0) return `${minutes} min`;
  const hours = minutes / 60;
  return hours === 1 ? "1 hour" : `${hours} hours`;
}

const ROLE_WORD: Record<AlarmAgentRole, string> = {
  worker: "Worker",
  lead: "Lead",
  mono: "Mono",
  giga: "Giga",
  session: "Session",
};

export function roleWord(role: AlarmAgentRole): string {
  return ROLE_WORD[role];
}

const CAUSE_CLAUSE: Record<AlarmCause, string> = {
  idle_open_work: "no turn",
  dialog: "dialog open",
  hung: "no progress",
  wake_missed: "message not taken",
  hold_missing: "sleep hold missing",
};

/** "since 14:05 (17 min, no turn)". */
export function stalledSpan(episode: AlarmEpisodeView): string {
  return `since ${episode.stalledSince} (${episode.minutes} min, ${CAUSE_CLAUSE[episode.cause]})`;
}

export function agentLabel(agent: AlarmAgent): string {
  return `${agent.name} (${agent.actor})`;
}

/** The word a card or chip leads with. */
export function statusWord(cause: AlarmCause): string {
  return cause === "dialog" ? "Waiting on dialog" : "Stalled";
}

/** The sidebar chip: "Stalled 17m", "Waiting on dialog 4m". */
export function stallChipLabel(marker: StallMarkerView): string {
  return `${statusWord(marker.cause)} ${marker.minutes}m`;
}

const DIALOG_WORD: Record<AlarmDialogKind, string> = {
  permission: "Permission",
  question: "Question",
  plan: "Plan",
};

/** The hover card's Cause row. */
export function causeLine(marker: StallMarkerView): string {
  switch (marker.cause) {
    case "idle_open_work":
      return `No turn for ${marker.minutes} min, open work`;
    case "dialog":
      return `${DIALOG_WORD[marker.dialog.kind]} dialog open ${marker.minutes} min`;
    case "hung":
      return `No progress for ${marker.minutes} min in a running turn`;
    case "wake_missed":
      return `Message not taken for ${marker.minutes} min`;
    case "hold_missing":
      return "Sleep hold missing";
  }
}

/** "Dialog (permission, 4 min): Allow Bash "rm -rf target"?" or "none". */
export function dialogLine(dialog: AlarmDialog | undefined): string {
  return dialog
    ? `Dialog (${dialog.kind}, ${dialog.minutes} min): ${dialog.text}`
    : "none";
}

export function lastActivityText(activity: AlarmLastActivity): string {
  return `${activity.at} ${activity.tool} ${activity.command} (${activity.state})`;
}

export function countdownText(view: AlarmCardView): string {
  if (view.audience === "tyler")
    return "Last level · it does not escalate further";
  const next = view.next;
  return next.kind === "provisioner"
    ? `Goes to ${next.name} in ${next.minutes} min`
    : `Notifies Tyler in ${next.minutes} min`;
}

const ACTED_WORD: Record<AlarmActKind, { chip: string; verb: string }> = {
  read: { chip: "Read", verb: "read the alarm for" },
  answer: { chip: "Answered", verb: "answered the dialog for" },
  steer: { chip: "Steered", verb: "steered" },
  nudge: { chip: "Nudged", verb: "nudged" },
  dismiss: { chip: "Dismissed", verb: "dismissed the alarm for" },
};

/** The line that replaces the countdown once someone acts. */
export function actedLine(
  acted: AlarmActed,
  target: AlarmAgent,
): { chip: string; text: string } {
  const word = ACTED_WORD[acted.how];
  return {
    chip: word.chip,
    text: `${acted.by} ${word.verb} ${target.name} at ${acted.at} · escalation stopped`,
  };
}

/** The sidebar hover card's Told row: who was told, then the act or "no act yet". */
export function toldLine(episode: AlarmEpisodeView): string {
  const told = episode.sends.map((send) => `${send.recipient} at ${send.at}`);
  const acted = episode.acted;
  if (acted)
    told.push(
      `${ACTED_WORD[acted.how].chip.toLowerCase()} by ${acted.by} at ${acted.at}`,
    );
  else if (told.length > 0) told.push("no act yet");
  return told.length > 0 ? told.join(" · ") : "No one yet";
}

/** "1:42" for the hold card's countdown. */
export function holdClock(remainingMs: number): string {
  const seconds = Math.max(0, Math.ceil(remainingMs / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** "2 min": how long a hold waits for an answer. */
export function holdWindowText(windowMs: number): string {
  return `${Math.round(windowMs / 60_000)} min`;
}

/** The countdown bar's width, 0 to 1. */
export function holdFraction(view: SleepHoldView): number {
  if (view.windowMs <= 0) return 0;
  return Math.min(1, Math.max(0, view.remainingMs / view.windowMs));
}

const MONO_ALARM = /^\s*<mono_alarm\b[^>]*\balarm="([^"]+)"/;
/**
 * S2's outbox key `alarm-<episode>-<level>`, carried as the request id. Dashes,
 * since sessionStore drops request ids outside [A-Za-z0-9_-] on save and load.
 */
const ALARM_REQUEST = /^alarm-(al_[A-Za-z0-9]+)-\d+$/;

/**
 * The episode id of a `<mono_alarm>` the app delivered. Like the U10 notices,
 * the request id must be the app's own outbox key for that episode, so a
 * quoted or pasted tag stays a plain message.
 */
export function monoAlarmId(item: {
  appRequestId?: string;
  text: string;
}): string | undefined {
  const delivered = ALARM_REQUEST.exec(item.appRequestId ?? "")?.[1];
  const tagged = MONO_ALARM.exec(item.text)?.[1];
  return delivered && delivered === tagged ? delivered : undefined;
}
