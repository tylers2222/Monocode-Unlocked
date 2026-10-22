import { invoke } from "@tauri-apps/api/core";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  AppActionError,
  type ToolErrorDetails,
} from "../../agent-app/model/appActionError";
import { excerpt, type ReplyTarget } from "../../sessions/model/messageIntent";
import {
  dequeueQueuedMessage,
  enqueueQueuedMessage,
} from "../../sessions/model/messageQueue";
import {
  WHOLE_TURN,
  type MessageLink,
  type QueuedMessage,
  type Session,
} from "../../sessions/model/session";
import { enqueueMonoMessage } from "./monoMessaging";
import {
  boardDecision,
  type BoardDecision,
} from "../../approvals/model/boardDecision";
import {
  APPROVALS_SENDER,
  newTurnId,
  noticeRequestId,
  renderMonoMessage,
  renderMonoNotice,
  type AckInput,
  type AskInput,
  type CallErrorCode,
  type Caller,
  type Carries,
  type Delivery,
  type GetInput,
  type Handle,
  type MessageId,
  type MessageSender,
  type MonoId,
  type MonoMessage,
  type ReadMessage,
  type MonoRole,
  type NoticeStatus,
  type RejectCode,
  type ReplyInput,
  type SendInput,
  type SessionId,
  type ToolError,
  type TurnId,
} from "./monoMessages";

/** A row that a session handles: the only kind `submit` accepts or lists. */
type Routed = Extract<MonoMessage, { handledBySession: unknown }>;
/** A row and the message its line names (Rust `Targeted`, design 16.7). */
type Targeted<M extends MonoMessage> = {
  message: M;
  target: ReplyTarget | null;
  carries: Carries | null;
};
/** A row `assign_slot` takes: it refuses every other status. */
type Waiting = Extract<MonoMessage, { status: "open" | "delivered" }>;
/** A row whose sender is owed a notice (design 5.3). */
type Noticed = Extract<MonoMessage, { status: NoticeStatus }>;

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
  /**
   * The lead or worker session that wrote a row with no Mono (design 13).
   * Undefined for any other session: the row is a deleted Mono's. Never
   * rejects.
   */
  sessionSender(
    sessionId: SessionId,
  ): Promise<Extract<MessageSender, { kind: "session" }> | undefined>;
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
 * or shows a new one with the row's link (design 16.3); the item itself has
 * no link, which is the user's own.
 */
export function ensureQueued(
  session: Session,
  message: {
    id: MessageId;
    text: string;
    link?: MessageLink;
    decision?: BoardDecision;
  },
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
    return enqueueMonoMessage(session, item, {
      ...(message.link ? { link: message.link } : {}),
      ...(message.decision ? { boardDecision: message.decision } : {}),
    });
  return enqueueQueuedMessage(
    session,
    bubble ? { ...item, blockId: message.id } : item,
  );
}

/**
 * The link a Mono row's bubble shows (design 16.3, 16.12.6): what a reply
 * or targeted steer names, with the target's excerpt, or the whole turn.
 */
function rowLink(
  message: MonoMessage,
  target: ReplyTarget | null,
): MessageLink | undefined {
  if (message.status === "rejected") return undefined;
  if (message.scope === "turn") return WHOLE_TURN;
  if (message.intent === "new" || !target) return undefined;
  return {
    intent: message.intent,
    replyTo: target.messageId,
    quote: excerpt(target.body),
  };
}

/**
 * Who sent a row: its Mono, the user for an approvals-board decision, else
 * the lead or worker session that wrote it.
 */
async function rowSender(
  host: MonoQueueHost,
  message: Pick<MonoMessage, "fromMonoId" | "fromSessionId">,
): Promise<MessageSender | undefined> {
  if (message.fromSessionId === APPROVALS_SENDER) return { kind: "user" };
  if (!message.fromMonoId) return host.sessionSender(message.fromSessionId);
  const mono = host.sender(message.fromMonoId);
  return mono && { kind: "mono", ...mono };
}

function queueItem(
  { message, target, carries }: Targeted<Routed>,
  sender: MessageSender | undefined,
  decision?: BoardDecision,
) {
  return {
    id: message.id,
    text: renderMonoMessage({ ...message, carries }, sender),
    link: rowLink(message, target),
    decision,
  };
}

/**
 * Each row's queue item. The senders and the cards of decision rows resolve
 * first, so the rows then queue in one synchronous run.
 */
function queueItems(host: MonoQueueHost, rows: Targeted<Routed>[]) {
  return Promise.all(
    rows.map(async (row) =>
      queueItem(
        row,
        await rowSender(host, row.message),
        row.message.fromSessionId === APPROVALS_SENDER
          ? await boardDecision(row.message.requestId)
          : undefined,
      ),
    ),
  );
}

/**
 * Design 5.3: a sender notice enters the chat once, as a queue row that
 * waits for the turn end; it gets its bubble when it is sent. Its request id
 * in the queue or the transcript means it was queued or sent before, so a
 * pump that runs again adds nothing.
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
  return enqueueQueuedMessage(session, {
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
    id: noticeRequestId(message.id, message.status),
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
  const rows = await invoke<Targeted<Routed>[]>("mono_messages_unqueued", {
    sessionId,
  });
  const items = await queueItems(host, rows);
  rows.forEach((row, index) =>
    host.update(sessionId, (session) =>
      ensureQueued(
        session,
        items[index],
        // A steer only joins the running turn it names (design 16.6), so its
        // item never waits for the turn end.
        row.message.intent === "steer" ? "steer" : "queue",
        host.isMonoChat(sessionId),
      ),
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

/**
 * What a hand-off gives the provider: the row with its handle and its
 * inference line (`text`), and the turn. `shown` is the same row without
 * the line, for a transcript block (design 16.7: the line goes to the agent
 * only), and `link` is what that block names (design 16.3).
 */
export type HandedOff = {
  text: string;
  shown: string;
  turnId: TurnId;
  link?: MessageLink;
};

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
 * means another hand-off has the row, it was already delivered, or it was a
 * steer that missed its turn and Rust dropped it, so the item goes and
 * nothing is sent. A failed hand-off gives the slot back and queues the row
 * again; a steer row is dropped instead (design 16.6), so its item goes.
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
  const assigned = await invoke<Targeted<Waiting> | null>(
    "mono_message_assign_slot",
    { ...slot, kind: mode === "dispatch" ? "submit" : "steer" },
  );
  if (!assigned) {
    host.update(sessionId, (session) => dequeueQueuedMessage(session, id));
    return false;
  }
  const row = { ...assigned.message, carries: assigned.carries };
  const handle = row.status === "open" ? row.slot?.handle : undefined;
  let sender: MessageSender | undefined;
  let accepted = false;
  try {
    sender = await rowSender(host, row);
    accepted = await deliver({
      text: renderMonoMessage(row, sender, handle, {
        sessionId: row.handledBySession,
        turnId,
        target: assigned.target,
      }),
      shown: renderMonoMessage(row, sender, handle),
      turnId,
      link: rowLink(row, assigned.target),
    });
  } finally {
    if (accepted) await invoke("mono_message_mark_delivery", slot);
    else {
      await invoke("mono_message_release", slot);
      host.update(sessionId, (session) =>
        row.intent === "steer"
          ? dequeueQueuedMessage(session, id)
          : ensureQueued(
              session,
              queueItem(assigned, sender),
              "queue",
              host.isMonoChat(sessionId),
            ),
      );
    }
  }
  return accepted;
}

/** `mono_message_drop`'s answer (design 5.5 rule 8). */
type DropAnswer = "dropped" | "closed" | "in_flight";

/**
 * Design 5.5 rule 8: the user removes queue item `itemId`. The item of a
 * Mono row goes only after Rust dropped the row or found it closed; while a
 * hand-off holds the row, the item stays. Returns whether the item went. A
 * board decision Rust dropped was never sent, so its bubble goes too: its
 * line would say it was sent (frame A·7b).
 */
export async function removeQueued(
  host: Pick<MonoQueueHost, "session" | "update">,
  sessionId: string,
  itemId: string,
): Promise<boolean> {
  const item = host
    .session(sessionId)
    ?.queuedMessages?.find((queued) => queued.id === itemId);
  let answer: DropAnswer | undefined;
  if (item?.monoMessageId) {
    answer = await invoke<DropAnswer>("mono_message_drop", {
      id: item.monoMessageId,
    });
    if (answer === "in_flight") return false;
  }
  host.update(sessionId, (session) => {
    const rest = dequeueQueuedMessage(session, itemId);
    if (answer !== "dropped") return rest;
    const blocks = rest.blocks.filter(
      (block) => !(block.boardDecision && block.id === item?.blockId),
    );
    return blocks.length < rest.blocks.length ? { ...rest, blocks } : rest;
  });
  return true;
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
  /**
   * Steers a running project session with `handed.text` and records
   * `handed.shown` as its transcript block, with the hand-off's turn and
   * link; throws when the turn ended.
   */
  steerProject(
    session: Session,
    handed: HandedOff,
    id: MessageId,
  ): Promise<void>;
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
  ownerDelivery(messageId: MessageId): Promise<RouteOutcome | undefined>;
  /** What this window's calls reported, for their duplicates. */
  reported: ReportedDeliveries;
};

/** The route of a new accept: the gate's choice, which the rows do not hold. */
export type Routing = { sessionId: SessionId; via: "queue" | "steer" };

/** The rows of an accept that go to one session; another window may own it. */
type RouteGroup = Routing & { messageIds: MessageId[]; ownerWindow?: string };

/** `mono-message-route`: a route this window runs because it owns the session. */
export type MonoRouteEvent = {
  messages: Targeted<Routed>[];
  route: Routing;
  callerWindow: string;
};

/**
 * What a route did: the delivery, or `dropped` when a steer could not join
 * its turn and Rust dropped the row with a notice (PR I2 review r1 M2).
 */
export type RouteOutcome = Delivery | "dropped";

/** `mono-message-routed`: the owner's answer to the caller window. */
export type MonoRoutedEvent = { messageId: MessageId; delivery: RouteOutcome };

/**
 * Deliveries that owner windows report, by message id. An answer can come
 * before the caller waits for it, so an answer no one waits for is kept for
 * one timeout, then dropped: a late answer to a wait that already timed out
 * is never kept for the life of the window.
 */
export function createOwnerDeliveries(timeoutMs: number) {
  const answers = new Map<MessageId, RouteOutcome>();
  const waiting = new Map<MessageId, (delivery: RouteOutcome) => void>();
  return {
    settle({ messageId, delivery }: MonoRoutedEvent) {
      const waiter = waiting.get(messageId);
      if (waiter) return waiter(delivery);
      answers.set(messageId, delivery);
      setTimeout(() => {
        if (answers.get(messageId) === delivery) answers.delete(messageId);
      }, timeoutMs);
    },
    wait(messageId: MessageId): Promise<RouteOutcome | undefined> {
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
  set(messageId: MessageId, delivery: Promise<RouteOutcome>): void;
  /** Waits for a first call that still routes; undefined when it failed. */
  get(messageId: MessageId): Promise<RouteOutcome | undefined> | undefined;
};

/**
 * The delivery each new accept in this window reported, so a duplicate of
 * the call reports the same. Keeps the newest `limit` entries.
 */
export function createReportedDeliveries(limit: number): ReportedDeliveries {
  const reported = new Map<MessageId, Promise<RouteOutcome | undefined>>();
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
            messageId: payload.messages[0].message.id,
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

/**
 * A row an accepted call wrote. A steer whose turn ended while the gate ran
 * comes back dropped, with no route (design 16.12.3.3).
 */
type Sent = Routed | Extract<MonoMessage, { status: "dropped" }>;

type Accepted = {
  outcome: "accepted";
  messages: Targeted<Sent>[];
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
  tool: "send_message" | "reply" | "ask" | "ack",
  requestId: string,
  input: SendInput | ReplyInput | AskInput | AckInput,
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
 * owns its session. A steer to a project session goes into the turn this
 * window runs, or is dropped; everything else enters the session's queue,
 * whose pump hands it off. Returns what the call reports.
 */
export async function routeMessages(
  host: MonoRouteHost,
  messages: Targeted<Routed>[],
  { sessionId, via }: Routing,
): Promise<RouteOutcome> {
  const bubble = host.isMonoChat(sessionId);
  if (via === "steer" && !bubble)
    return steerProjectSession(host, messages, sessionId);
  // A session no window can load gets its rows when one loads it.
  const loaded = host.session(sessionId) ?? (await host.open(sessionId));
  if (!loaded) return "queue";
  // Rows the load queues are older than these, so they go first.
  await host.reconciled(sessionId);
  const items = await queueItems(host, messages);
  const target = host.session(sessionId) ?? loaded;
  const kind = via === "steer" ? "steer" : "queue";
  const routed = new Set<string>(messages.map((row) => row.message.id));
  // The load may have queued these rows already; they still open the turn.
  const idle =
    !target.busy &&
    (target.queuedMessages ?? []).every((queued) => routed.has(queued.id));
  for (const item of items)
    host.update(sessionId, (session) =>
      ensureQueued(session, item, kind, bubble),
    );
  return kind === "steer" ? "steer" : idle ? "submit" : "queue";
}

/**
 * Design 16.6 (PR I2 review r1 M2): only a Mono chat's queue pump steers,
 * so a steer to a project session goes into the turn this window runs for
 * it now, or nowhere. A hand-off that fails drops the row in Rust; with no
 * turn here the row is dropped now. Either way its sender gets a notice and
 * the call answers `dropped`, never a delivery that will not happen.
 */
async function steerProjectSession(
  host: MonoRouteHost,
  messages: Targeted<Routed>[],
  sessionId: string,
): Promise<RouteOutcome> {
  const loaded = host.session(sessionId) ?? (await host.open(sessionId));
  // The load may queue these rows; they go with the steer or the drop.
  if (loaded) await host.reconciled(sessionId);
  const target = host.session(sessionId) ?? loaded;
  let steered = true;
  for (const { message } of messages) {
    const delivered =
      target && host.turnId(sessionId)
        ? await handOff(
            host,
            sessionId,
            message.id,
            "steer",
            async (handed) => {
              await host.steerProject(target, handed, message.id);
              return true;
            },
          ).catch(() => false)
        : await dropSteer(host, sessionId, message.id);
    steered &&= delivered;
  }
  return steered ? "steer" : "dropped";
}

/**
 * Drops a steer row that no turn here can take, and its item if the load
 * queued one. Answers true only when a hand-off holds the row (`in_flight`),
 * so that hand-off decides what happens to it.
 */
async function dropSteer(
  host: MonoQueueHost,
  sessionId: string,
  id: MessageId,
): Promise<boolean> {
  const answer = await invoke<DropAnswer>("mono_message_drop", { id });
  if (answer === "in_flight") return true;
  host.update(sessionId, (session) => dequeueQueuedMessage(session, id));
  return false;
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
): Promise<RouteOutcome[]> {
  return Promise.all(
    routes.map(async (group) => {
      if (!group.ownerWindow)
        return routeMessages(
          host,
          messages.filter(
            (row): row is Targeted<Routed> =>
              row.message.status !== "dropped" &&
              group.messageIds.includes(row.message.id),
          ),
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
      you:
        resolved.caller.kind !== "session" &&
        mono.id === resolved.caller.monoId,
    })),
  };
}

type Got =
  | { outcome: "found"; message: ReadMessage }
  | {
      outcome: "failed";
      error: {
        code: "invalid_input" | "unknown_handle" | "unknown_message";
        hint: string;
      };
    };

/** `messages.get` (design 16.4): reads one message; writes nothing. */
export async function getMessage(host: MonoMessagingHost, input: GetInput) {
  const resolved = await host.caller();
  if (!resolved.ok) throw new AppActionError(resolved.code, resolved.hint);
  const got = await invoke<Got>("mono_message_get", {
    caller: resolved.caller,
    input,
  });
  if (got.outcome === "failed")
    throw new AppActionError(got.error.code, got.error.hint);
  return { message: got.message };
}

/** `messages.send` (design 3.2). */
export async function sendMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: SendInput,
) {
  return sendAnswer(host, await submit(host, "send_message", requestId, input));
}

/** `messages.ask` (R6): a send that is also a key on the asked child. */
export async function askMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: AskInput,
) {
  return sendAnswer(host, await submit(host, "ask", requestId, input));
}

/** `messages.ack` (R6): a send to the asker that spends the ask's key. */
export async function ackMessage(
  host: MonoMessagingHost,
  requestId: string,
  input: AckInput,
) {
  return sendAnswer(host, await submit(host, "ack", requestId, input));
}

/** What `messages.send`, `messages.ask` and `messages.ack` answer. */
async function sendAnswer(host: MonoMessagingHost, submitted: Accepted) {
  const [{ message }] = submitted.messages;
  const delivery = await sendDelivery(host, submitted, message);
  // A steer that could not join its turn is dropped; the answer says so
  // (PR I2 review r1 M2).
  const dropped = delivery === "dropped";
  return {
    message_id: message.id,
    status: dropped ? "dropped" : message.status,
    delivery,
    hop: message.hop,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}

/**
 * Only the call that got the accept routes (design 5.5 rule 1). A duplicate
 * reports what that call reported in this window, else its stored row's
 * delivery (PR 4a review m3). A steer that Rust dropped after the commit,
 * because its turn ended while the gate ran, comes back dropped with no
 * route (design 16.12.3.3).
 */
async function sendDelivery(
  host: MonoMessagingHost,
  submitted: Accepted,
  message: Sent,
): Promise<RouteOutcome> {
  const stored = message.status === "dropped" ? "dropped" : message.delivery;
  if (submitted.duplicate)
    return (await host.reported.get(message.id)) ?? stored;
  const delivery = route(host, submitted, "wait").then(
    ([routed]) => routed ?? stored,
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
    message_ids: submitted.messages.map((row) => row.message.id),
    answered: submitted.answered,
    ...(submitted.duplicate ? { duplicate: true as const } : {}),
  };
}
