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
export type Delivery = "submit" | "queue" | "steer";
export type IntentKind = "new" | "reply" | "steer" | "continue";
/**
 * The intent of an accepted row (design 16.1, 16.12). `continue` never passes
 * the gate. A steer names one message or the whole turn; neither and both
 * have no form.
 */
export type Intent =
  | { intent: "new"; replyTo: null; scope: null }
  | { intent: "reply" | "steer"; replyTo: MessageId; scope: null }
  | { intent: "steer"; replyTo: null; scope: "turn" };
/** What a rejected row asked for. `replyTo` only when it named a stored row. */
export type AskedIntent = {
  intent: IntentKind;
  replyTo: MessageId | null;
  scope: "turn" | null;
};

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

function isIntentKind(raw: unknown): raw is IntentKind {
  return (
    raw === "new" || raw === "reply" || raw === "steer" || raw === "continue"
  );
}

/** A row's intent columns, parsed: an accepted row has an `Intent`. */
export type RowIntent =
  | { status: "accepted"; intent: Intent }
  | { status: "rejected"; asked: AskedIntent };

/**
 * The intent columns of a row from Rust. Undefined when they disagree: an
 * accepted row is `new` with no target, `reply` with `replyTo`, or `steer`
 * with exactly one of `replyTo` and `scope`.
 */
export function parseRowIntent(row: {
  status: unknown;
  intent: unknown;
  replyTo: unknown;
  scope: unknown;
}): RowIntent | undefined {
  const { intent } = row;
  const replyTo = row.replyTo === null ? null : parseMessageId(row.replyTo);
  const scope =
    row.scope === null || row.scope === "turn" ? row.scope : undefined;
  if (replyTo === undefined || scope === undefined || !isIntentKind(intent))
    return undefined;
  if (row.status === "rejected")
    return { status: "rejected", asked: { intent, replyTo, scope } };
  if (intent === "new")
    return replyTo === null && scope === null
      ? { status: "accepted", intent: { intent, replyTo, scope } }
      : undefined;
  if (intent === "continue") return undefined;
  if (replyTo !== null && scope === null)
    return { status: "accepted", intent: { intent, replyTo, scope } };
  if (intent === "steer" && replyTo === null && scope === "turn")
    return { status: "accepted", intent: { intent, replyTo, scope } };
  return undefined;
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
  | (MessageBase &
      AskedIntent & {
        status: "rejected";
        gateResult: RejectCode;
        wantingReply: boolean;
      })
  | (MessageBase &
      Intent & { status: "open"; wantingReply: true } & Routed & {
        slot: WaitingSlot | null;
      })
  | (MessageBase &
      Intent & { status: "delivered"; wantingReply: false } & Routed & {
        slot: PlainSlot | null;
      })
  | (MessageBase &
      Intent & { status: "answered"; wantingReply: true } & Routed & {
        slot: WaitingSlot | null;
        closedAt: number;
      })
  | (MessageBase &
      Intent & { status: "unanswered"; wantingReply: true } & Routed & {
        slot: Delivered<WaitingSlot>;
        closedAt: number;
        notifiedAt: number | null;
      })
  | (MessageBase &
      Intent & {
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
  | "intent_mismatch"
  | "intent_not_allowed"
  | "target_gone"
  | "cannot_steer"
  | "steer_needs_target"
  | "steer_scope_not_allowed"
  | "nothing_running"
  | "rate_limited"
  | "hop_limit"
  | "no_open_handle"
  | "unknown_handle"
  | "handle_closed"
  | "ambiguous_reply";

/** Failures that write no row: every app action code, from the one list. */
export type CallErrorCode = AppErrorCode;

/** `open_handles` on ambiguous_reply, unknown_handle and no_open_handle. */
export type ToolError = {
  ok: false;
  code: RejectCode | CallErrorCode;
  hint: string;
  message_id?: MessageId;
  open_handles?: { handle: Handle; message_id: MessageId; from: string }[];
};

/** What a send names (design 16.12). "Both" has no form. */
type SendTarget =
  /** A message id only (PR I2 review r1): a steer names a message the caller sent. */
  | { reply_to: MessageId; scope?: never }
  /** Everything the recipient is doing now, in its own running turn. */
  | { scope: "turn"; reply_to?: never }
  | { reply_to?: never; scope?: never };

/** A `send_message` call as the model gave it (design 16.4, 16.12). */
export type SendInput = {
  message: string;
  send_to: string;
  wanting_reply: boolean;
  /** As asked: the gate refuses the intents a send cannot carry (I1, W1, I2). */
  intent: IntentKind;
} & SendTarget;

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
  if (!isIntentKind(raw.intent))
    throw new Error(
      'intent is required and is one of "new", "steer", "reply" or "continue". Use "new" for new work, or "steer" with reply_to to change the work a message started.',
    );
  const send = {
    message: text,
    send_to: raw.send_to,
    wanting_reply: raw.wanting_reply,
    intent: raw.intent,
  };
  if (raw.reply_to !== undefined && raw.scope !== undefined)
    throw new Error(
      'Pass reply_to or scope "turn", not both. reply_to steers the work one message started; scope "turn" steers everything the recipient is doing now.',
    );
  if (raw.scope !== undefined) {
    if (raw.scope !== "turn")
      throw new Error(
        'scope must be "turn": steer everything the recipient is doing now.',
      );
    return { ...send, scope: "turn" };
  }
  if (raw.reply_to === undefined) return send;
  const replyTo = parseMessageId(raw.reply_to);
  if (!replyTo)
    throw new Error(
      "reply_to must be one message_id (m_…). A steer names a message you sent: use the message_id your send_message returned.",
    );
  return { ...send, reply_to: replyTo };
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

/** A `get_message` call (design 16.4): one message id or handle. */
export type GetInput = { handle: MessageId | Handle };

/** Throws the `invalid_input` hint. Rust parses the same input again. */
export function parseGetInput(raw: Record<string, unknown>): GetInput {
  const handle = parseHandle(raw.handle) ?? parseMessageId(raw.handle);
  if (!handle)
    throw new Error(
      'handle must be one message_id or one handle such as "#1".',
    );
  return { handle };
}

/** One end of a message as `get_message` shows it. */
export type NamedParty =
  { mono_id: MonoId; mono_name: string } | { session_id: SessionId };

/** What `get_message` answers (design 16.4). */
export type ReadMessage = {
  message_id: MessageId;
  from: NamedParty;
  to: NamedParty | null;
  intent: IntentKind;
  reply_to: MessageId | null;
  body: string;
  wanting_reply: boolean;
  /** Only accepted rows are readable. */
  status: Exclude<MonoMessage["status"], "rejected">;
  created_at: number;
  /** Only when the message holds a handle in the caller's current turn. */
  handle?: Handle;
};

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

export type NoticeStatus = "unanswered" | "dropped";

/** A sender notice as its queue item holds it (design 5.3). */
export type MonoNotice = {
  messageId: MessageId;
  status: NoticeStatus;
  recipient: string;
  /** The first 60 characters of the message, on one line. */
  line: string;
};

/** The request id of a notice's queue item and bubble: one per row and status. */
export function noticeRequestId(id: MessageId, status: NoticeStatus): string {
  return `notice-${id}-${status}`;
}

/**
 * What the sender's chat reads when its message ended without an answer
 * (design 5.3): which message, to whom, and its first 60 characters.
 */
export function renderMonoNotice(
  message: Pick<MessageBase, "id" | "body"> & { status: NoticeStatus },
  recipient: string,
): string {
  const line = Array.from(message.body.trim().split(/\s+/).join(" "))
    .slice(0, 60)
    .join("");
  return `<mono_notice message_id="${message.id}" status="${message.status}" to="${attribute(recipient)}">\n${line}\n</mono_notice>`;
}

const NOTICE =
  /^<mono_notice message_id="([^"]*)" status="(unanswered|dropped)" to="([^"]*)">\n([^\n]*)\n<\/mono_notice>$/;

/**
 * The notice a queue item or bubble holds. Only the pump's request id makes
 * one: a message that merely quotes a `<mono_notice>` stays a message.
 */
export function parseMonoNotice(item: {
  appRequestId?: string;
  text: string;
}): MonoNotice | undefined {
  const match = NOTICE.exec(item.text);
  const messageId = parseMessageId(match?.[1]);
  if (!match || !messageId) return undefined;
  const status = match[2] as NoticeStatus;
  if (item.appRequestId !== noticeRequestId(messageId, status))
    return undefined;
  return {
    messageId,
    status,
    recipient: match[3].replace(/&#(\d+);/g, (_, code: string) =>
      String.fromCharCode(Number(code)),
    ),
    line: match[4],
  };
}

const NOTICE_WORDS: Record<NoticeStatus, { label: string; verb: string }> = {
  unanswered: { label: "Unanswered", verb: "did not reply to" },
  dropped: { label: "Dropped", verb: "never got" },
};

/** A notice in words (design U10): its status word and the sentence after it. */
export function noticeSentence(notice: MonoNotice): {
  label: string;
  text: string;
} {
  const words = NOTICE_WORDS[notice.status];
  // renderMonoNotice keeps 60 characters, so a full line was cut.
  const cut = Array.from(notice.line).length >= 60 ? "…" : "";
  return {
    label: words.label,
    text: `${notice.recipient} ${words.verb} “${notice.line}${cut}”`,
  };
}
