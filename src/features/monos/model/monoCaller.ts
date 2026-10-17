import { invoke } from "@tauri-apps/api/core";
import type { AgentAppHost } from "../../agent-app/model/agentApp";
import {
  AppActionError,
  type ToolErrorDetails,
} from "../../agent-app/model/appActionError";
import { appAccess } from "../../sessions/model/operatorCommand";
import type { Session } from "../../sessions/model/session";
import {
  findMono,
  monoForSession,
  monoRosterPersisted,
  monoWritesSettled,
} from "./mono";
import { habitRunMono } from "./monoHabits";
import {
  parseMonoId,
  parseSessionId,
  parseTurnId,
  type CallErrorCode,
  type Caller,
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

/** The Mono a session speaks for: its chat, or one of its habit runs. */
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
 * with app access. Undefined for any other session.
 */
function callerOf(session: CallingSession, turnId: TurnId): Caller | undefined {
  const sessionId = parseSessionId(session.id);
  if (!sessionId) return undefined;
  const mono = callerMono(sessionId);
  if (mono) return { ...mono, sessionId, turnId };
  const access = appAccess(session.blocks);
  if (!access) return undefined;
  return {
    kind: "session",
    sessionId,
    turnId,
    access,
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

/** How the level gate's commands answer a call they refuse. */
type Failed = {
  outcome: "failed";
  error: { code: RejectCode | CallErrorCode; hint: string } & ToolErrorDetails;
};

/**
 * Design 6.6: the level gate's commands for the app calls of `session` in
 * turn `turnId`. Unlike `resolveCaller` they need no roster: the gate reads
 * only lead ids, and writes a Mono caller's id through a subselect, so a
 * Mono can start sessions while `roster_unavailable` holds.
 */
export function levelGate(
  session: CallingSession,
  turnId: string | null,
  /** This window's copy of a session's lead; Rust reads it only for a session it has no row of. */
  leadOf: (sessionId: string) => string | undefined,
): NonNullable<AgentAppHost["levels"]> {
  const caller = (): Caller => {
    const turn = parseTurnId(turnId);
    const resolved = turn ? callerOf(session, turn) : undefined;
    if (!resolved)
      throw new AppActionError(TURN_INACTIVE.code, TURN_INACTIVE.hint);
    return resolved;
  };
  return {
    async check(requestId, { action, target, input }) {
      const answer = await invoke<
        { outcome: "allowed"; leadSessionId: string | null } | Failed
      >("mono_app_action_check", {
        caller: caller(),
        action,
        target,
        targetLeadSessionId: target === null ? null : (leadOf(target) ?? null),
        requestId,
        input,
      });
      if (answer.outcome === "failed") {
        const { code, hint, ...details } = answer.error;
        throw new AppActionError(code, hint, details);
      }
      return { leadSessionId: answer.leadSessionId };
    },
    async startPrior(requestId, input) {
      return invoke<"none" | { same: string } | "reused">(
        "mono_message_start_prior",
        { caller: caller(), requestId, input },
      );
    },
    async recordStart(requestId, sessionId, input) {
      const answer = await invoke<
        { outcome: "recorded"; messageId: string } | Failed
      >("mono_message_record_start", {
        caller: caller(),
        sessionId,
        requestId,
        input,
      });
      if (answer.outcome === "failed") {
        const { code, hint, ...details } = answer.error;
        throw new AppActionError(code, hint, details);
      }
      return answer.messageId;
    },
  };
}
