import { invoke } from "@tauri-apps/api/core";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  AppActionError,
  type ToolErrorDetails,
} from "../../agent-app/model/appActionError";
import { dequeueQueuedMessage } from "../../sessions/model/messageQueue";
import type { QueuedMessage, Session } from "../../sessions/model/session";
import { enqueueMonoMessage } from "./monoMessaging";
import {
  newTurnId,
  renderMonoMessage,
  renderMonoNotice,
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
/** A row `assign_slot` takes: it refuses every other status. */
type Waiting = Extract<MonoMessage, { status: "open" | "delivered" }>;
/** A row whose sender is owed a notice (design 5.3). */
type Noticed = Extract<MonoMessage, { status: "unanswered" | "dropped" }>;

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
  /** A Mono's name and role, from the roster. */
  sender(monoId: MonoId | null): { name: string; role: MonoRole } | undefined;
  /** The turn this window runs for session `id` now, if any. */
  turnId(id: string): TurnId | undefined;
  /** `control_owns_session`: this window runs the routes of session `id`. */
  owns(id: string): Promise<boolean>;
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
 * Design 5.3: a sender notice enters the chat once, as a message that waits
 * for the turn end. Its request id in the queue or the transcript means it
 * was queued or sent before, so a pump that runs again adds nothing.
 */
function ensureNotice(
  session: Session,
  notice: { id: string; text: string },
): Session {
  if (
    session.queuedMessages?.some(
      (queued) => queued.appRequestId === notice.id,
    ) ||
    session.blocks.some((block) => block.appRequestId === notice.id)
  )
    return session;
  return enqueueMonoMessage(session, {
    id: notice.id,
    appRequestId: notice.id,
    text: notice.text,
    attachments: [],
    waitForIdle: true,
  });
}

function noticeItem(host: MonoQueueHost, message: Noticed) {
  const recipient =
    message.to?.kind === "session"
      ? message.addressed
      : (host.sender(message.to?.monoId ?? null)?.name ?? "a deleted Mono");
  return {
    id: `notice-${message.id}-${message.status}`,
    text: renderMonoNotice(message, recipient),
  };
}

/**
 * Design 5.5 rule 6, only in the window that owns the session (PR 4a review
 * m19): queue every row of the session that no item and no slot holds. Then
 * the notice pump of design 5.3: the session is a Mono's chat, and each of
 * that Mono's messages that ended unanswered or dropped gets one notice.
 */
export async function reconcile(
  host: MonoQueueHost,
  sessionId: string,
): Promise<void> {
  if (!(await host.owns(sessionId))) return;
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
  const notices = await invoke<Noticed[]>("mono_messages_pending_notices", {
    sessionId,
  });
  for (const message of notices) {
    // A chat this window closed meanwhile gets its notices at its next load.
    if (!host.session(sessionId)) return;
    // The claim comes first: the owner can change while this pump runs, and
    // only the window whose claim marks the row queues its notice (PR 5
    // review r2 m1).
    const claimed = await invoke<boolean>("mono_message_mark_notified", {
      id: message.id,
      status: message.status,
    });
    if (!claimed) continue;
    const notice = noticeItem(host, message);
    host.update(sessionId, (session) => ensureNotice(session, notice));
  }
}

/**
 * One reconcile for each load of a session in this window (design 5.5 rule
 * 6). The load starts it, and a route that loaded the session awaits the
 * same one, so the older rows it queues stay ahead (PR 4a review m6).
 */
export function createLoadReconciles(host: MonoQueueHost) {
  const loads = new Map<string, Promise<void>>();
  return {
    /** The reconcile of the session's current load; the first ask starts it. */
    of(sessionId: string): Promise<void> {
      let load = loads.get(sessionId);
      if (!load) {
        load = reconcile(host, sessionId).catch(console.error);
        loads.set(sessionId, load);
      }
      return load;
    },
    /** Forgets the sessions the window no longer has; a new load reconciles again. */
    keep(loaded: ReadonlySet<string>) {
      for (const id of loads.keys()) if (!loaded.has(id)) loads.delete(id);
    },
  };
}

/** What a hand-off gives the provider: the row with its handle, and the turn. */
export type HandedOff = { text: string; turnId: TurnId };

/**
 * An opener hand-off took its slot for a new turn `turnId`. A session that
 * is busy runs another turn, so the submit must refuse it: the hand-off then
 * releases the slot and queues the row again (PR 5 review m6).
 */
export function opensTurn(session: Session, turnId: TurnId | undefined) {
  return !(turnId && session.busy);
}

/**
 * Design 5.5 rules 3 and 4. A hand-off takes the row's slot before it sends
 * the text: `dispatch` opens a new turn, `steer` goes into the one this
 * window runs, and waits in the queue when there is none. A refused slot
 * means another hand-off has the row or it was already delivered, so the
 * item goes and nothing is sent. A failed hand-off gives the slot back and
 * queues the row again.
 */
export async function handOff(
  host: MonoQueueHost,
  sessionId: string,
  id: MessageId,
  mode: "dispatch" | "steer",
  deliver: (handed: HandedOff) => boolean | Promise<boolean>,
): Promise<boolean> {
  const turnId = mode === "dispatch" ? newTurnId() : host.turnId(sessionId);
  if (!turnId) return false;
  const slot = { id, turnId };
  const row = await invoke<Waiting | null>("mono_message_assign_slot", {
    ...slot,
    kind: mode === "dispatch" ? "submit" : "steer",
  });
  if (!row) {
    host.update(sessionId, (session) => dequeueQueuedMessage(session, id));
    return false;
  }
  let accepted = false;
  try {
    accepted = await deliver({
      text: renderMonoMessage(
        row,
        host.sender(row.fromMonoId),
        row.status === "open" ? row.slot?.handle : undefined,
      ),
      turnId,
    });
  } finally {
    if (accepted) await invoke("mono_message_mark_delivery", slot);
    else {
      await invoke("mono_message_release", slot);
      host.update(sessionId, (session) =>
        ensureQueued(
          session,
          queueItem(host, row),
          "queue",
          host.isMonoChat(sessionId),
        ),
      );
    }
  }
  return accepted;
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
  id: MessageId,
  submit: (
    handed: HandedOff,
    providerTook: (accepted: boolean) => void,
  ) => boolean | Promise<boolean>,
): Promise<boolean> {
  return new Promise((resolve) => {
    handOff(host, sessionId, id, "dispatch", (handed) =>
      acceptedTurn((providerTook) => {
        const started = submit(handed, providerTook);
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
  /** Loads a session into this window. */
  open(id: string): Promise<Session | null>;
  /** The reconcile of this window's load of a session (`createLoadReconciles`). */
  reconciled(id: string): Promise<void>;
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
  /** The delivery the owner window reports for a route it ran, if it does. */
  ownerDelivery(messageId: MessageId): Promise<Delivery | undefined>;
  /** What this window's calls reported, for their duplicates. */
  reported: ReportedDeliveries;
};

/** The route of a new accept: the gate's choice, which the rows do not hold. */
export type Routing = { sessionId: SessionId; via: "queue" | "steer" };

/** The rows of an accept that go to one session; another window may own it. */
type RouteGroup = Routing & { messageIds: MessageId[]; ownerWindow?: string };

/** `mono-message-route`: a route this window runs because it owns the session. */
export type MonoRouteEvent = {
  messages: Routed[];
  route: Routing;
  callerWindow: string;
};

/** `mono-message-routed`: the owner's answer to the caller window. */
export type MonoRoutedEvent = { messageId: MessageId; delivery: Delivery };

/**
 * Deliveries that owner windows report, by message id. An answer can come
 * before the caller waits for it, so an answer no one waits for is kept for
 * one timeout, then dropped: a late answer to a wait that already timed out
 * is never kept for the life of the window.
 */
export function createOwnerDeliveries(timeoutMs: number) {
  const answers = new Map<MessageId, Delivery>();
  const waiting = new Map<MessageId, (delivery: Delivery) => void>();
  return {
    settle({ messageId, delivery }: MonoRoutedEvent) {
      const waiter = waiting.get(messageId);
      if (waiter) return waiter(delivery);
      answers.set(messageId, delivery);
      setTimeout(() => {
        if (answers.get(messageId) === delivery) answers.delete(messageId);
      }, timeoutMs);
    },
    wait(messageId: MessageId): Promise<Delivery | undefined> {
      const early = answers.get(messageId);
      answers.delete(messageId);
      if (early) return Promise.resolve(early);
      return new Promise((resolve) => {
        const timer = setTimeout(() => {
          waiting.delete(messageId);
          resolve(undefined);
        }, timeoutMs);
        waiting.set(messageId, (delivery) => {
          clearTimeout(timer);
          waiting.delete(messageId);
          resolve(delivery);
        });
      });
    },
  };
}

export type OwnerDeliveries = ReturnType<typeof createOwnerDeliveries>;

/** What this window's calls reported, by message id (PR 4a review m3). */
export type ReportedDeliveries = {
  set(messageId: MessageId, delivery: Promise<Delivery>): void;
  /** Waits for a first call that still routes; undefined when it failed. */
  get(messageId: MessageId): Promise<Delivery | undefined> | undefined;
};

/**
 * The delivery each new accept in this window reported, so a duplicate of
 * the call reports the same. Keeps the newest `limit` entries.
 */
export function createReportedDeliveries(limit: number): ReportedDeliveries {
  const reported = new Map<MessageId, Promise<Delivery | undefined>>();
  return {
    set(messageId, delivery) {
      reported.set(
        messageId,
        delivery.catch(() => undefined),
      );
      for (const oldest of reported.keys()) {
        if (reported.size <= limit) break;
        reported.delete(oldest);
      }
    },
    get: (messageId) => reported.get(messageId),
  };
}

/**
 * This window's side of design 6 across windows: it runs a route Rust sent
 * to it as the owner, answers the caller with the delivery, and takes the
 * answers to its own calls. Both listen on this window only. A global
 * `listen` is registered as `Any`, which Tauri matches for every event
 * whatever the emitter's target (as `main.tsx` notes for `quit_confirm`), so
 * every window would run the route and load its own copy of the session.
 */
export function listenForMonoRoutes(
  host: () => MonoRouteHost,
  deliveries: Pick<OwnerDeliveries, "settle">,
): () => void {
  const here = getCurrentWebviewWindow();
  const routes = here.listen<MonoRouteEvent>(
    "mono-message-route",
    ({ payload }) => {
      void routeMessages(host(), payload.messages, payload.route)
        .then((delivery) =>
          emitTo(payload.callerWindow, "mono-message-routed", {
            messageId: payload.messages[0].id,
            delivery,
          } satisfies MonoRoutedEvent),
        )
        .catch(console.error);
    },
  );
  const answers = here.listen<MonoRoutedEvent>(
    "mono-message-routed",
    ({ payload }) => deliveries.settle(payload),
  );
  return () => {
    void routes.then((unlisten) => unlisten());
    void answers.then((unlisten) => unlisten());
  };
}

/** `mono-message-notices`: sessions with rows to queue or a notice owed. */
export type MonoNoticesEvent = { sessions: string[] };

/**
 * Reconciles each session a `mono-message-notices` event names that this
 * window has loaded. Rust sends the event to every window; `reconcile` acts
 * only in the one that owns the session.
 */
export function listenForMonoNotices(host: MonoQueueHost): () => void {
  const listening = getCurrentWebviewWindow().listen<MonoNoticesEvent>(
    "mono-message-notices",
    ({ payload }) => {
      for (const sessionId of payload.sessions)
        if (host.session(sessionId))
          void reconcile(host, sessionId).catch(console.error);
    },
  );
  return () => void listening.then((unlisten) => unlisten());
}

type Accepted = {
  outcome: "accepted";
  messages: Routed[];
  answered: { handle: Handle | null; message_id: MessageId }[];
  duplicate: boolean;
  /** Empty for a duplicate. */
  routes: RouteGroup[];
};

type Submitted =
  | Accepted
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
): Promise<Accepted> {
  const resolved = await host.caller();
  if (!resolved.ok) throw new AppActionError(resolved.code, resolved.hint);
  const loaded = host.sessions();
  const sendTo = "send_to" in input ? input.send_to : undefined;
  const addressed =
    sendTo && !loaded.some((session) => session.id === sendTo)
      ? await host.stored(sendTo)
      : null;
  const submitted = await invoke<Submitted>("mono_message_submit", {
    caller: resolved.caller,
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
 * owns its session. A project session that runs a turn in this window is
 * steered now; everything else enters the session's queue, whose pump hands
 * it off. Returns the delivery the call reports.
 */
export async function routeMessages(
  host: MonoRouteHost,
  messages: Routed[],
  { sessionId, via }: Routing,
): Promise<Delivery> {
  // A session no window can load gets its rows when one loads it.
  const loaded = host.session(sessionId) ?? (await host.open(sessionId));
  if (!loaded) return "queue";
  // Rows the load queues are older than these, so they go first.
  await host.reconciled(sessionId);
  const target = host.session(sessionId) ?? loaded;
  const bubble = host.isMonoChat(sessionId);
  if (via === "steer" && !bubble && host.turnId(sessionId)) {
    let steered = true;
    for (const message of messages) {
      const delivered = await handOff(
        host,
        sessionId,
        message.id,
        "steer",
        async ({ text }) => {
          await host.steerProject(target, text, message.id);
          return true;
        },
      ).catch(() => false);
      steered &&= delivered;
    }
    return steered ? "steer" : "queue";
  }
  const kind = via === "steer" && bubble ? "steer" : "queue";
  const routed = new Set<string>(messages.map((message) => message.id));
  // The load may have queued these rows already; they still open the turn.
  const idle =
    !target.busy &&
    (target.queuedMessages ?? []).every((queued) => routed.has(queued.id));
  for (const message of messages)
    host.update(sessionId, (session) =>
      ensureQueued(session, queueItem(host, message), kind, bubble),
    );
  return kind === "steer" ? "steer" : idle ? "submit" : "queue";
}

/**
 * Runs each route of a new accept here, unless another window owns its
 * session and got it as `mono-message-route`. A multi-handle reply has one
 * route per sender session; the routes run at once. Returns each route's
 * delivery. `owners: "skip"` does not wait for another window's report, for
 * a call that reports no delivery.
 */
function route(
  host: MonoMessagingHost,
  { messages, routes }: Accepted,
  owners: "wait" | "skip",
): Promise<Delivery[]> {
  return Promise.all(
    routes.map(async (group) => {
      if (!group.ownerWindow)
        return routeMessages(
          host,
          messages.filter((message) => group.messageIds.includes(message.id)),
          group,
        );
      if (owners === "skip") return group.via;
      return (await host.ownerDelivery(group.messageIds[0])) ?? group.via;
    }),
  );
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

/** `messages.send` (design 3.2). */
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
    delivery: await sendDelivery(host, submitted, message),
    hop: message.hop,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}

/**
 * Only the call that got the accept routes (design 5.5 rule 1). A duplicate
 * reports what that call reported in this window, else its stored row's
 * delivery (PR 4a review m3).
 */
async function sendDelivery(
  host: MonoMessagingHost,
  submitted: Accepted,
  message: Routed,
): Promise<Delivery> {
  if (submitted.duplicate)
    return (await host.reported.get(message.id)) ?? message.delivery;
  const delivery = route(host, submitted, "wait").then(
    ([routed]) => routed ?? message.delivery,
  );
  host.reported.set(message.id, delivery);
  return delivery;
}

/** `messages.reply` (design 3.3). */
export async function replyMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: ReplyInput,
) {
  const submitted = await submit(host, "reply", requestId, input);
  if (!submitted.duplicate) await route(host, submitted, "skip");
  return {
    message_ids: submitted.messages.map((message) => message.id),
    answered: submitted.answered,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}
