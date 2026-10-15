import { invoke } from "@tauri-apps/api/core";
import {
  AppActionError,
  type ToolErrorDetails,
} from "../../agent-app/model/appActionError";
import { dequeueQueuedMessage } from "../../sessions/model/messageQueue";
import type { QueuedMessage, Session } from "../../sessions/model/session";
import { enqueueMonoMessage } from "./monoMessaging";
import {
  renderMonoMessage,
  type CallErrorCode,
  type Caller,
  type Delivery,
  type Handle,
  type MessageId,
  type MonoId,
  type MonoMessage,
  type MonoRole,
  type RejectCode,
  type ReplyInput,
  type SendInput,
  type SessionId,
  type ToolError,
  type TurnId,
} from "./monoMessages";

/** A row that a session handles: the only kind `submit` accepts or lists. */
type Routed = Extract<MonoMessage, { handledBySession: unknown }>;

/** A queue item waits for the turn end (`queue`) or may steer a running turn. */
export type QueueKind = "queue" | "steer";

/** What every rule below needs from the window that owns the session. */
export type MonoQueueHost = {
  /** The window's copy of a session, when it has one loaded. */
  session(id: string): Session | undefined;
  /** One synchronous update of the window's sessions. */
  update(id: string, change: (session: Session) => Session): void;
  /** A Mono's chat shows each queued message as a bubble. */
  isMonoChat(id: string): boolean;
  sender(monoId: MonoId | null): { name: string; role: MonoRole } | undefined;
  /** The id of the turn session `id` runs now, or of the one it starts next. */
  turnId(id: string, turn: "current" | "next"): TurnId;
};

/**
 * Design 5.5 rule 2: the only way a Mono row enters a queue. It adds an item
 * whose id is the message id, unless the queue has one. `queue` waits for
 * the turn end; `steer` may steer the running turn. Both go to the tail, so
 * steers keep their order. A Mono chat's item reuses the bubble with that id,
 * or shows a new one.
 */
export function ensureQueued(
  session: Session,
  message: { id: MessageId; text: string },
  kind: QueueKind,
  bubble: boolean,
): Session {
  if (session.queuedMessages?.some((queued) => queued.id === message.id))
    return session;
  const item: QueuedMessage = {
    id: message.id,
    monoMessageId: message.id,
    text: message.text,
    attachments: [],
    ...(kind === "queue" ? { waitForIdle: true } : {}),
  };
  if (bubble && !session.blocks.some((block) => block.id === message.id))
    return enqueueMonoMessage(session, item);
  return {
    ...session,
    queuedMessages: [
      ...(session.queuedMessages ?? []),
      bubble ? { ...item, blockId: message.id } : item,
    ],
    queueStatus: session.queueStatus === "paused" ? "paused" : "active",
  };
}

function queueItem(host: MonoQueueHost, message: Routed) {
  return {
    id: message.id,
    text: renderMonoMessage(message, host.sender(message.fromMonoId)),
  };
}

/**
 * Design 5.5 rule 6: queue every row of the session that no item and no
 * slot holds. Runs when the window loads the session.
 */
export async function reconcile(
  host: MonoQueueHost,
  sessionId: string,
): Promise<void> {
  const messages = await invoke<Routed[]>("mono_messages_unqueued", {
    sessionId,
  });
  for (const message of messages)
    host.update(sessionId, (session) =>
      ensureQueued(
        session,
        queueItem(host, message),
        "queue",
        host.isMonoChat(sessionId),
      ),
    );
}

/**
 * Design 5.5 rules 3 and 4. A hand-off takes the row's slot before it sends
 * the text: `dispatch` opens the next turn, `steer` goes into the running
 * one. A refused slot means another hand-off has the row or it was already
 * delivered, so the item goes and nothing is sent. A failed hand-off gives
 * the slot back and queues the row again.
 */
export async function handOff(
  host: MonoQueueHost,
  sessionId: string,
  message: { id: MessageId; text: string },
  mode: "dispatch" | "steer",
  deliver: () => boolean | Promise<boolean>,
): Promise<boolean> {
  const slot = {
    id: message.id,
    turnId: host.turnId(sessionId, mode === "dispatch" ? "next" : "current"),
  };
  const assigned = await invoke<boolean>("mono_message_assign_slot", {
    ...slot,
    kind: mode === "dispatch" ? "submit" : "steer",
  });
  if (!assigned) {
    host.update(sessionId, (session) =>
      dequeueQueuedMessage(session, message.id),
    );
    return false;
  }
  let accepted = false;
  try {
    accepted = await deliver();
  } finally {
    if (accepted) await invoke("mono_message_mark_delivery", slot);
    else {
      await invoke("mono_message_release", slot);
      host.update(sessionId, (session) =>
        ensureQueued(session, message, "queue", host.isMonoChat(sessionId)),
      );
    }
  }
  return accepted;
}

/** Turn ids by session and turn number, for this window's hand-offs. */
export function createTurnIds(
  turnNumber: (sessionId: string) => number,
): MonoQueueHost["turnId"] {
  const ids = new Map<string, { turn: number; id: TurnId }[]>();
  return (sessionId, which) => {
    const turn = turnNumber(sessionId) + (which === "next" ? 1 : 0);
    const kept = (ids.get(sessionId) ?? []).filter(
      (entry) => entry.turn >= turn - 1,
    );
    const known = kept.find((entry) => entry.turn === turn);
    if (known) return known.id;
    const id = crypto.randomUUID() as TurnId;
    ids.set(sessionId, [...kept, { turn, id }]);
    return id;
  };
}

/**
 * Design 5.4 for an opener: `submit` starts a turn and reports, through the
 * callback it is given, whether the provider took that turn. A submit that
 * answers true has only started it; the turn can still fail before the
 * provider has the text (no provider connected, an authorize error).
 */
export function acceptedTurn(
  submit: (
    providerTook: (accepted: boolean) => void,
  ) => boolean | Promise<boolean>,
): Promise<boolean> {
  return new Promise((resolve) => {
    Promise.resolve(submit(resolve)).then(
      (started) => {
        if (!started) resolve(false);
      },
      () => resolve(false),
    );
  });
}

/**
 * The pump's opener hand-off. It answers as soon as `submit` started the
 * turn or refused it, so the queue moves on; the hand-off keeps waiting for
 * the provider in the background and marks or releases the row then.
 */
export function dispatchOpener(
  host: MonoQueueHost,
  sessionId: string,
  message: { id: MessageId; text: string },
  submit: (
    providerTook: (accepted: boolean) => void,
  ) => boolean | Promise<boolean>,
): Promise<boolean> {
  return new Promise((resolve) => {
    handOff(host, sessionId, message, "dispatch", () =>
      acceptedTurn((providerTook) => {
        const started = submit(providerTook);
        Promise.resolve(started).then(resolve, () => resolve(false));
        return started;
      }),
    ).then(
      (accepted) => {
        if (!accepted) resolve(false);
      },
      () => resolve(false),
    );
  });
}

/** What the window that owns a session needs to run a route there. */
export type MonoRouteHost = MonoQueueHost & {
  /** Loads a session into this window; loading runs `reconcile` for it. */
  open(id: string): Promise<Session | null>;
  /** Steers a running project session; throws when the turn ended. */
  steerProject(session: Session, text: string, id: MessageId): Promise<void>;
};

export type MonoMessagingHost = MonoRouteHost & {
  /** `resolveCaller` for the calling session. */
  caller(): Promise<{ ok: true; caller: Caller } | ToolError>;
  /** The sessions loaded in this window. */
  sessions(): readonly Session[];
  /** A session only the database has, or null. */
  stored(id: string): Promise<Session | null>;
  steerable(session: Session): boolean;
  /** Harness ids that can steer a running turn (design 6: harness only). */
  steerableHarnesses(): string[];
  /** One of the caller's project sessions (design 6.1). */
  ownProject(session: Session): boolean;
  /** Every Mono, in rail order. */
  monos(): { id: MonoId; name: string; role: MonoRole }[];
};

/** The route of a new accept: the gate's choice, which the rows do not hold. */
export type Routing = { sessionId: SessionId; via: "queue" | "steer" };

/** `mono-message-route`: a route this window runs because it owns the session. */
export type MonoRouteEvent = { messages: Routed[]; route: Routing };

type Submitted =
  | {
      outcome: "accepted";
      messages: Routed[];
      answered: { handle: Handle | null; message_id: MessageId }[];
      duplicate: boolean;
      route?: Routing;
      /** Another window owns the session and runs the route (design 6). */
      ownerWindow?: string;
    }
  | {
      outcome: "failed";
      error: {
        code: RejectCode | CallErrorCode;
        hint: string;
      } & ToolErrorDetails;
    };

async function submit(
  host: MonoMessagingHost,
  tool: "send_message" | "reply",
  requestId: string,
  input: SendInput | ReplyInput,
) {
  const resolved = await host.caller();
  if (!resolved.ok) throw new AppActionError(resolved.code, resolved.hint);
  const { kind, monoId, sessionId } = resolved.caller;
  const loaded = host.sessions();
  const sendTo = "send_to" in input ? input.send_to : undefined;
  const addressed =
    sendTo && !loaded.some((session) => session.id === sendTo)
      ? await host.stored(sendTo)
      : null;
  const submitted = await invoke<Submitted>("mono_message_submit", {
    caller: { kind, monoId, sessionId },
    tool,
    requestId,
    input,
    // A session whose worktree is gone cannot receive (design 6).
    live: [...loaded, ...(addressed ? [addressed] : [])]
      .filter((session) => !session.worktreeRemoved)
      .map((session) => ({
        sessionId: session.id,
        steerable: host.steerable(session),
        ownProject: host.ownProject(session),
      })),
    steerableHarnesses: host.steerableHarnesses(),
  });
  if (submitted.outcome === "failed") {
    const { code, hint, ...details } = submitted.error;
    throw new AppActionError(code, hint, details);
  }
  return submitted;
}

/**
 * Executes the route of a new accept (design 5.5 rule 1) in the window that
 * owns its session. A project session that runs is steered now; everything
 * else enters the session's queue, whose pump hands it off. Returns the
 * delivery the call reports.
 */
export async function routeMessages(
  host: MonoRouteHost,
  messages: Routed[],
  { sessionId, via }: Routing,
): Promise<Delivery> {
  // A session no window can load gets its rows when one loads it.
  const loaded = host.session(sessionId) ?? (await host.open(sessionId));
  if (!loaded) return "queue";
  const target = host.session(sessionId) ?? loaded;
  const bubble = host.isMonoChat(sessionId);
  if (via === "steer" && !bubble) {
    let steered = true;
    for (const message of messages) {
      const item = queueItem(host, message);
      const delivered = await handOff(
        host,
        sessionId,
        item,
        "steer",
        async () => {
          await host.steerProject(target, item.text, message.id);
          return true;
        },
      ).catch(() => false);
      steered &&= delivered;
    }
    return steered ? "steer" : "queue";
  }
  const idle = !target.busy && !target.queuedMessages?.length;
  for (const message of messages)
    host.update(sessionId, (session) =>
      ensureQueued(session, queueItem(host, message), via, bubble),
    );
  return via === "steer" ? "steer" : idle ? "submit" : "queue";
}

/**
 * Routes a new accept here, unless another window owns its session; that
 * window got the route as `mono-message-route`. A duplicate is not routed
 * and reports `stored`.
 */
async function routed(
  host: MonoMessagingHost,
  submitted: Awaited<ReturnType<typeof submit>>,
  stored: Delivery,
): Promise<Delivery> {
  const { route, ownerWindow, messages } = submitted;
  if (submitted.duplicate || !route) return stored;
  if (ownerWindow) return route.via;
  return routeMessages(host, messages, route);
}

/** `messages.get_monos` (design 3.1). */
export async function getMonos(host: MonoMessagingHost) {
  const resolved = await host.caller();
  if (!resolved.ok) throw new AppActionError(resolved.code, resolved.hint);
  return {
    monos: host.monos().map((mono) => ({
      mono_id: mono.id,
      mono_name: mono.name,
      role: mono.role,
      you: mono.id === resolved.caller.monoId,
    })),
  };
}

/** `messages.send` (design 3.2). A duplicate reports its stored row. */
export async function sendMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: SendInput,
) {
  const submitted = await submit(host, "send_message", requestId, input);
  const [message] = submitted.messages;
  return {
    message_id: message.id,
    status: message.status,
    delivery: await routed(host, submitted, message.delivery),
    hop: message.hop,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}

/** `messages.reply` (design 3.3). */
export async function replyMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: ReplyInput,
) {
  const submitted = await submit(host, "reply", requestId, input);
  await routed(host, submitted, "queue");
  return {
    message_ids: submitted.messages.map((message) => message.id),
    answered: submitted.answered,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}
