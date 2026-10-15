/** Codes whose hint is always the same. */
const HINTS = {
  not_a_mono: "Only a Mono can use monos.* actions",
  unknown_mono: "Unknown Mono; run monos.list for IDs",
  no_conversation:
    "That Mono has no conversation yet; open it in MonoCode once",
  giga_only: "Only a Giga Mono can create or update Monos",
  self_update: "A Mono cannot update itself",
  mono_busy:
    "That Mono is busy, so nothing was changed. Update it again after its turn ends",
} as const;

/**
 * Every code an app action answers with. The one list in code: design 1.4's
 * `CallErrorCode` (PR 3) derives from it rather than repeating it.
 */
export type AppErrorCode =
  | keyof typeof HINTS
  | "invalid_input"
  | "request_id_reused"
  | "unknown_action"
  | "action_removed";

/**
 * An app action the app refused for a reason the agent can act on. `hint`
 * says how to fix the call; it is also the error message.
 */
export class AppActionError extends Error {
  constructor(
    readonly code: AppErrorCode,
    readonly hint: string,
  ) {
    super(hint);
  }
}

export function monoError(code: keyof typeof HINTS): AppActionError {
  return new AppActionError(code, HINTS[code]);
}

/**
 * The `control_reply` body for a failed call. `error` stays equal to `hint`,
 * so callers that read only `error` see the same text as before.
 */
export function appErrorResponse(
  error: unknown,
):
  | { ok: false; code: AppErrorCode; hint: string; error: string }
  | { ok: false; error: string } {
  if (error instanceof AppActionError)
    return { ok: false, code: error.code, hint: error.hint, error: error.hint };
  return {
    ok: false,
    error: error instanceof Error ? error.message : String(error),
  };
}
