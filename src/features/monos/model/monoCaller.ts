import { invoke } from "@tauri-apps/api/core";
import type { AgentAppHost } from "../../agent-app/model/agentApp";
import {
  AppActionError,
  type AppErrorCode,
  type ToolErrorDetails,
} from "../../agent-app/model/appActionError";
import { operatorEnabledInThread } from "../../sessions/model/operatorCommand";
import type { Session } from "../../sessions/model/session";
import {
  findMono,
  monoForSession,
  monoRosterPersisted,
  monoWritesSettled,
} from "./mono";
import { habitRunMono } from "./monoHabits";
import {
  parseMessageId,
  parseMonoId,
  parseSessionId,
  parseTurnId,
  type Caller,
  type MessageId,
  type MonoId,
  type RejectCode,
  type ToolError,
  type TurnId,
} from "./monoMessages";

/** What a caller needs from the session a call comes from. */
export type CallingSession = Pick<
  Session,
  "id" | "blocks" | "cwd" | "leadSessionId"
>;

/**
 * The Mono a session speaks for: its chat, or one of its habit runs. This
 * decides who gets the messaging tools at spawn time and who may call them.
 */
export function callerMono(
  sessionId: string,
): Pick<Extract<Caller, { monoId: MonoId }>, "kind" | "monoId"> | undefined {
  const chat = monoForSession(sessionId);
  const mono = chat ?? findMono(habitRunMono(sessionId) ?? "");
  const monoId = parseMonoId(mono?.id);
  if (!monoId) return undefined;
  return { kind: chat ? "mono_chat" : "habit_run", monoId };
}

const TURN_INACTIVE = {
  ok: false,
  code: "turn_inactive",
  hint: "MonoCode app access is inactive. Use /operator once in this thread to enable it, then call the CLI during an active agent turn. Retrying this request now will not enable access.",
} as const satisfies ToolError;

/**
 * A Mono's chat or habit run, or (design 13.4) a lead or worker session
 * with /operator access. Undefined for any other session.
 */
function callerOf(session: CallingSession, turnId: TurnId): Caller | undefined {
  const sessionId = parseSessionId(session.id);
  if (!sessionId) return undefined;
  const mono = callerMono(sessionId);
  if (mono) return { ...mono, sessionId, turnId };
  if (!operatorEnabledInThread(session.blocks)) return undefined;
  return {
    kind: "session",
    sessionId,
    turnId,
    access: "operator",
    leadSessionId: parseSessionId(session.leadSessionId) ?? null,
    cwd: session.cwd,
  };
}

/**
 * The caller of a messaging call. Rust reads roles and projects from
 * `monos`, so the roster must be the database's and every write this window
 * queued must have landed first.
 */
export async function resolveCaller(
  session: CallingSession,
  turnId: string | null,
): Promise<{ ok: true; caller: Caller } | ToolError> {
  const turn = parseTurnId(turnId);
  if (!turn) return TURN_INACTIVE;
  if (!monoRosterPersisted())
    return {
      ok: false,
      code: "roster_unavailable",
      hint: "MonoCode could not load the Mono list from its database. Mono messaging is off until the app restarts. Use a new request id when you retry.",
    };
  await monoWritesSettled();
  const caller = callerOf(session, turn);
  if (!caller)
    return {
      ok: false,
      code: "not_a_mono",
      hint: "Only a Mono's chat, its habit runs or a session with MonoCode app access can use Mono messaging.",
    };
  return { ok: true, caller };
}

/** What `mono_app_action_check` answers (Rust `ActionChecked`). */
export type ActionChecked =
  | { outcome: "allowed"; leadSessionId: string | null }
  | {
      outcome: "failed";
      error: { code: RejectCode | AppErrorCode; hint: string } & ToolErrorDetails;
    };

type Failed = Extract<ActionChecked, { outcome: "failed" }>;

/** A `failed` reply's error, parsed; undefined for any other shape. */
function parseFailed(error: unknown): Failed | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  const { code, hint } = error as Record<string, unknown>;
  if (typeof code !== "string" || typeof hint !== "string") return undefined;
  return {
    outcome: "failed",
    error: {
      ...(error as ToolErrorDetails),
      code: code as RejectCode | AppErrorCode,
      hint,
    },
  };
}

/** The reply of `mono_app_action_check`, parsed; undefined for any other shape. */
function parseActionChecked(reply: unknown): ActionChecked | undefined {
  if (typeof reply !== "object" || reply === null) return undefined;
  const { outcome, leadSessionId, error } = reply as Record<string, unknown>;
  if (outcome === "allowed") {
    const lead = leadSessionId === null ? null : parseSessionId(leadSessionId);
    return lead === undefined ? undefined : { outcome, leadSessionId: lead };
  }
  return outcome === "failed" ? parseFailed(error) : undefined;
}

/** The reply of `mono_message_record_start` (Rust `StartRecorded`), parsed. */
function parseStartRecorded(
  reply: unknown,
): { outcome: "recorded"; messageId: MessageId } | Failed | undefined {
  if (typeof reply !== "object" || reply === null) return undefined;
  const { outcome, messageId, error } = reply as Record<string, unknown>;
  if (outcome === "recorded") {
    const id = parseMessageId(messageId);
    return id && { outcome, messageId: id };
  }
  return outcome === "failed" ? parseFailed(error) : undefined;
}

/**
 * A parsed answer of a level gate command. Throws its refusal, and throws
 * `unknown` for a reply this window cannot read, which never allows anything.
 */
function answered<T extends { outcome: "allowed" | "recorded" }>(
  answer: T | Failed | undefined,
  unknown: string,
): T {
  if (!answer) throw new Error(unknown);
  if (answer.outcome === "failed") {
    const { code, hint, ...details } = answer.error;
    throw new AppActionError(code, hint, details);
  }
  return answer;
}

/**
 * Design 6.6: the level gate for the app calls of `session` in turn
 * `turnId`. Unlike `resolveCaller` it needs no persisted roster: Rust reads
 * only lead ids, and writes a Mono caller's id through a subselect.
 */
export function levelGate(
  session: CallingSession,
  turnId: string | null,
  /** This window's copy of a session's lead; Rust reads it only for a session it has no row of. */
  leadOf: (sessionId: string) => string | undefined,
): AgentAppHost["levels"] {
  const caller = (): Caller => {
    const turn = parseTurnId(turnId);
    const resolved = turn ? callerOf(session, turn) : undefined;
    if (!resolved)
      throw new AppActionError(TURN_INACTIVE.code, TURN_INACTIVE.hint);
    return resolved;
  };
  return {
    async check(requestId, { action, target, startedSessionId, input }) {
      const call = {
        caller: caller(),
        action,
        target,
        targetLeadSessionId: target === null ? null : (leadOf(target) ?? null),
        startedSessionId,
        requestId,
        input,
      };
      const answer = answered(
        parseActionChecked(await invoke("mono_app_action_check", { call })),
        "The level check answered in an unknown form, so the action did not run.",
      );
      return { leadSessionId: answer.leadSessionId };
    },
    async recordStart(requestId, sessionId, input) {
      const call = { caller: caller(), sessionId, requestId, input };
      return answered(
        parseStartRecorded(await invoke("mono_message_record_start", { call })),
        "The start row answered in an unknown form",
      ).messageId;
    },
  };
}
