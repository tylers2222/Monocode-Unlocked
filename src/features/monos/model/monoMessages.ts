import type { AppErrorCode } from "../../agent-app/model/appActionError";
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

/** A turn id as `control_authorize_turn` takes it: 1 to 128 characters. */
export function parseTurnId(raw: unknown): TurnId | undefined {
  return typeof raw === "string" && raw.length > 0 && raw.length <= 128
    ? (raw as TurnId)
    : undefined;
}

/** The id of a turn this window is about to start (design 5.2). */
export function newTurnId(): TurnId {
  return crypto.randomUUID() as TurnId;
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

/** Failures that write no row: every app action code, from the one list. */
export type CallErrorCode = AppErrorCode;

/** `open_handles` on ambiguous_reply, unknown_handle and no_open_handle; `candidates` on ambiguous_steer. */
export type ToolError = {
  ok: false;
  code: RejectCode | CallErrorCode;
  hint: string;
  message_id?: MessageId;
  open_handles?: { handle: Handle; message_id: MessageId; from: string }[];
  candidates?: { message_id: MessageId; preview: string }[];
};

/** A `send_message` call as the model gave it (design 3.2), defaults filled in. */
export type SendInput = {
  message: string;
  send_to: string;
  wanting_reply: boolean;
  action_type: ActionType;
};

/** A `reply` call (design 3.3). `to` names the handles it answers. */
export type ReplyInput = {
  message: string;
  wanting_reply: boolean;
  to?: Handle[];
};

function message(raw: Record<string, unknown>): string {
  if (typeof raw.message !== "string" || !raw.message.trim())
    throw new Error("message must be a non-empty string");
  return raw.message;
}

/** Throws the `invalid_input` hint. Rust parses the same input again (S1). */
export function parseSendInput(raw: Record<string, unknown>): SendInput {
  const text = message(raw);
  if (
    typeof raw.send_to !== "string" ||
    !raw.send_to.trim() ||
    raw.send_to.length > 256
  )
    throw new Error(
      "send_to must be a mono_id from get_monos or the id of a session in one of your projects",
    );
  if (typeof raw.wanting_reply !== "boolean")
    throw new Error(
      "wanting_reply is required: true when you need an answer, false otherwise",
    );
  if (
    raw.action_type !== undefined &&
    raw.action_type !== "steer" &&
    raw.action_type !== "queue"
  )
    throw new Error('action_type must be "steer" or "queue"');
  return {
    message: text,
    send_to: raw.send_to,
    wanting_reply: raw.wanting_reply,
    action_type: raw.action_type ?? "queue",
  };
}

/** Throws the `invalid_input` hint (R1). Rust parses the same input again. */
export function parseReplyInput(raw: Record<string, unknown>): ReplyInput {
  const text = message(raw);
  if (raw.wanting_reply !== undefined && typeof raw.wanting_reply !== "boolean")
    throw new Error("wanting_reply must be true or false");
  const reply = { message: text, wanting_reply: raw.wanting_reply ?? false };
  return raw.to === undefined ? reply : { ...reply, to: parseTo(raw.to) };
}

/** One handle, or a list of 1 to 9 different ones; one is sent as a list. */
function parseTo(raw: unknown): Handle[] {
  const handles = (
    typeof raw === "string" ? [raw] : Array.isArray(raw) ? raw : []
  ).map(parseHandle);
  if (
    handles.length === 0 ||
    handles.length > 9 ||
    new Set(handles).size < handles.length ||
    !handles.every((handle) => handle !== undefined)
  )
    throw new Error(
      'to must be a handle such as "#1", or a list of 1 to 9 different handles',
    );
  return handles;
}

function attribute(value: string): string {
  return value.replace(/[&"<>]/g, (c) => `&#${c.charCodeAt(0)};`);
}

/**
 * What the receiving agent reads (design 5.2). A message that wants a reply
 * shows the handle of the turn it was handed to. How to treat and answer it
 * lives once in `<mono_messaging_rules>`.
 */
export function renderMonoMessage(
  message: Pick<MessageBase, "id" | "fromMonoId" | "body"> & {
    wantingReply: boolean;
  },
  sender: { name: string; role: MonoRole } | undefined,
  handle?: Handle,
): string {
  const attributes = [
    `message_id="${message.id}"`,
    `from="${attribute(sender?.name ?? "a deleted Mono")}"`,
    ...(message.fromMonoId ? [`mono_id="${message.fromMonoId}"`] : []),
    ...(sender ? [`role="${sender.role}"`] : []),
    `wanting_reply="${message.wantingReply}"`,
    ...(handle ? [`handle="${handle}"`] : []),
  ];
  return `<mono_message ${attributes.join(" ")}>\n${message.body}\n</mono_message>`;
}

/**
 * What the sender's chat reads when its message ended without an answer
 * (design 5.3): which message, to whom, and its first 60 characters.
 */
export function renderMonoNotice(
  message: Pick<MessageBase, "id" | "body"> & {
    status: "unanswered" | "dropped";
  },
  recipient: string,
): string {
  const line = Array.from(message.body.trim().split(/\s+/).join(" "))
    .slice(0, 60)
    .join("");
  return `<mono_notice message_id="${message.id}" status="${message.status}" to="${attribute(recipient)}">\n${line}\n</mono_notice>`;
}
