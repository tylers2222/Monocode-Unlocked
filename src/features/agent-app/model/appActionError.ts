import type {
  Handle,
  MessageId,
  RejectCode,
} from "../../monos/model/monoMessages";

/** Codes a Rust command answers with as its whole error, and their hints. */
const RUST_COMMAND_HINTS = {
  unknown_milestone:
    "No milestone with that id in this project; run milestones.list for ids",
  milestones_off:
    "The user turned milestones off for this project, so nothing was changed. Do not retry; milestones.list still reads it",
  stale_order:
    "ids must be every milestone id of the project, each once. Run milestones.list and send the full list",
  unknown_approval:
    "No approval card with that id. Use the id that approvals.file returned.",
  not_your_approval:
    "Another Giga filed that card. You can update or withdraw only your own cards.",
  approval_not_open:
    "That card is decided or withdrawn, so nothing changed. File a new card if it still needs a decision.",
  heading_taken:
    "Another Giga has an open card with that heading in this project. Use another heading.",
} as const;

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
  not_session_owner:
    "Only the session's lead or the session that started it can clear its queue. Do not retry.",
  app_cli_unavailable:
    "This session cannot use the MonoCode app CLI: it is an Inbox Ask, an orchestration lead or worker, or no longer open in MonoCode. Do not retry.",
  ...RUST_COMMAND_HINTS,
} as const;

/**
 * Every code an app action answers with. The one list in code: design 1.4's
 * `CallErrorCode` (PR 3) is this type. `unknown_token` and `turn_inactive`
 * come from `control.rs`, `no_token` from the CLI and `roster_unavailable`
 * from `resolveCaller`. `not_direct_child`, `ack_refused` and
 * `extend_refused` come from `ask_gate.rs` with their hints (R6, R10).
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
  | "roster_unavailable"
  | "not_direct_child"
  | "ack_refused"
  | "extend_refused";

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

export function isRustCommandCode(
  code: unknown,
): code is keyof typeof RUST_COMMAND_HINTS {
  return (
    typeof code === "string" &&
    Object.prototype.hasOwnProperty.call(RUST_COMMAND_HINTS, code)
  );
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
