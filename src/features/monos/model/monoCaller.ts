import {
  findMono,
  monoForSession,
  monoRosterPersisted,
  monoWritesSettled,
} from "./mono";
import { habitRunMono } from "./monoHabits";
import {
  parseMonoId,
  type Caller,
  type MonoId,
  type SessionId,
  type ToolError,
  type TurnId,
} from "./monoMessages";

/**
 * The Mono a session speaks for: its chat, or one of its habit runs. This
 * decides who gets the messaging tools at spawn time and who may call them.
 */
export function callerMono(
  sessionId: string,
): { kind: Caller["kind"]; monoId: MonoId } | undefined {
  const chat = monoForSession(sessionId);
  const mono = chat ?? findMono(habitRunMono(sessionId) ?? "");
  const monoId = parseMonoId(mono?.id);
  if (!monoId) return undefined;
  return { kind: chat ? "mono_chat" : "habit_run", monoId };
}

/**
 * The Mono a messaging call comes from. Rust reads roles and projects from
 * `monos`, so the roster must be the database's and every write this window
 * queued must have landed first.
 */
export async function resolveCaller(
  sessionId: SessionId,
  turnId: TurnId,
): Promise<{ ok: true; caller: Caller } | ToolError> {
  if (!monoRosterPersisted())
    return {
      ok: false,
      code: "roster_unavailable",
      hint: "MonoCode could not load the Mono list from its database. Mono messaging is off until the app restarts. Use a new request id when you retry.",
    };
  await monoWritesSettled();
  const mono = callerMono(sessionId);
  if (!mono)
    return {
      ok: false,
      code: "not_a_mono",
      hint: "Only a Mono's chat or its habit runs can use Mono messaging.",
    };
  return { ok: true, caller: { ...mono, sessionId, turnId } };
}
