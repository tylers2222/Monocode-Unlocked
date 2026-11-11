/**
 * What one click on an alarm or hold card does (U2 contract, "Writes"). A
 * provisioner's card acts as its chat's owner through `control_alarm_act`;
 * Tyler's landing does the effect in this window and logs it with
 * `alarms_tyler_act`; a hold card answers with `holds_tyler_decide`.
 */

import type { EpisodeWire } from "./alarmsWire";
import type { AlarmActEvent } from "./alarmViews";

export type AlarmActHost = {
  invoke: (command: string, args: Record<string, unknown>) => Promise<unknown>;
  /** Opens a session, or a Mono's chat, by its actor (`session:<id>`, `mono:<id>`). */
  openAgent: (actor: string) => void;
  /**
   * Answers `sessionId`'s pending dialog as the user does
   * (`answerAlarmDialog`); throws when that dialog is gone.
   */
  answerDialog: (sessionId: string, input: Record<string, unknown>) => void;
  /** Sends `text` into `sessionId` as the user does, steering a busy turn. */
  sendAsUser: (sessionId: string, text: string) => void;
  /** Says an act was refused or failed. */
  refused: (what: string, detail: unknown) => void;
  /** A fresh id per click, `[A-Za-z0-9_-]` only. */
  requestId: () => string;
};

type CardEvent = Extract<AlarmActEvent, { kind: "alarm" }>;
type HoldEvent = Extract<AlarmActEvent, { kind: "hold" }>;
type Answer = Extract<CardEvent, { act: "answer" }>["answer"];

export function freshRequestId(): string {
  return `ui-${crypto.randomUUID()}`;
}

/** Rust's own answer to a plan (act.rs `answer_plan`): a new message. */
export const PLAN_APPROVED = "Your plan is approved. Implement it.";

/** The result of a call that came back `{ok: true}`, else undefined after saying so. */
async function call(
  host: AlarmActHost,
  command: string,
  args: Record<string, unknown>,
): Promise<Record<string, unknown> | undefined> {
  try {
    const result = await host.invoke(command, args);
    if (
      typeof result === "object" &&
      result !== null &&
      "ok" in result &&
      result.ok === true
    )
      return "result" in result &&
        typeof result.result === "object" &&
        result.result !== null
        ? { ...result.result }
        : {};
    host.refused(command, result);
  } catch (error) {
    host.refused(command, error);
  }
  return undefined;
}

/** A permission's Allow / Deny, or a plan's Approve; undefined for anything else. */
function decisionOf(answer: Answer): "allow" | "deny" | undefined {
  if (answer.kind !== "option") return undefined;
  if (answer.option === "Deny") return "deny";
  return answer.option === "Allow" || answer.option === "Approve"
    ? "allow"
    : undefined;
}

/** `alarms.answer`'s one answer field for the card's pick. */
function answerFields(
  dialog: EpisodeWire["dialog"],
  answer: Answer,
): Record<string, unknown> | undefined {
  if (answer.kind === "text") return { text: answer.text };
  if (dialog?.kind === "question") return { choice: answer.option };
  const decision = decisionOf(answer);
  return decision && { [decision]: true };
}

function cardInput(
  episode: EpisodeWire | undefined,
  event: CardEvent,
): Record<string, unknown> | undefined {
  const alarm = event.alarmId;
  switch (event.act) {
    case "read":
    case "nudge":
      return { alarm };
    case "steer":
      return { alarm, text: event.text };
    case "dismiss":
      return { alarm, reason: event.text };
    case "answer": {
      const fields = answerFields(episode?.dialog ?? null, event.answer);
      return fields && { alarm, ...fields };
    }
  }
}

/**
 * Frame 2: an act on the card in chat `sessionId`, as that chat's owner.
 * Read also opens the stalled agent.
 */
export async function provisionerAct(
  host: AlarmActHost,
  sessionId: string,
  episode: EpisodeWire | undefined,
  event: CardEvent,
): Promise<void> {
  const input = cardInput(episode, event);
  if (!input) {
    host.refused(`alarms.${event.act}`, event);
    return;
  }
  await call(host, "control_alarm_act", {
    sessionId,
    action: `alarms.${event.act}`,
    input,
    requestId: host.requestId(),
  });
  if (event.act === "read") host.openAgent(event.target.actor);
}

/** Answers the stalled agent's harness dialog here; the detail to log, or undefined. */
function answerHere(
  host: AlarmActHost,
  sessionId: string,
  dialog: NonNullable<EpisodeWire["dialog"]>,
  answer: Answer,
): string | undefined {
  const bound = { dialogRequestId: dialog.requestId };
  if (dialog.kind === "question") {
    const reply =
      answer.kind === "text"
        ? { text: answer.text }
        : { choice: answer.option };
    host.answerDialog(sessionId, { kind: "question", ...bound, ...reply });
    return answer.kind === "text" ? answer.text : answer.option;
  }
  const decision = decisionOf(answer);
  if (!decision) return undefined;
  host.answerDialog(sessionId, { kind: "permission", ...bound, decision });
  return decision;
}

/**
 * A plan has no harness dialog: Rust marks it answered on the log, and only
 * then does the agent get Rust's own approval line, as the user would type it.
 */
async function approvePlan(
  host: AlarmActHost,
  episode: EpisodeWire,
  sessionId: string,
  answer: Answer,
): Promise<void> {
  if (decisionOf(answer) !== "allow") {
    host.refused("alarms.answer", answer);
    return;
  }
  const logged = await call(host, "alarms_tyler_act", {
    alarm: episode.id,
    how: "answer",
    detail: "allow",
    requestId: host.requestId(),
  });
  if (logged) host.sendAsUser(sessionId, PLAN_APPROVED);
}

async function tylerAnswer(
  host: AlarmActHost,
  episode: EpisodeWire,
  sessionId: string,
  answer: Answer,
): Promise<void> {
  if (episode.dialog?.kind === "plan")
    return approvePlan(host, episode, sessionId, answer);
  let detail: string | undefined;
  try {
    detail = episode.dialog
      ? answerHere(host, sessionId, episode.dialog, answer)
      : undefined;
  } catch (error) {
    host.refused("alarms.answer", error);
    return;
  }
  if (detail === undefined) {
    host.refused("alarms.answer", answer);
    return;
  }
  // Effect, then log.
  await call(host, "alarms_tyler_act", {
    alarm: episode.id,
    how: "answer",
    detail,
    requestId: host.requestId(),
  });
}

/**
 * Frame 4: Tyler acts on the stalled agent from its own composer. Answer
 * answers here, then logs; Steer and Nudge log, then send as the user would.
 */
export async function tylerAct(
  host: AlarmActHost,
  episode: EpisodeWire,
  event: CardEvent,
): Promise<void> {
  const sessionId = episode.agent.sessionId;
  if (!sessionId || event.act === "read") {
    host.refused(`alarms.${event.act}`, event);
    return;
  }
  if (event.act === "answer")
    return tylerAnswer(host, episode, sessionId, event.answer);
  const detail = "text" in event ? event.text : null;
  const logged = await call(host, "alarms_tyler_act", {
    alarm: episode.id,
    how: event.act,
    detail,
    requestId: host.requestId(),
  });
  if (!logged || event.act === "dismiss") return;
  const text = event.act === "steer" ? event.text : logged.prompt;
  if (typeof text === "string" && text.trim()) host.sendAsUser(sessionId, text);
  else host.refused("alarms.nudge", logged);
}

/** Frame 3: Tyler's Allow or Deny on a hold card. */
export async function holdAct(
  host: AlarmActHost,
  event: HoldEvent,
): Promise<void> {
  await call(host, "holds_tyler_decide", {
    hold: event.holdId,
    allow: event.act === "allow",
    reason: null,
    requestId: host.requestId(),
  });
}
