import { isPersistableId } from "../../sessions/data/sessionStore";
import { isMonoId } from "./mono";

declare const brand: unique symbol;
type Brand<T, B extends string> = T & { readonly [brand]: B };

export type MonoId = Brand<string, "MonoId">;
export type SessionId = Brand<string, "SessionId">;
/** `m_` and 12 base32 characters. */
export type MessageId = Brand<string, "MessageId">;
/** `#1` to `#99`, numbered per receiving turn. */
export type Handle = Brand<`#${number}`, "Handle">;
export type RequestId = Brand<string, "RequestId">;
/** `crypto.randomUUID()` of the turn that authorized a call. */
export type TurnId = Brand<string, "TurnId">;

export type MonoRole = "giga" | "mono";
export type Tool = "send_message" | "reply";
export type ActionType = "queue" | "steer";
export type Delivery = "submit" | "queue" | "steer";

// One parse function per brand, each undefined on bad input. Only boundary
// code calls them; everything past the boundary trusts the brand.

export function parseMonoId(raw: unknown): MonoId | undefined {
  return typeof raw === "string" && isMonoId(raw) ? (raw as MonoId) : undefined;
}

export function parseSessionId(raw: unknown): SessionId | undefined {
  return typeof raw === "string" && isPersistableId(raw)
    ? (raw as SessionId)
    : undefined;
}

export function parseMessageId(raw: unknown): MessageId | undefined {
  return typeof raw === "string" && /^m_[a-z2-7]{12}$/.test(raw)
    ? (raw as MessageId)
    : undefined;
}

export function parseHandle(raw: unknown): Handle | undefined {
  return typeof raw === "string" && /^#[1-9][0-9]?$/.test(raw)
    ? (raw as Handle)
    : undefined;
}

export type Party =
  | { kind: "mono"; monoId: MonoId }
  /** A project session; it only receives. */
  | { kind: "session"; sessionId: SessionId };

export type Caller = {
  kind: "mono_chat" | "habit_run";
  monoId: MonoId;
  sessionId: SessionId;
  turnId: TurnId;
};

type MessageBase = {
  id: MessageId;
  requestId: RequestId;
  part: number;
  tool: Tool;
  /** Null only after the Mono was deleted. */
  fromMonoId: MonoId | null;
  fromSessionId: SessionId;
  /** Null for unknown_target rows, or after the recipient Mono was deleted. */
  to: Party | null;
  addressed: string;
  body: string;
  bodyChars: number;
  actionType: ActionType | null;
  replyTo: MessageId | null;
  steers: MessageId | null;
  hop: number;
  createdAt: number;
};

/** Where an accepted message went. */
type Routed = { handledBySession: SessionId; delivery: Delivery };
/** The receiving turn. A row that wants a reply always has a handle there; one that does not never has. */
type WaitingSlot = {
  turnId: TurnId;
  handle: Handle;
  deliveredAt: number | null;
};
type PlainSlot = { turnId: TurnId; deliveredAt: number | null };
type Delivered<S> = S & { deliveredAt: number };

export type MonoMessage =
  | (MessageBase & {
      status: "rejected";
      gateResult: RejectCode;
      wantingReply: boolean;
    })
  | (MessageBase & { status: "open"; wantingReply: true } & Routed & {
        slot: WaitingSlot | null;
      })
  | (MessageBase & { status: "delivered"; wantingReply: false } & Routed & {
        slot: PlainSlot | null;
      })
  | (MessageBase & { status: "answered"; wantingReply: true } & Routed & {
        slot: WaitingSlot | null;
        closedAt: number;
      })
  | (MessageBase & { status: "unanswered"; wantingReply: true } & Routed & {
        slot: Delivered<WaitingSlot>;
        closedAt: number;
        notifiedAt: number | null;
      })
  | (MessageBase & {
      status: "dropped";
      wantingReply: boolean;
      closedAt: number;
      notifiedAt: number | null;
    });

export type RejectCode =
  | "too_large"
  | "unknown_target"
  | "self_send"
  | "target_no_conversation"
  | "direction_forbidden"
  | "upward_steer_forbidden"
  | "reply_unsupported_target"
  | "habit_cannot_wait"
  | "unknown_message"
  | "steer_target_mismatch"
  | "ambiguous_steer"
  | "target_gone"
  | "cannot_steer"
  | "rate_limited"
  | "hop_limit"
  | "no_open_handle"
  | "unknown_handle"
  | "handle_closed"
  | "ambiguous_reply";

/** Failures that write no row. */
export type CallErrorCode =
  | "invalid_input"
  | "request_id_reused"
  | "unknown_token"
  | "turn_inactive"
  | "no_token"
  | "not_a_mono"
  | "unknown_action"
  | "action_removed"
  | "unknown_mono"
  | "giga_only"
  | "self_update"
  | "mono_busy"
  | "roster_unavailable";

/** `open_handles` on ambiguous_reply, unknown_handle and no_open_handle; `candidates` on ambiguous_steer. */
export type ToolError = {
  ok: false;
  code: RejectCode | CallErrorCode;
  hint: string;
  message_id?: MessageId;
  open_handles?: { handle: Handle; message_id: MessageId; from: string }[];
  candidates?: { message_id: MessageId; preview: string }[];
};
