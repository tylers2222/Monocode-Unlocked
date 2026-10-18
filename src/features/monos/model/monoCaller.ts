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
  parseMonoId,
  parseSessionId,
  parseTurnId,
  type Caller,
  type MonoId,
  type ToolError,
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

/**
 * The caller of a messaging call: a Mono's chat or habit run, or (design
 * 13.4) a lead or worker session with /operator access. Rust reads roles
 * and projects from `monos`, so the roster must be the database's and every
 * write this window queued must have landed first.
 */
export async function resolveCaller(
  session: CallingSession,
  turnId: string | null,
): Promise<{ ok: true; caller: Caller } | ToolError> {
  const turn = parseTurnId(turnId);
  if (!turn)
    return {
      ok: false,
      code: "turn_inactive",
      hint: "MonoCode app access is inactive. Use /operator once in this thread to enable it, then call the CLI during an active agent turn. Retrying this request now will not enable access.",
    };
  if (!monoRosterPersisted())
    return {
      ok: false,
      code: "roster_unavailable",
      hint: "MonoCode could not load the Mono list from its database. Mono messaging is off until the app restarts. Use a new request id when you retry.",
    };
  await monoWritesSettled();
  const sessionId = parseSessionId(session.id);
  const mono = sessionId ? callerMono(sessionId) : undefined;
  if (sessionId && mono)
    return { ok: true, caller: { ...mono, sessionId, turnId: turn } };
  if (!sessionId || !operatorEnabledInThread(session.blocks))
    return {
      ok: false,
      code: "not_a_mono",
      hint: "Only a Mono's chat, its habit runs or a session with MonoCode app access can use Mono messaging.",
    };
  return {
    ok: true,
    caller: {
      kind: "session",
      sessionId,
      turnId: turn,
      access: "operator",
      leadSessionId: parseSessionId(session.leadSessionId) ?? null,
      cwd: session.cwd,
    },
  };
}
