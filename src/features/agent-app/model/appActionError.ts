import type {
  Handle,
  MessageId,
  RejectCode,
} from "../../monos/model/monoMessages";

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
  app_cli_unavailable:
    "This session cannot use the MonoCode app CLI: it is an Inbox Ask, an orchestration lead or worker, or no longer open in MonoCode. Do not retry.",
  unknown_milestone:
    "No milestone with that id in this project; run milestones.list for ids",
  milestones_off:
    "The user turned milestones off for this project, so nothing was changed. Do not retry; milestones.list still reads it",
  stale_order:
    "ids must be every milestone id of the project, each once. Run milestones.list and send the full list",
} as const;

/**
 * Every code an app action answers with. The one list in code: design 1.4's
 * `CallErrorCode` (PR 3) is this type. `unknown_token` and `turn_inactive`
 * come from `control.rs`, `no_token` from the CLI and `roster_unavailable`
 * from `resolveCaller`.
 */
export type AppErrorCode =
  | keyof typeof HINTS
  | "invalid_input"
  | "request_id_reused"
  | "unknown_action"
  | "action_removed"
  | "unknown_token"
  | "turn_inactive"
  | "no_token"
  | "roster_unavailable";

/** What a messaging failure adds to its code and hint (design 1.4 `ToolError`). */
export type ToolErrorDetails = {
  message_id?: MessageId;
  open_handles?: { handle: Handle; message_id: MessageId; from: string }[];
  duplicate?: true;
};

/**
 * An app action the app refused for a reason the agent can act on. `hint`
 * says how to fix the call; it is also the error message.
 */
export class AppActionError extends Error {
  constructor(
    readonly code: AppErrorCode | RejectCode,
    readonly hint: string,
    readonly details: ToolErrorDetails = {},
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
export function appErrorResponse(error: unknown):
  | ({
      ok: false;
      code: AppErrorCode | RejectCode;
      hint: string;
      error: string;
    } & ToolErrorDetails)
  | { ok: false; error: string } {
  if (error instanceof AppActionError)
    return {
      ok: false,
      code: error.code,
      hint: error.hint,
      error: error.hint,
      ...error.details,
    };
  return {
    ok: false,
    error: error instanceof Error ? error.message : String(error),
  };
}
