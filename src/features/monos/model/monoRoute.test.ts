// @vitest-environment happy-dom
import { beforeEach, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import {
  dequeueQueuedMessage,
  steerableQueuedMessage,
} from "../../sessions/model/messageQueue";
import {
  deleteSession,
  sanitizeSessionForPersist,
} from "../../sessions/data/sessionStore";
import {
  handleAgentApp,
  type AgentAppHost,
} from "../../agent-app/model/agentApp";
import { appErrorResponse } from "../../agent-app/model/appActionError";
import {
  queuedAppDelivery,
  recordAppSteer,
  TURN_FINISHED_ERROR,
} from "../../../app/model/appSessionSend";
import { acknowledgeMonoMessage } from "./monoMessaging";
import {
  agentMessages,
  createLoadReconciles,
  createOwnerDeliveries,
  createReportedDeliveries,
  dispatchOpener,
  listenForMonoNotices,
  opensTurn,
  listenForMonoRoutes,
  ensureQueued,
  getMessage,
  handOff,
  reconcile,
  removeQueued,
  routeMessages,
  type HandedOff,
  type MonoMessagingHost,
} from "./monoRoute";
import { submitQueued } from "./monoReminders";
import {
  renderMonoMessage,
  renderMonoNotice,
  type Caller,
  type Carries,
  type Handle,
  type MessageId,
  type MonoId,
  type SessionId,
  type TurnId,
} from "./monoMessages";
import { resetMonoSession } from "./monoWorkspace";

/**
 * `mono_messages` as the Rust commands keep it: `submit` accepts every call
 * and routes it to `route`, unless `failure` is set; the gate itself is
 * tested in `mono_gate.rs`. The slot rules are those of `assign_slot`,
 * `release` and `mark_delivery`. A notice is pending (design 5.3) on an
 * unanswered or dropped row that no one was told about, and it belongs to the
 * current chat of the Mono that sent the row.
 */
const rust = vi.hoisted(() => {
  type Row = {
    id: string;
    requestId: string;
    fromMonoId: string | null;
    fromSessionId: string;
    to:
      | { kind: "mono"; monoId: string }
      | { kind: "session"; sessionId: string }
      | null;
    addressed: string;
    body: string;
    hop: number;
    intent: "new" | "steer";
    replyTo: string | null;
    scope: "turn" | null;
    status: "open" | "delivered" | "unanswered" | "dropped";
    wantingReply: boolean;
    handledBySession: string | null;
    delivery: string;
    slot: {
      turnId: string;
      handle?: string;
      deliveredAt: number | null;
    } | null;
    notifiedAt: number | null;
  };
  const state = {
    rows: [] as Row[],
    route: { sessionId: "a-chat", via: "queue" },
    /** The window Rust names as the owner of the route's session, if not this one. */
    ownerWindow: undefined as string | undefined,
    /** The rejection the next `submit` answers. */
    failure: undefined as Record<string, unknown> | undefined,
    /**
     * The route session's turn ended while the gate ran, so `submit` drops
     * the steer it accepted after the commit and gives it no route
     * (design 16.12.3.3).
     */
    turnEnded: false,
    /** `assign_slot` runs once this settles. */
    assigning: undefined as Promise<void> | undefined,
    /** How many `mark_notified` calls fail before one succeeds. */
    failedMarks: 0,
    /** Each Mono's current chat (`monos.data_json.sessionId`). */
    chats: {} as Record<string, string>,
    /** The sessions each window has loaded (`control_loaded_sessions`). */
    loaded: {} as Record<string, string[]>,
    /**
     * `mono_outbox` (R9): each reminder's chat and the prompt and delivery
     * Rust's claim hands out; `afterTurn` is a busy turn that cannot take it.
     */
    reminders: [] as {
      key: string;
      chat: string;
      text: string;
      wake: boolean;
      afterTurn?: boolean;
      claimed?: boolean;
    }[],
    claims: [] as Record<string, unknown>[],
    sentReminders: [] as string[],
    commands: [] as string[],
    submitted: [] as Record<string, unknown>[],
    gets: [] as Record<string, unknown>[],
  };
  const row = (id: string) => state.rows.find((entry) => entry.id === id);
  const held = (entry: Row, turnId: string) =>
    entry.slot?.turnId === turnId && entry.slot.deliveredAt === null;
  // `Targeted`: the row and what its line quotes (`reply_target`), read with
  // its slot. reply_to is a foreign key, so a target this fixture did not
  // write (`ask`'s m_zzzzzzzzzzzz) stands for a row sent before the test.
  // IPC returns a copy, so later changes to the row do not show.
  const targeted = (entry: Row) => {
    const message = structuredClone(entry);
    if (!entry.replyTo) return { message, target: null };
    const target = row(entry.replyTo);
    return {
      message,
      target: target
        ? {
            messageId: target.id,
            body: target.body,
            handledBySession: target.handledBySession,
            slot: target.slot?.handle
              ? { turnId: target.slot.turnId, handle: target.slot.handle }
              : null,
          }
        : {
            messageId: entry.replyTo,
            body: "Earlier work",
            handledBySession: entry.handledBySession,
            slot: null,
          },
    };
  };
  const invoke = async (command: string, args: Record<string, never>) => {
    state.commands.push(command);
    switch (command) {
      case "mono_message_submit": {
        state.submitted.push(args);
        const { caller, requestId, input } = args as {
          caller: { monoId?: string; sessionId: string };
          requestId: string;
          input: {
            message: string;
            wanting_reply: boolean;
            send_to?: string;
            intent?: string;
            reply_to?: string;
            scope?: "turn";
          };
        };
        if (state.failure) return { outcome: "failed", error: state.failure };
        const prior = state.rows.filter(
          (entry) =>
            entry.requestId === requestId &&
            entry.fromSessionId === caller.sessionId,
        );
        if (prior.length)
          return {
            outcome: "accepted",
            messages: prior.map(targeted),
            answered: [],
            duplicate: true,
            routes: [],
          };
        const answering = state.rows.find(
          (entry) =>
            entry.handledBySession === caller.sessionId &&
            entry.status === "open" &&
            entry.slot?.deliveredAt != null,
        );
        const accepted: Row = {
          id: `m_${String.fromCharCode(97 + state.rows.length).repeat(12)}`,
          requestId,
          fromMonoId: caller.monoId ?? null,
          fromSessionId: caller.sessionId,
          to: input.send_to ? { kind: "mono", monoId: input.send_to } : null,
          addressed: input.send_to ?? "",
          body: input.message,
          hop: input.intent === "steer" ? 1 : 0,
          intent: input.intent === "steer" ? "steer" : "new",
          replyTo: input.reply_to ?? null,
          scope: input.scope ?? null,
          status: input.wanting_reply ? "open" : "delivered",
          wantingReply: input.wanting_reply,
          handledBySession: state.route.sessionId,
          delivery: "queue",
          slot: null,
          notifiedAt: null,
        };
        state.rows.push(accepted);
        if (state.turnEnded) {
          accepted.status = "dropped";
          return {
            outcome: "accepted",
            messages: [targeted(accepted)],
            answered: [],
            duplicate: false,
            routes: [],
          };
        }
        return {
          outcome: "accepted",
          messages: [targeted(accepted)],
          answered: answering
            ? [{ handle: "#1", message_id: answering.id }]
            : [],
          duplicate: false,
          routes: [
            {
              ...state.route,
              messageIds: [accepted.id],
              ...(state.ownerWindow ? { ownerWindow: state.ownerWindow } : {}),
            },
          ],
        };
      }
      case "mono_message_assign_slot": {
        await state.assigning;
        const entry = row(args.id);
        if (!entry || (entry.status !== "open" && entry.status !== "delivered"))
          return null;
        const answer = () => targeted(entry);
        if (entry.slot) return held(entry, args.turnId) ? answer() : null;
        // A steer only joins a running turn (design 16.6); an opener
        // hand-off drops it. The turn it names is Rust's to check.
        if (entry.intent === "steer" && args.kind !== "steer") {
          entry.status = "dropped";
          return null;
        }
        // Handles count up from #1 in each turn of the session.
        const taken = state.rows.flatMap((other) =>
          other.handledBySession === entry.handledBySession &&
          other.slot?.turnId === args.turnId &&
          other.slot.handle
            ? [Number(other.slot.handle.slice(1))]
            : [],
        );
        entry.slot = {
          turnId: args.turnId,
          deliveredAt: null,
          ...(entry.wantingReply
            ? { handle: `#${Math.max(0, ...taken) + 1}` }
            : {}),
        };
        entry.delivery = args.kind;
        return answer();
      }
      case "mono_message_release": {
        const entry = row(args.id);
        if (entry && held(entry, args.turnId)) {
          if (entry.intent === "steer") entry.status = "dropped";
          else entry.slot = null;
        }
        return null;
      }
      case "mono_message_mark_delivery": {
        const entry = row(args.id);
        if (entry?.slot && held(entry, args.turnId)) entry.slot.deliveredAt = 1;
        return null;
      }
      // `Inner::route_owner` with no running turn: the first window by
      // label that has the session loaded, else the asking window.
      case "control_owns_session": {
        const holders = Object.keys(state.loaded)
          .filter((label) => state.loaded[label].includes(args.sessionId))
          .sort();
        return (holders[0] ?? args.window) === args.window;
      }
      case "mono_messages_unqueued":
        return state.rows
          .filter(
            (entry) =>
              entry.handledBySession === args.sessionId &&
              !entry.slot &&
              (entry.status === "open" || entry.status === "delivered"),
          )
          .map(targeted);
      case "mono_outbox_pending":
        return state.reminders
          .filter((entry) => entry.chat === args.sessionId && !entry.claimed)
          .map((entry) => entry.key);
      case "mono_outbox_claim": {
        state.claims.push(args);
        const entry = state.reminders.find((r) => r.key === args.key);
        if (!entry || entry.claimed || entry.afterTurn) return null;
        entry.claimed = true;
        return { text: entry.text, wake: entry.wake };
      }
      case "mono_outbox_sent":
        state.sentReminders.push(args.key);
        return true;
      case "mono_messages_pending_notices":
        return state.rows.filter(
          (entry) =>
            (entry.status === "unanswered" || entry.status === "dropped") &&
            entry.notifiedAt === null &&
            entry.fromMonoId !== null &&
            state.chats[entry.fromMonoId] === args.sessionId,
        );
      case "mono_message_mark_notified": {
        if (state.failedMarks > 0) {
          state.failedMarks -= 1;
          throw new Error("database is locked");
        }
        const entry = row(args.id);
        if (entry?.status !== args.status || entry.notifiedAt !== null)
          return false;
        entry.notifiedAt = 2;
        return true;
      }
      // `session_delete` with `end`: the clear in the delete transaction. The
      // session's questions close without a notice, and the rows it still had
      // to take are dropped and leave it.
      case "session_delete": {
        if (!args.end) return null;
        for (const entry of state.rows)
          if (
            entry.fromSessionId === args.sessionId &&
            (entry.status === "open" || entry.status === "unanswered")
          ) {
            entry.status = "dropped";
            entry.notifiedAt = 1;
          }
        const dropped = state.rows.filter(
          (entry) =>
            entry.handledBySession === args.sessionId &&
            (entry.status === "open" ||
              (entry.status === "delivered" && !entry.slot?.deliveredAt)),
        );
        for (const entry of dropped) {
          entry.status = "dropped";
          entry.handledBySession = null;
        }
        const chats = [
          ...new Set(
            dropped.flatMap((entry) =>
              entry.fromMonoId ? [state.chats[entry.fromMonoId]] : [],
            ),
          ),
        ];
        if (chats.length)
          queueMicrotask(() =>
            bus.emit("mono-message-notices", { sessions: chats }),
          );
        return null;
      }
      case "mono_message_drop": {
        const entry = row(args.id);
        if (
          !entry ||
          (entry.status !== "open" && entry.status !== "delivered") ||
          entry.slot?.deliveredAt
        )
          return "closed";
        if (entry.slot) return "in_flight";
        entry.status = "dropped";
        return "dropped";
      }
      case "mono_message_get": {
        state.gets.push(args);
        const { input } = args as { input: { handle: string } };
        const entry = row(input.handle);
        return entry
          ? {
              outcome: "found",
              message: { message_id: entry.id, body: entry.body },
            }
          : {
              outcome: "failed",
              error: {
                code: "unknown_message",
                hint: "No message you can read has that id.",
              },
            };
      }
      // approvals.rs approvals_get: the card a decision row names, decided.
      case "approvals_get":
        return {
          id: args.id,
          number: 3,
          heading: "Ship it",
          status: {
            state: "decided",
            verdict: "approved",
            decidedAt: 1,
            note: null,
            chosen: [],
          },
        };
    }
    throw new Error(`unexpected command ${command}`);
  };
  return { state, invoke, targeted };
});

type Row = (typeof rust.state.rows)[number];

vi.mock("@tauri-apps/api/core", () => ({ invoke: rust.invoke }));

/**
 * Tauri's event routing as `tauri/src/event/listener.rs` does it: a listener
 * on a window gets only events sent to that window's label, and a global
 * `listen` (target `Any`) gets every event, whatever its target. Rust's
 * `app.emit` reaches every listener of every window.
 */
const bus = vi.hoisted(() => {
  type Listener = {
    target: string;
    event: string;
    handler: (event: { payload: unknown }) => void;
  };
  const listeners: Listener[] = [];
  const add = (target: string, event: string, handler: Listener["handler"]) => {
    const listener = { target, event, handler };
    listeners.push(listener);
    return Promise.resolve(() => {
      listeners.splice(listeners.indexOf(listener), 1);
    });
  };
  return {
    current: "main",
    listeners,
    add,
    emitTo(label: string, event: string, payload: unknown) {
      for (const listener of [...listeners])
        if (
          listener.event === event &&
          (listener.target === "Any" || listener.target === label)
        )
          listener.handler({ payload });
      return Promise.resolve();
    },
    emit(event: string, payload: unknown) {
      for (const listener of [...listeners])
        if (listener.event === event) listener.handler({ payload });
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (event: { payload: unknown }) => void) =>
    bus.add("Any", event, handler),
  emitTo: bus.emitTo,
  emit: () => Promise.resolve(),
}));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => {
    const label = bus.current;
    return {
      label,
      listen: (event: string, handler: (event: { payload: unknown }) => void) =>
        bus.add(label, event, handler),
    };
  },
}));

beforeEach(() => {
  bus.listeners.length = 0;
  rust.state.rows = [];
  rust.state.route = { sessionId: "a-chat", via: "queue" };
  rust.state.ownerWindow = undefined;
  rust.state.failure = undefined;
  rust.state.turnEnded = false;
  rust.state.assigning = undefined;
  rust.state.failedMarks = 0;
  rust.state.chats = { giga: "giga-chat", "mono-a": "a-chat" };
  rust.state.loaded = {};
  rust.state.commands = [];
  rust.state.reminders = [];
  rust.state.claims = [];
  rust.state.sentReminders = [];
  rust.state.submitted = [];
  rust.state.gets = [];
});

const chat = (id: string, change: Partial<Session> = {}): Session => ({
  ...newSession("claude", "/Users/me", "claude:opus"),
  id,
  ...change,
});

/**
 * A window with `loaded` sessions, reconciled when it loaded them, before
 * any row of a test existed. `open` loads one of `unloaded` once `opening`
 * settles, and the window reconciles it after the render, as App's effect
 * does. The window runs a turn of each busy session.
 */
function world(
  loaded: Session[],
  unloaded: Session[] = [],
  {
    caller = "giga" as "giga" | "mono-a" | Caller,
    stored = [] as Session[],
    ownerDeliveries = createOwnerDeliveries(50),
    label = "main",
    opening = Promise.resolve(),
    /** The lead and worker sessions `sessionSender` knows, by id. */
    senders = {} as Record<string, { title: string; level: "lead" | "worker" }>,
  } = {},
) {
  let sessions = loaded;
  rust.state.loaded[label] = loaded.map((session) => session.id);
  const steerProject = vi.fn(async () => {});
  const open = vi.fn(async (id: string) => {
    await opening;
    const found = unloaded.find((session) => session.id === id) ?? null;
    if (found) {
      sessions = [...sessions, found];
      rust.state.loaded[label] = sessions.map((session) => session.id);
      queueMicrotask(() => void loads.of(id));
    }
    return found;
  });
  const monos = [
    { id: "giga" as MonoId, name: "Boss", role: "giga" as const },
    { id: "mono-a" as MonoId, name: "A", role: "mono" as const },
  ];
  const host: MonoMessagingHost = {
    session: (id) => sessions.find((session) => session.id === id),
    update: (id, change) => {
      sessions = sessions.map((session) =>
        session.id === id ? change(session) : session,
      );
    },
    isMonoChat: (id) => id.endsWith("-chat"),
    sender: (monoId) => monos.find((mono) => mono.id === monoId),
    sessionSender: async (id) =>
      senders[id] ? { kind: "session", ...senders[id] } : undefined,
    turnId: (id) =>
      host.session(id)?.busy ? (`run-${id}` as TurnId) : undefined,
    owns: (id) =>
      rust.invoke("control_owns_session", {
        sessionId: id,
        window: label,
      } as never),
    reconciled: (id) =>
      loaded.some((session) => session.id === id)
        ? Promise.resolve()
        : loads.of(id),
    caller: async () => ({
      ok: true,
      caller:
        typeof caller === "object"
          ? caller
          : {
              kind: "mono_chat",
              monoId: (caller === "giga" ? "giga" : "mono-a") as MonoId,
              sessionId: (caller === "giga"
                ? "giga-chat"
                : "a-chat") as SessionId,
              turnId: "t" as TurnId,
            },
    }),
    sessions: () => sessions,
    stored: async (id) => stored.find((session) => session.id === id) ?? null,
    open,
    steerable: () => true,
    steerableHarnesses: () => ["claude"],
    ownerDelivery: (messageId) => ownerDeliveries.wait(messageId),
    reported: createReportedDeliveries(16),
    ownProject: () => true,
    monos: () => monos,
    steerProject,
  };
  const loads = createLoadReconciles(host);
  const app = { messages: agentMessages(host) } as unknown as AgentAppHost;
  const source = chat(
    typeof caller === "object"
      ? caller.sessionId
      : caller === "giga"
        ? "giga-chat"
        : "a-chat",
  );
  return {
    host,
    open,
    steerProject,
    get: (id: string) => sessions.find((session) => session.id === id)!,
    send: (input: Record<string, unknown>, requestId = "r1") =>
      handleAgentApp(source, requestId, "messages.send", input, app),
    run: (
      action: string,
      input: Record<string, unknown> = {},
      requestId = "r9",
    ) => handleAgentApp(source, requestId, action, input, app),
  };
}

const ask = (wanting_reply = false, intent: "new" | "steer" = "new") => ({
  message: "Review PR 12",
  send_to: "mono-a",
  wanting_reply,
  intent,
  ...(intent === "steer" ? { reply_to: "m_zzzzzzzzzzzz" } : {}),
});

/** A message from the Giga's chat to Mono A that ended as `change` says. */
const ended = (change: Partial<Row>): Row => ({
  id: "m_aaaaaaaaaaaa",
  requestId: "r0",
  fromMonoId: "giga",
  fromSessionId: "giga-chat",
  to: { kind: "mono", monoId: "mono-a" },
  addressed: "mono-a",
  body: "Is CI\ngreen?",
  hop: 0,
  intent: "new",
  status: "unanswered",
  wantingReply: true,
  handledBySession: "a-chat",
  delivery: "submit",
  slot: { turnId: "t-gone", handle: "#1", deliveredAt: 1 },
  notifiedAt: null,
  ...change,
});

const ids = (session: Session) =>
  (session.queuedMessages ?? []).map((item) => item.id);

// Defect: an idle Mono's message waits in its queue without saying it will
// start a turn, or enters the queue twice, or keeps no link to its row.
it("starts an idle Mono's turn with the message, as one item named by its row", async () => {
  const { send, get } = world([chat("a-chat")]);
  const result = await send(ask());
  expect(result).toEqual({
    message_id: "m_aaaaaaaaaaaa",
    status: "delivered",
    delivery: "submit",
    hop: 0,
  });
  const [item] = get("a-chat").queuedMessages ?? [];
  expect(item).toMatchObject({
    id: "m_aaaaaaaaaaaa",
    monoMessageId: "m_aaaaaaaaaaaa",
    blockId: "m_aaaaaaaaaaaa",
    waitForIdle: true,
  });
  expect(item.text).toContain('message_id="m_aaaaaaaaaaaa"');
  expect(item.text).toContain('wanting_reply="false"');
  expect(item.text).toContain("Review PR 12");
  expect(
    get("a-chat").blocks.filter((block) => block.id === item.id),
  ).toHaveLength(1);
});

// Defect: a queued message for a busy Mono steers its running turn.
it("queues a message for a busy Mono behind its running turn", async () => {
  const { send, get } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  expect(await send(ask(true))).toMatchObject({
    status: "open",
    delivery: "queue",
  });
  expect(get("a-chat").queuedMessages?.[0].waitForIdle).toBe(true);
  expect(
    steerableQueuedMessage(get("a-chat"), "run-a-chat" as TurnId),
  ).toBeUndefined();
});

// ADE-78. Defect: a message for a Mono whose queue the relaunch paused
// answers "queue", as if the turn end would send it; it waits for Resume.
it("tells the sender a message joined a paused queue, and keeps it for Resume", async () => {
  const { send, get } = world([
    chat("a-chat", {
      queueStatus: "paused",
      queuedMessages: [
        {
          id: "before",
          text: "Before the quit",
          attachments: [],
          pausedBy: "quit",
        },
      ],
    }),
  ]);
  expect(await send(ask())).toMatchObject({
    status: "delivered",
    delivery: "paused",
  });
  expect(get("a-chat")).toMatchObject({ queueStatus: "paused" });
  expect(ids(get("a-chat"))).toEqual(["before", "m_aaaaaaaaaaaa"]);
});

// Defect: a Mono steer waits for the turn end (it never reaches the running
// turn), or a waiting item ahead of it blocks it.
it("steers a busy, ready Mono chat with the message, past a waiting item", async () => {
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  const waiting = {
    id: "later",
    text: "After this turn",
    attachments: [],
    waitForIdle: true,
  };
  const { send, get } = world([
    chat("a-chat", {
      busy: true,
      turnReadiness: "ready",
      queuedMessages: [waiting],
    }),
  ]);
  expect(await send(ask(false, "steer"))).toMatchObject({ delivery: "steer" });
  expect(ids(get("a-chat"))).toEqual(["later", "m_aaaaaaaaaaaa"]);
  expect(
    steerableQueuedMessage(get("a-chat"), "run-a-chat" as TurnId)?.id,
  ).toBe("m_aaaaaaaaaaaa");
});

// Defect: the second steer reaches the running turn first (head placement).
it("steers two messages into a starting turn in the order they came, once it is ready", async () => {
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  const { send, get, host } = world([
    chat("a-chat", { busy: true, turnReadiness: "starting" }),
  ]);
  await send({ ...ask(false, "steer"), message: "First" }, "r1");
  await send({ ...ask(false, "steer"), message: "Second" }, "r2");
  expect(
    steerableQueuedMessage(get("a-chat"), "run-a-chat" as TurnId),
  ).toBeUndefined();
  host.update("a-chat", (session) => ({ ...session, turnReadiness: "ready" }));
  const first = steerableQueuedMessage(get("a-chat"), "run-a-chat" as TurnId);
  expect(first?.text).toContain("First");
  host.update("a-chat", (session) => dequeueQueuedMessage(session, first!.id));
  expect(
    steerableQueuedMessage(get("a-chat"), "run-a-chat" as TurnId)?.text,
  ).toContain("Second");
});

// Defect: a retried call routes again, so a message already handed off
// comes back into the receiver's queue.
it("answers a duplicate call with its row and queues nothing again", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  const first = await send(ask());
  const item = get("a-chat").queuedMessages![0];
  await handOff(host, "a-chat", item.id as MessageId, "dispatch", () => {
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return true;
  });
  expect(await send(ask())).toEqual({ ...(first as object), duplicate: true });
  expect(get("a-chat").queuedMessages).toBeUndefined();
});

// Defect: the load that the route causes reconciles the new row and the route
// queues it again, or both hand-offs deliver it.
it("routes into a chat this window had not loaded with one item and one delivery", async () => {
  const { send, get, host } = world([], [chat("a-chat")]);
  await send(ask());
  expect(ids(get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
  const item = get("a-chat").queuedMessages![0];
  const deliver = vi.fn(() => {
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return true;
  });
  const id = item.id as MessageId;
  expect(await handOff(host, "a-chat", id, "dispatch", deliver)).toBe(true);
  // A stale copy of the item, as a reload or a second reconcile leaves it.
  host.update("a-chat", (session) =>
    ensureQueued(session, { id, text: item.text }, "queue", true),
  );
  expect(await handOff(host, "a-chat", id, "dispatch", deliver)).toBe(false);
  expect(deliver).toHaveBeenCalledTimes(1);
  expect(ids(get("a-chat"))).toEqual([]);
});

// Design 16.6 (PR I2 review r1 M2). Defect: a steer that finds the turn
// over waits in the queue and opens the next turn as new work, or the call
// answers `queue`, so the model waits for a steer that will never arrive.
it("answers dropped for a project-session steer whose turn just finished", async () => {
  rust.state.route = { sessionId: "project", via: "steer" };
  const { send, get, steerProject } = world([chat("project", { busy: true })]);
  steerProject.mockRejectedValueOnce(new Error(TURN_FINISHED_ERROR));
  expect(await send(ask(false, "steer"))).toMatchObject({
    status: "dropped",
    delivery: "dropped",
  });
  expect(rust.state.commands).toContain("mono_message_release");
  expect(get("project").queuedMessages).toBeUndefined();
});

// PR I4 review r1 m1 (the reviewer's probe). Defect: a project-session steer
// records the text with its inference line as the transcript block, so the
// line shows in the session before PR I6, or the agent gets no line.
it("steers a project session with the line and records the block without it", async () => {
  const { send, get, host, steerProject } = world([
    chat("project", { busy: true }),
  ]);
  rust.state.route = { sessionId: "project", via: "queue" };
  await send({
    ...ask(),
    message: "Refactor the login flow",
    send_to: "project",
  });
  const target = rust.state.rows[0].id as MessageId;
  await handOff(host, "project", target, "steer", () => true);
  rust.state.route = { sessionId: "project", via: "steer" };
  await send(
    {
      message: "Keep the API stable",
      send_to: "project",
      wanting_reply: false,
      intent: "steer",
      reply_to: target,
    },
    "r2",
  );
  const [[, handed, id]] = steerProject.mock.calls as unknown as [
    [Session, { text: string; shown: string }, string],
  ];
  expect(handed.text).toContain(
    `reply_to="${target}">\n[steer → ${target} · "Refactor the login flow"]\nKeep the API stable\n`,
  );
  const block = recordAppSteer(get("project"), handed.shown, id).blocks.at(-1);
  expect(block?.text).toBe(
    `<mono_message message_id="${id}" from="Boss" mono_id="giga" role="giga" wanting_reply="false">\nKeep the API stable\n</mono_message>`,
  );
});

// PR I5 (design 16.3, 16.8). Defect: a Mono steer's block in a project
// session has no turn id, so it is never a running item a user can steer,
// or no link, so its quote can never show.
it("records a project-session steer with the turn it joined and the message it names", async () => {
  const { send, get, host, steerProject } = world([
    chat("project", { busy: true }),
  ]);
  rust.state.route = { sessionId: "project", via: "queue" };
  await send({
    ...ask(),
    message: "Refactor the login flow",
    send_to: "project",
  });
  const target = rust.state.rows[0].id as MessageId;
  await handOff(host, "project", target, "steer", () => true);
  rust.state.route = { sessionId: "project", via: "steer" };
  await send(
    {
      message: "Keep the API stable",
      send_to: "project",
      wanting_reply: false,
      intent: "steer",
      reply_to: target,
    },
    "r2",
  );
  const [[, handed, id]] = steerProject.mock.calls as unknown as [
    [Session, HandedOff, string],
  ];
  const block = recordAppSteer(get("project"), handed.shown, id, {
    turnId: handed.turnId,
    link: handed.link,
  }).blocks.at(-1);
  expect(block).toMatchObject({
    turnId: "run-project",
    link: {
      intent: "steer",
      replyTo: target,
      quote: "Refactor the login flow",
    },
  });
});

// PR I5 (PR I4 review r1 m4, design 16.3). Defect: the route payload has no
// target, so a Mono steer's bubble has no link; or a new row gets one; or
// the queue item carries it, where it would read as the user's own link.
it("links a routed steer's bubble to the message it names, and a new row's bubble to none", async () => {
  const { send, get } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  await send({
    ...ask(true),
    message: 'Fix the "login" bug\nin auth.ts — then run the tests and report',
  });
  const target = rust.state.rows[0].id as MessageId;
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send(
    {
      message: "Keep the API stable",
      send_to: "mono-a",
      wanting_reply: false,
      intent: "steer",
      reply_to: target,
    },
    "r2",
  );
  const steer = rust.state.rows[1].id;
  const bubble = (id: string) =>
    get("a-chat").blocks.find((block) => block.id === id);
  expect(bubble(steer)?.link).toEqual({
    intent: "steer",
    replyTo: target,
    quote: 'Fix the "login" bug in auth.ts — then…',
  });
  expect(bubble(target)).not.toHaveProperty("link");
  expect(
    get("a-chat").queuedMessages?.find((item) => item.id === steer),
  ).not.toHaveProperty("link");
});

// PR I5 (PR I4 review r1 m4). Defect: only routed bubbles have a link, so
// after a reload the bubble reconcile queues has none.
it("links a steer's bubble that reconcile queues after a reload as a route would", async () => {
  const { send, get, host } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  await send({ ...ask(true), message: "Refactor the login flow" });
  const target = rust.state.rows[0].id as MessageId;
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send(
    {
      message: "Keep the API stable",
      send_to: "mono-a",
      wanting_reply: false,
      intent: "steer",
      reply_to: target,
    },
    "r2",
  );
  const steer = rust.state.rows[1].id;
  host.update("a-chat", (session) => ({
    ...session,
    blocks: session.blocks.filter((block) => block.id !== steer),
    queuedMessages: [],
  }));
  await reconcile(host, "a-chat");
  expect(
    get("a-chat").blocks.find((block) => block.id === steer)?.link,
  ).toEqual({
    intent: "steer",
    replyTo: target,
    quote: "Refactor the login flow",
  });
});

// Design 16.12.6 (PR I5 builds the data). Defect: a whole-turn steer's
// bubble cannot be labelled Steer · whole turn.
it("links a whole-turn steer's bubble to the whole turn", async () => {
  const { send, get } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send({
    message: "Stop and commit",
    send_to: "mono-a",
    wanting_reply: false,
    intent: "steer",
    scope: "turn",
  });
  expect(get("a-chat").blocks.at(-1)?.link).toEqual({
    intent: "steer",
    scope: "turn",
  });
});

// Design 5.5 rule 4. Defect: a new message whose steer into a project
// session failed is lost, or keeps its slot so nothing queues it again.
it("queues a failed hand-off of new work again", async () => {
  const { send, get, host } = world([chat("project")]);
  rust.state.route = { sessionId: "project", via: "queue" };
  await send({ ...ask(), send_to: "project" });
  const id = "m_aaaaaaaaaaaa" as MessageId;
  host.update("project", (session) => dequeueQueuedMessage(session, id));
  expect(await handOff(host, "project", id, "dispatch", () => false)).toBe(
    false,
  );
  expect(rust.state.rows[0]).toMatchObject({ status: "delivered", slot: null });
  expect(get("project").queuedMessages).toEqual([
    expect.objectContaining({ id, waitForIdle: true }),
  ]);
});

// Defect: a project steer that the provider took is delivered again later.
it("marks a project-session steer the provider took, so nothing queues it again", async () => {
  rust.state.route = { sessionId: "project", via: "steer" };
  const { send, get, host } = world([chat("project", { busy: true })]);
  expect(await send(ask(false, "steer"))).toMatchObject({ delivery: "steer" });
  expect(rust.state.rows[0].slot?.deliveredAt).toBe(1);
  await reconcile(host, "project");
  expect(get("project").queuedMessages).toBeUndefined();
});

// Defect: reconcile adds a row twice, or as an item that steers.
it("reconciles a row no item holds once, as an item that waits for the turn end", async () => {
  const { send, get, host } = world([chat("a-chat", { busy: true })]);
  await send(ask());
  host.update("a-chat", (session) =>
    dequeueQueuedMessage(session, "m_aaaaaaaaaaaa"),
  );
  await reconcile(host, "a-chat");
  await reconcile(host, "a-chat");
  expect(get("a-chat").queuedMessages).toEqual([
    expect.objectContaining({ id: "m_aaaaaaaaaaaa", waitForIdle: true }),
  ]);
  expect(
    get("a-chat").blocks.filter((block) => block.id === "m_aaaaaaaaaaaa"),
  ).toHaveLength(1);
});

// R9. Defects: a reminder that reaches its chat twice, in another text than
// the one Rust's claim hands out, as the user's visible turn, or as a steer
// in R9b; one claimed for a chat this window does not hold, or queued when
// Rust leaves it for the turn end; one sent into a turn no provider can take
// (it leaves the queue and keeps its claim); a sent mark for a turn the
// provider refused, or for an item that is no reminder.
it("queues each reminder Rust hands this window once, as a hidden prompt, and marks it sent when the provider takes it", async () => {
  const { get, host } = world([chat("lead-1"), chat("lead-2", { busy: true })]);
  const [idle, busy, gone] = [
    "m_aaaaaaaaaaaa",
    "m_bbbbbbbbbbbb",
    "m_cccccccccccc",
  ];
  const text = `<mono_reminder ask_id="${idle}" stage="t5">\nAsk ${idle} is due.\n</mono_reminder>`;
  rust.state.reminders = [
    { key: `reminder:${idle}:t5`, chat: "lead-1", text, wake: true },
    {
      key: `reminder:${busy}:t5`,
      chat: "lead-2",
      text: "B",
      wake: true,
      afterTurn: true,
    },
    { key: `reminder:${gone}:t5`, chat: "lead-3", text: "C", wake: true },
  ];
  for (const id of ["lead-1", "lead-1", "lead-2", "lead-3"])
    await reconcile(host, id);

  const key = `reminder:${idle}:t5`;
  const item = get("lead-1").queuedMessages![0];
  expect(get("lead-1").queuedMessages).toEqual([
    {
      id: key,
      appRequestId: key,
      text,
      attachments: [],
      monoReminder: true,
      waitForIdle: true,
      queuedAt: expect.any(Number),
    },
  ]);
  expect(get("lead-2").queuedMessages ?? []).toEqual([]);
  expect(rust.state.claims).toEqual([
    { key, steerable: false },
    { key: `reminder:${busy}:t5`, steerable: false },
  ]);
  expect(queuedAppDelivery(item)).toEqual({
    appRequestId: key,
    appDelivery: { via: "queued", queuedMessageId: key },
    internal: true,
  });

  const submits: { providerTook?: (accepted: boolean) => void }[] = [];
  const submit = (options: (typeof submits)[number]) => submits.push(options);
  expect(submitQueued(item, false, submit)).toBe(false);
  expect(submits).toEqual([]);
  submitQueued(item, true, submit);
  submits[0].providerTook!(false);
  expect(rust.state.sentReminders).toEqual([]);
  submits[0].providerTook!(true);
  await vi.waitFor(() => expect(rust.state.sentReminders).toEqual([key]));
  const notice = {
    ...item,
    id: "n",
    appRequestId: "n",
    monoReminder: undefined,
  };
  submitQueued(notice, false, submit);
  expect(submits[1]).toEqual({});
  expect(queuedAppDelivery(notice)).toEqual({
    appRequestId: "n",
    appDelivery: { via: "queued", queuedMessageId: "n" },
  });
});

// Defect: an opener that failed after its dequeue is lost, or comes back as
// two items and two bubbles.
it("queues a failed opener again once, under the bubble it already has", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  await send(ask());
  const item = get("a-chat").queuedMessages![0];
  const id = item.id as MessageId;
  const accepted = await handOff(host, "a-chat", id, "dispatch", () => {
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return false;
  });
  expect(accepted).toBe(false);
  expect(rust.state.rows[0].slot).toBeNull();
  await reconcile(host, "a-chat");
  expect(get("a-chat").queuedMessages).toEqual([
    expect.objectContaining({ id: item.id, blockId: item.id }),
  ]);
  expect(
    get("a-chat").blocks.filter((block) => block.id === item.id),
  ).toHaveLength(1);
});

// Defect: a hand-off sends a row that another turn's hand-off holds.
it("drops an item whose row another turn holds and sends nothing", async () => {
  const { send, get, host } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send(ask(false, "steer"));
  const item = get("a-chat").queuedMessages![0];
  const id = item.id as MessageId;
  // A steer into the running turn is in flight when the drain opens the
  // next turn with it.
  let finishSteer!: (accepted: boolean) => void;
  const steering = handOff(
    host,
    "a-chat",
    id,
    "steer",
    () => new Promise<boolean>((resolve) => (finishSteer = resolve)),
  );
  await vi.waitFor(() => expect(finishSteer).toBeDefined());
  const deliver = vi.fn(() => true);
  expect(await handOff(host, "a-chat", id, "dispatch", deliver)).toBe(false);
  expect(deliver).not.toHaveBeenCalled();
  expect(ids(get("a-chat"))).toEqual([]);
  finishSteer(true);
  expect(await steering).toBe(true);
  expect(rust.state.rows[0].slot?.deliveredAt).toBe(1);
});

// Defect: a restart forgets which row an item delivers, so it is sent as a
// plain message and its row is never handed off.
it("keeps an item's message id across a save and a restore", async () => {
  const { send, get } = world([chat("a-chat", { busy: true })]);
  await send(ask());
  const [saved] = sanitizeSessionForPersist(get("a-chat")).queuedMessages ?? [];
  expect(saved).toMatchObject({
    id: "m_aaaaaaaaaaaa",
    monoMessageId: "m_aaaaaaaaaaaa",
    waitForIdle: true,
  });
  const forged = sanitizeSessionForPersist({
    ...get("a-chat"),
    queuedMessages: [
      { ...saved, monoMessageId: "m_bbbbbbbbbbbb" as MessageId },
    ],
  }).queuedMessages?.[0];
  expect(forged?.monoMessageId).toBeUndefined();
});

it("lists every Mono for messages.get_monos and marks the caller", async () => {
  const { run } = world([]);
  expect(await run("messages.get_monos")).toEqual({
    monos: [
      { mono_id: "giga", mono_name: "Boss", role: "giga", you: true },
      { mono_id: "mono-a", mono_name: "A", role: "mono", you: false },
    ],
  });
});

// Design 16.4. Defect: messages.get never reaches Rust with the caller's
// turn, or a refusal comes back as a message instead of its code and hint.
it("reads a message for messages.get with the caller and answers Rust's code on a refusal", async () => {
  const { send, run } = world([chat("a-chat")]);
  await send(ask());
  const [sent] = rust.state.rows;
  expect(await run("messages.get", { handle: sent.id })).toEqual({
    message: { message_id: sent.id, body: sent.body },
  });
  expect(rust.state.gets[0]).toEqual({
    caller: {
      kind: "mono_chat",
      monoId: "giga",
      sessionId: "giga-chat",
      turnId: "t",
    },
    input: { handle: sent.id },
  });
  await expect(
    run("messages.get", { handle: "m_zzzzzzzzzzzz" }),
  ).rejects.toMatchObject({
    code: "unknown_message",
    hint: "No message you can read has that id.",
  });
});

// PR I3 review r1 m4. Defect: a project session, which resolveCaller
// answers with not_a_mono, still reaches Rust and reads a message.
it("answers not_a_mono to messages.get from a project session and reads nothing", async () => {
  const { host } = world([]);
  const session = {
    ...host,
    caller: async () =>
      ({
        ok: false,
        code: "not_a_mono",
        hint: "Only a Mono's chat or its habit runs can use Mono messaging.",
      }) as const,
  };
  await expect(
    getMessage(session, { handle: "m_aaaaaaaaaaaa" as MessageId }),
  ).rejects.toMatchObject({ code: "not_a_mono" });
  expect(rust.state.commands).not.toContain("mono_message_get");
});

// Review PR 4a M1. Defect: an opener counts as delivered once the turn
// starts, so a turn that no provider takes loses the message for good.
it("keeps an opener undelivered and queues it again when no provider takes the turn", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  await send(ask());
  const item = get("a-chat").queuedMessages![0];
  let providerTook!: (accepted: boolean) => void;
  const started = await dispatchOpener(
    host,
    "a-chat",
    item.id as MessageId,
    (_, took) => {
      providerTook = took;
      // submitSession's commit takes the item out and starts the turn.
      host.update("a-chat", (session) =>
        dequeueQueuedMessage(session, item.id),
      );
      return true;
    },
  );
  expect(started).toBe(true);
  expect(rust.state.rows[0].slot?.deliveredAt).toBeNull();
  providerTook(false);
  await vi.waitFor(() =>
    expect(get("a-chat").queuedMessages).toEqual([
      expect.objectContaining({ id: item.id, blockId: item.id }),
    ]),
  );
  expect(rust.state.rows[0].slot).toBeNull();
});

it("marks an opener delivered once the provider takes its turn", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  await send(ask());
  const item = get("a-chat").queuedMessages![0];
  let providerTook!: (accepted: boolean) => void;
  await dispatchOpener(host, "a-chat", item.id as MessageId, (_, took) => {
    providerTook = took;
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return true;
  });
  providerTook(true);
  await vi.waitFor(() => expect(rust.state.rows[0].slot?.deliveredAt).toBe(1));
  expect(get("a-chat").queuedMessages).toBeUndefined();
});

// Review PR 4a M2. Defect: the caller window loads its own copy of a session
// another window owns and queues the message there.
it("leaves a route to the window that owns the session and runs one sent to it", async () => {
  rust.state.ownerWindow = "workspace-2";
  const caller = world([], [chat("a-chat")]);
  expect(await caller.send(ask())).toMatchObject({ delivery: "queue" });
  expect(caller.open).not.toHaveBeenCalled();
  expect(caller.host.session("a-chat")).toBeUndefined();
  const owner = world([chat("a-chat", { busy: true })]);
  await routeMessages(owner.host, rust.state.rows.map(rust.targeted) as never, {
    sessionId: "a-chat" as SessionId,
    via: "queue",
  });
  expect(ids(owner.get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
});

// Review PR 4a M6. Defect: a reply is never delivered, a retried reply is
// delivered twice, or it asks for an answer it did not ask for.
it("delivers a reply once to the sender's chat, as a message that wants no answer", async () => {
  rust.state.route = { sessionId: "giga-chat", via: "queue" };
  const { run, get, host } = world([chat("giga-chat", { busy: true })], [], {
    caller: "mono-a",
  });
  const first = await run("messages.reply", { message: "CI is green" }, "re-1");
  expect(first).toEqual({ message_ids: ["m_aaaaaaaaaaaa"], answered: [] });
  const [item] = get("giga-chat").queuedMessages ?? [];
  expect(item.text).toContain('wanting_reply="false"');
  expect(item.text).toContain("CI is green");
  host.update("giga-chat", (session) => dequeueQueuedMessage(session, item.id));
  await handOff(host, "giga-chat", item.id as MessageId, "steer", () => true);
  expect(
    await run("messages.reply", { message: "CI is green" }, "re-1"),
  ).toEqual({
    ...first,
    duplicate: true,
  });
  expect(get("giga-chat").queuedMessages).toBeUndefined();
});

// PR 5 review m1. Defect: a reply waits for each other window's delivery
// report in turn, up to the owner timeout each, though it reports none.
it("answers a reply routed by another window without waiting for its report", async () => {
  rust.state.route = { sessionId: "giga-chat", via: "queue" };
  rust.state.ownerWindow = "workspace-2";
  const ownerDeliveries = createOwnerDeliveries(60_000);
  const wait = vi.spyOn(ownerDeliveries, "wait");
  const { run } = world([], [], { caller: "mono-a", ownerDeliveries });
  expect(await run("messages.reply", { message: "CI is green" })).toEqual({
    message_ids: ["m_aaaaaaaaaaaa"],
    answered: [],
  });
  expect(wait).not.toHaveBeenCalled();
});

// PR 5 review m6 (T-e). Defect: a steer hand-off with no turn of the
// session in this window drops the item, so the row waits with no item and
// no slot until a reload.
it("keeps a steer's item when this window runs no turn of the session", async () => {
  rust.state.rows = [ended({ status: "open", slot: null, delivery: "queue" })];
  const { host, get } = world([chat("a-chat")]);
  await reconcile(host, "a-chat");
  const deliver = vi.fn(() => true);
  expect(
    await handOff(
      host,
      "a-chat",
      "m_aaaaaaaaaaaa" as MessageId,
      "steer",
      deliver,
    ),
  ).toBe(false);
  expect(deliver).not.toHaveBeenCalled();
  expect(ids(get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
  expect(rust.state.commands).not.toContain("mono_message_assign_slot");
});

// PR 5 review m6 (T-f). Defect: an opener whose session turned busy since
// the hand-off took its slot is submitted into the running turn.
it("lets an opener's turn start only in a session that runs none", () => {
  const turnId = "t-new" as TurnId;
  expect(opensTurn(chat("a-chat", { busy: true }), turnId)).toBe(false);
  expect(opensTurn(chat("a-chat"), turnId)).toBe(true);
  expect(opensTurn(chat("a-chat", { busy: true }), undefined)).toBe(true);
});

it("renders the tags the agents read", () => {
  const message = {
    id: "m_aaaaaaaaaaaa" as MessageId,
    fromMonoId: "giga" as MonoId,
    fromSessionId: "giga-chat" as SessionId,
    body: "Is CI green?",
    wantingReply: true,
    intent: "new" as const,
    replyTo: null,
    scope: null,
    carries: null,
  };
  const sender = {
    kind: "mono" as const,
    name: 'Boss "B"',
    role: "giga" as const,
  };
  expect(renderMonoMessage(message, sender)).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="Boss &#34;B&#34;" mono_id="giga" role="giga" wanting_reply="true">\nIs CI green?\n</mono_message>',
  );
  expect(renderMonoMessage(message, sender, "#2" as Handle)).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="Boss &#34;B&#34;" mono_id="giga" role="giga" wanting_reply="true" handle="#2">\nIs CI green?\n</mono_message>',
  );
  // Design 5.3: 60 code points of the message on one line; an emoji is one.
  expect(
    renderMonoNotice(
      {
        id: "m_aaaaaaaaaaaa" as MessageId,
        body: `${"x".repeat(58)}\n\n🦀🦀🦀`,
        status: "dropped",
      },
      "Scout <S>",
    ),
  ).toBe(
    `<mono_notice message_id="m_aaaaaaaaaaaa" status="dropped" to="Scout &#60;S&#62;">\n${"x".repeat(58)} 🦀\n</mono_notice>`,
  );
});

const ledgerRow = (carries: Carries | null) => ({
  id: "m_aaaaaaaaaaaa" as MessageId,
  fromMonoId: "mono-a" as MonoId,
  fromSessionId: "mono-chat" as SessionId,
  body: "Go",
  wantingReply: false,
  intent: "new" as const,
  replyTo: null,
  scope: null,
  carries,
});
const agentReading = {
  sessionId: "worker-1" as SessionId,
  turnId: "t-1" as TurnId,
  target: null,
};

// Exists only for gate/mutation (R6 one-off): the real-app scenario proves the
// tags reach the agent; this pins the rounding and each kind's attributes.
it("gives the agent an ask's ask_id and due_in, and an ack's ack_of", () => {
  const tags = (carries: Carries | null) =>
    renderMonoMessage(ledgerRow(carries), undefined, undefined, agentReading)
      .split("\n")[0]
      .replace(/^.*wanting_reply="false"/, "");
  const ask = (dueInMs: number | null) => tags({ kind: "ask", dueInMs });
  expect(ask(29 * 60_000 + 1)).toBe(' ask_id="m_aaaaaaaaaaaa" due_in="30m">');
  expect(ask(30 * 60_000)).toBe(' ask_id="m_aaaaaaaaaaaa" due_in="30m">');
  expect(ask(-90_000)).toBe(' ask_id="m_aaaaaaaaaaaa" due_in="0m">');
  expect(ask(null)).toBe(' ask_id="m_aaaaaaaaaaaa">');
  expect(tags({ kind: "ack", askId: "m_bbbbbbbbbbbb" as MessageId })).toBe(
    ' ack_of="m_bbbbbbbbbbbb">',
  );
  const ask2 = "m_bbbbbbbbbbbb" as MessageId;
  expect(tags({ kind: "question", askId: ask2, seq: 2 })).toBe(
    ' question_of="m_bbbbbbbbbbbb" seq="2">',
  );
  expect(tags({ kind: "answer", askId: ask2, seq: 3 })).toBe(
    ' answer_of="m_bbbbbbbbbbbb" seq="3">',
  );
  expect(tags({ kind: "update", askId: ask2 })).toBe(
    ' update_of="m_bbbbbbbbbbbb">',
  );
  expect(tags(null)).toBe(">");
});

// R6a (Tyler 2026-10-24). Defect: the chat keeps the copy rendered at the
// hand-off, so an ask's bubble still reads due_in="30m" after its deadline
// passed, and repeats message_id as ask_id.
it("shows an ask in the chat with no countdown and no ask_id as its deadline passes", () => {
  const due = 30 * 60_000;
  const chatCopy = (activeMs: number) =>
    renderMonoMessage(
      ledgerRow({ kind: "ask", dueInMs: due - activeMs }),
      undefined,
    );
  const plain = renderMonoMessage(ledgerRow(null), undefined);
  expect(chatCopy(0)).toBe(plain);
  expect(chatCopy(due + 60 * 60_000)).toBe(plain);
  expect(
    renderMonoMessage(
      ledgerRow({ kind: "ack", askId: "m_bbbbbbbbbbbb" as MessageId }),
      undefined,
    ),
  ).toContain(' ack_of="m_bbbbbbbbbbbb">');
  const ask2 = "m_bbbbbbbbbbbb" as MessageId;
  for (const carries of [
    { kind: "question", askId: ask2, seq: 1 },
    { kind: "answer", askId: ask2, seq: 1 },
    { kind: "update", askId: ask2 },
  ] as const)
    expect(renderMonoMessage(ledgerRow(carries), undefined)).toBe(plain);
});

// Design 13. Defect: a lead's or worker's message reads as a deleted Mono's,
// with no session_id to send an answer to and no level; or a deleted Mono's
// message reads as a session's.
it("names a lead or worker sender by its title, session id and level", () => {
  const message = {
    id: "m_aaaaaaaaaaaa" as MessageId,
    fromMonoId: null,
    fromSessionId: "lead-1" as SessionId,
    body: "Done: tests pass",
    wantingReply: false,
    intent: "new" as const,
    replyTo: null,
    scope: null,
    carries: null,
  };
  expect(
    renderMonoMessage(message, {
      kind: "session",
      title: 'Fix "login"',
      level: "lead",
    }),
  ).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="Fix &#34;login&#34;" session_id="lead-1" level="lead" wanting_reply="false">\nDone: tests pass\n</mono_message>',
  );
  expect(
    renderMonoMessage(message, {
      kind: "session",
      title: "Tests",
      level: "worker",
    }),
  ).toContain(' from="Tests" session_id="lead-1" level="worker" ');
  expect(renderMonoMessage(message, undefined)).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="a deleted Mono" wanting_reply="false">\nDone: tests pass\n</mono_message>',
  );
});

// Design 13. Defect: a window shows a row that a lead or worker wrote as a
// deleted Mono's, in its queue item or in the text its hand-off sends.
it("names the session that wrote a row with no Mono when it is a lead or worker", async () => {
  const report = {
    fromMonoId: null,
    to: { kind: "session" as const, sessionId: "lead-1" },
    addressed: "lead-1",
    replyTo: null,
    scope: null,
    status: "delivered" as const,
    wantingReply: false,
    handledBySession: "lead-1",
    delivery: "queue",
    slot: null,
  };
  rust.state.rows = [
    ended({ ...report, fromSessionId: "worker-1", body: "Report: done" }),
    ended({
      ...report,
      id: "m_bbbbbbbbbbbb",
      requestId: "r1",
      fromSessionId: "old-chat",
      body: "From a Mono since deleted",
    }),
  ];
  const { host, get } = world([chat("lead-1")], [], {
    senders: { "worker-1": { title: "Fix login", level: "worker" } },
  });
  await reconcile(host, "lead-1");
  const [fromWorker, fromDeleted] = get("lead-1").queuedMessages ?? [];
  expect(fromWorker.text).toContain(
    ' from="Fix login" session_id="worker-1" level="worker" ',
  );
  expect(fromDeleted.text).toContain(' from="a deleted Mono" wanting_reply=');
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(
    host,
    "lead-1",
    fromWorker.id as MessageId,
    "dispatch",
    deliver,
  );
  expect(deliver.mock.calls[0][0].text).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="Fix login" session_id="worker-1" level="worker" wanting_reply="false">\nReport: done\n</mono_message>',
  );
});

// ADE-52. Defect: an approvals-board decision reads as a deleted Mono's
// message, or its item steers the Giga's running turn instead of waiting
// for the turn end.
it("queues an approvals-board decision in the Giga's chat as the user's, after the turn", async () => {
  rust.state.rows = [
    ended({
      requestId: "00000000-0000-4000-8000-000000000001",
      fromMonoId: null,
      fromSessionId: "approvals-board",
      to: { kind: "mono", monoId: "giga" },
      addressed: "giga",
      body: 'The user decided approval card APR-3 "Ship it".',
      status: "delivered",
      wantingReply: false,
      handledBySession: "giga-chat",
      delivery: "queue",
      slot: null,
    }),
  ];
  const { host, get } = world([chat("giga-chat", { busy: true })]);
  await reconcile(host, "giga-chat");
  expect(get("giga-chat").queuedMessages).toMatchObject([
    {
      monoMessageId: "m_aaaaaaaaaaaa",
      waitForIdle: true,
      text: '<mono_message message_id="m_aaaaaaaaaaaa" from="the user" via="approvals board" wanting_reply="false">\nThe user decided approval card APR-3 "Ship it".\n</mono_message>',
    },
  ]);
  expect(get("giga-chat").blocks.at(-1)?.boardDecision).toEqual({
    number: 3,
    heading: "Ship it",
    verdict: "approved",
    note: null,
    chosen: [],
  });
});

// PR7-6 review r1 nit 3, design 5.5 rule 4. Defect: a refused hand-off of a
// lead's or worker's row queues it again as a deleted Mono's.
it("queues a refused hand-off of a session sender's row again under its sender", async () => {
  rust.state.rows = [
    ended({
      fromMonoId: null,
      fromSessionId: "worker-1",
      to: { kind: "session", sessionId: "lead-1" },
      addressed: "lead-1",
      body: "Report: done",
      replyTo: null,
      scope: null,
      status: "delivered",
      wantingReply: false,
      handledBySession: "lead-1",
      delivery: "queue",
      slot: null,
    }),
  ];
  const { host, get } = world([chat("lead-1")], [], {
    senders: { "worker-1": { title: "Fix login", level: "worker" } },
  });
  const id = "m_aaaaaaaaaaaa" as MessageId;
  expect(await handOff(host, "lead-1", id, "dispatch", () => false)).toBe(
    false,
  );
  expect(get("lead-1").queuedMessages).toEqual([
    expect.objectContaining({
      id,
      text: expect.stringContaining(
        ' from="Fix login" session_id="worker-1" level="worker" ',
      ),
    }),
  ]);
});

// Design 16.7, E47. Defect: a reply reaches its sender with no line, with the
// wrong intent word, or with a handle of the session that answered, which
// names nothing in the sender's turn.
it("starts a reply with a line that names the question by its message id", () => {
  const question = "m_bbbbbbbbbbbb" as MessageId;
  expect(
    renderMonoMessage(
      {
        id: "m_cccccccccccc" as MessageId,
        fromMonoId: "mono-a" as MonoId,
        fromSessionId: "a-chat" as SessionId,
        body: "Green.",
        wantingReply: false,
        intent: "reply",
        replyTo: question,
        scope: null,
        carries: null,
      },
      { kind: "mono", name: "A", role: "mono" },
      undefined,
      {
        sessionId: "giga-chat" as SessionId,
        turnId: "turn-2" as TurnId,
        target: {
          messageId: question,
          body: "Is CI green on main?",
          handledBySession: "a-chat" as SessionId,
          slot: { turnId: "turn-1" as TurnId, handle: "#1" as Handle },
        },
      },
    ),
  ).toBe(
    '<mono_message message_id="m_cccccccccccc" from="A" mono_id="mono-a" role="mono" wanting_reply="false" reply_to="m_bbbbbbbbbbbb">\n[reply → m_bbbbbbbbbbbb · "Is CI green on main?"]\nGreen.\n</mono_message>',
  );
});

// PR I4 review r1 n1. Defect: a hand-off whose target is missing, or is not
// the row's reply_to, silently gives no line or names another message.
it("refuses to render a hand-off whose target is not the row's reply_to", () => {
  const steer = {
    id: "m_cccccccccccc" as MessageId,
    fromMonoId: "giga" as MonoId,
    fromSessionId: "giga-chat" as SessionId,
    body: "Keep the API stable",
    wantingReply: false,
    intent: "steer" as const,
    replyTo: "m_bbbbbbbbbbbb" as MessageId,
    scope: null,
    carries: null,
  };
  const reading = {
    sessionId: "a-chat" as SessionId,
    turnId: "turn-1" as TurnId,
  };
  expect(() =>
    renderMonoMessage(steer, undefined, undefined, {
      ...reading,
      target: null,
    }),
  ).toThrow("names no target, not its reply_to m_bbbbbbbbbbbb");
  expect(() =>
    renderMonoMessage(steer, undefined, undefined, {
      ...reading,
      target: {
        messageId: "m_dddddddddddd" as MessageId,
        body: "Other work",
        handledBySession: "a-chat" as SessionId,
        slot: null,
      },
    }),
  ).toThrow("names m_dddddddddddd, not its reply_to m_bbbbbbbbbbbb");
});

// Review PR 4a m2. Defect: a session whose worktree was removed counts as
// existing, so a message to it is accepted and never delivered.
it("never lists a session whose worktree was removed", async () => {
  const removed = chat("old-wt", { worktreeRemoved: true });
  const { send } = world([chat("project", { worktreeRemoved: true })], [], {
    stored: [removed],
  });
  await send({ ...ask(), send_to: "old-wt" });
  const live = rust.state.submitted[0].live as { sessionId: string }[];
  expect(live.map((entry) => entry.sessionId)).toEqual([]);
});

// R6 and R14. Exists only for gate/mutation (R6 one-off); app.sh r6 proves
// ask and ack end to end. Defect: an ask, ack, question, answer or update
// reaches Rust as another tool or with another call's input, a send stops
// being send_message, or an ask to a session no window has loaded leaves it
// out of `live`, so the gate cannot see it. An ack names no session: Rust
// lists the ask's parent itself (R6b review r1 M1). The agent app cannot
// dispatch question, answer or update until R14b-2, so they run on the host.
it("submits messages.ask, ack, question, answer and update as their own tools", async () => {
  const { host, send, run } = world([], [], { stored: [chat("lead-9")] });
  const stored = vi.spyOn(host, "stored");
  await send(ask());
  await run("messages.ask", { message: "Go", send_to: "lead-9" }, "r-ask");
  await run("messages.ask", { message: "Go", send_to: 42 }, "r-odd");
  await run(
    "messages.ack",
    { ask_id: "m_aaaaaaaaaaaa", message: "Done" },
    "r-ack",
  );
  const messages = agentMessages(host);
  await messages.question("r-q", { message: "Which branch?" });
  await messages.answer("r-an", {
    ask_id: "m_aaaaaaaaaaaa",
    seq: 1,
    message: "main",
  });
  await messages.update("r-up", { message: "Half done" });
  expect(
    rust.state.submitted.map(({ tool, requestId }) => [tool, requestId]),
  ).toEqual([
    ["send_message", "r1"],
    ["ask", "r-ask"],
    ["ask", "r-odd"],
    ["ack", "r-ack"],
    ["question", "r-q"],
    ["answer", "r-an"],
    ["update", "r-up"],
  ]);
  expect(rust.state.submitted.slice(4).map(({ input }) => input)).toEqual([
    { message: "Which branch?" },
    { ask_id: "m_aaaaaaaaaaaa", seq: 1, message: "main" },
    { message: "Half done" },
  ]);
  expect(
    rust.state.submitted
      .slice(0, 3)
      .map(({ live }) =>
        (live as { sessionId: string }[]).map((entry) => entry.sessionId),
      ),
  ).toEqual([[], ["lead-9"], []]);
  expect(stored.mock.calls).toEqual([["mono-a"], ["lead-9"]]);
});

// R4: Rust reads a session's level only from its stored row. Defect: the
// window's copy of a session's lead reaches the gate, where an agent could
// set it.
it("tells the gate no live session's lead", async () => {
  const { send } = world([
    chat("worker-1", { leadSessionId: "lead-1" }),
    chat("lead-1"),
  ]);
  await send({ ...ask(), send_to: "worker-1" });
  expect(rust.state.submitted[0].live).toEqual([
    {
      sessionId: "worker-1",
      steerable: true,
      ownProject: true,
    },
    {
      sessionId: "lead-1",
      steerable: true,
      ownProject: true,
    },
  ]);
});

// Design 13.4, PR7-5b: a session caller's call reaches Rust as that
// session, and the level refusal Rust answers reaches the agent whole.
// Defect: the refusal loses its code or hint on the way back, or the call
// goes as a Mono.
it("answers a session caller's send with the level refusal Rust gave it", async () => {
  const hint =
    'A worker can message only its lead. Message your lead (lead-1): messages.send with send_to "lead-1".';
  rust.state.failure = {
    code: "worker_scope",
    hint,
    message_id: "m_cccccccccccc",
  };
  const worker: Caller = {
    kind: "session",
    sessionId: "worker-1" as SessionId,
    turnId: "t" as TurnId,
    access: "operator",
    cwd: "/Users/me",
  };
  const { send } = world([chat("worker-1"), chat("a-chat")], [], {
    caller: worker,
  });
  const failed = await send(ask()).then(
    () => undefined,
    (error: unknown) => error,
  );
  expect(appErrorResponse(failed)).toEqual({
    ok: false,
    code: "worker_scope",
    hint,
    error: hint,
    message_id: "m_cccccccccccc",
  });
  expect(rust.state.submitted[0].caller).toEqual(worker);
});

// Review PR 4a r2 m10. Defect: an opener whose submit refuses or throws
// keeps its slot, so no later hand-off or reconcile can deliver the row.
it.each([
  ["refuses", () => false],
  ["throws", () => Promise.reject(new Error("authorize failed"))],
] as const)(
  "releases an opener whose submit %s and keeps its one item",
  async (_, submit) => {
    const { send, get, host } = world([chat("a-chat")]);
    await send(ask());
    const item = get("a-chat").queuedMessages![0];
    const started = await dispatchOpener(
      host,
      "a-chat",
      item.id as MessageId,
      submit,
    );
    expect(started).toBe(false);
    await vi.waitFor(() => expect(rust.state.rows[0].slot).toBeNull());
    expect(ids(get("a-chat"))).toEqual([item.id]);
  },
);

// Review PR 4a r2 m12. Defect: a call whose route another window ran
// reports the gate's route, never the submit that window started.
it("reports the delivery the owner window gives for the route it ran", async () => {
  rust.state.ownerWindow = "workspace-2";
  const ownerDeliveries = createOwnerDeliveries(1_000);
  const caller = world([], [], { ownerDeliveries });
  const sending = caller.send(ask());
  const owner = world([chat("a-chat")]);
  await vi.waitFor(() => expect(rust.state.rows).toHaveLength(1));
  const delivery = await routeMessages(
    owner.host,
    rust.state.rows.map(rust.targeted) as never,
    {
      sessionId: "a-chat" as SessionId,
      via: "queue",
    },
  );
  ownerDeliveries.settle({
    messageId: "m_aaaaaaaaaaaa" as MessageId,
    delivery,
  });
  expect(await sending).toMatchObject({ delivery: "submit" });
});

// Defect: the owner answers before the caller starts to wait, and the call
// reports the gate's route instead.
it("keeps an owner's answer that comes before the caller waits for it", async () => {
  const deliveries = createOwnerDeliveries(50);
  const id = "m_aaaaaaaaaaaa" as MessageId;
  deliveries.settle({ messageId: id, delivery: "submit" });
  expect(await deliveries.wait(id)).toBe("submit");
  expect(await deliveries.wait(id)).toBeUndefined();
});

// Review PR 4a r3 M1. Defect: a route that Rust sends to the owner window
// also runs in the caller window, which loads its own copy of the session,
// and the caller takes an answer from a window that is not the owner.
it("runs a route only in the owner window, and the caller takes only the owner's answer", async () => {
  rust.state.ownerWindow = "owner";
  const callerDeliveries = createOwnerDeliveries(200);
  const ownerDeliveries = createOwnerDeliveries(200);
  const caller = world([], [chat("a-chat")], {
    ownerDeliveries: callerDeliveries,
  });
  const owner = world([chat("a-chat")]);
  bus.current = "caller";
  listenForMonoRoutes(() => caller.host, callerDeliveries);
  bus.current = "owner";
  listenForMonoRoutes(() => owner.host, ownerDeliveries);
  const sending = caller.send(ask());
  await vi.waitFor(() => expect(rust.state.rows).toHaveLength(1));
  // mono_message_submit's emit_to(owner).
  await bus.emitTo("owner", "mono-message-route", {
    messages: rust.state.rows.map(rust.targeted),
    route: { sessionId: "a-chat", via: "queue" },
    callerWindow: "caller",
  });
  expect(await sending).toMatchObject({ delivery: "submit" });
  expect(ids(owner.get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
  expect(caller.open).not.toHaveBeenCalled();
  expect(caller.host.session("a-chat")).toBeUndefined();
  expect(
    await ownerDeliveries.wait("m_aaaaaaaaaaaa" as MessageId),
  ).toBeUndefined();
});

// Review PR 4a r3 m15. Defect: when the owner window never answers, the call
// reports some other delivery than the route the gate chose.
it("reports the gate's route when the owner window never answers", async () => {
  rust.state.ownerWindow = "owner";
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  const { send } = world([], [], {
    ownerDeliveries: createOwnerDeliveries(20),
  });
  expect(await send(ask(false, "steer"))).toMatchObject({ delivery: "steer" });
});

// Review PR 4a r3 m17. Defect: an answer that no one waits for, such as one
// that comes after its wait timed out, is kept for the life of the window.
it("drops an answer no one waits for after one timeout", async () => {
  vi.useFakeTimers();
  try {
    const deliveries = createOwnerDeliveries(50);
    const id = "m_aaaaaaaaaaaa" as MessageId;
    deliveries.settle({ messageId: id, delivery: "submit" });
    vi.advanceTimersByTime(51);
    const waited = deliveries.wait(id);
    vi.advanceTimersByTime(51);
    expect(await waited).toBeUndefined();
  } finally {
    vi.useRealTimers();
  }
});

// Review PR 4a r4 m18. Defect: an owner's answer that comes before the caller
// waits (an idle target the owner submits at once) expires at once, so the
// call reports the gate's route.
it("keeps an early answer until the caller waits, within one timeout", async () => {
  vi.useFakeTimers();
  try {
    const deliveries = createOwnerDeliveries(50);
    const id = "m_aaaaaaaaaaaa" as MessageId;
    deliveries.settle({ messageId: id, delivery: "submit" });
    vi.advanceTimersByTime(40);
    const waited = deliveries.wait(id);
    vi.advanceTimersByTime(51);
    expect(await waited).toBe("submit");
  } finally {
    vi.useRealTimers();
  }
});

// Review PR 4a r4 n11. Defect: the cleanup leaves the listeners in place, so
// a remounted window runs each route twice.
it("stops running routes once its listeners are cleaned up", async () => {
  const owner = world([], [chat("a-chat")]);
  bus.current = "owner";
  const stop = listenForMonoRoutes(() => owner.host, createOwnerDeliveries(50));
  stop();
  await Promise.resolve();
  await bus.emitTo("owner", "mono-message-route", {
    messages: [],
    route: { sessionId: "a-chat", via: "queue" },
    callerWindow: "caller",
  });
  expect(owner.open).not.toHaveBeenCalled();
});

// Design 5.2 and PR 4a review m9. Defect: the agent never sees the handle it
// answers with, or the queued text already has one that the turn which takes
// the message may not give it.
it("hands off a message that wants a reply with its turn's handle, and queues it without one", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  await send(ask(true));
  const [item] = get("a-chat").queuedMessages ?? [];
  expect(item.text).not.toContain("handle=");
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(host, "a-chat", item.id as MessageId, "dispatch", deliver);
  const [[handed]] = deliver.mock.calls;
  expect(handed.text).toContain(
    ' wanting_reply="true" handle="#1">\nReview PR 12\n',
  );
  expect(handed.turnId).toBe(rust.state.rows[0].slot?.turnId);
});

// Design 16.7, E47, PR I4 row. Defect: the steer reaches the agent with no
// line, quotes the whole target, names it by a handle of another turn or
// without the reply_to id a later turn needs, or the queued bubble text
// carries the line.
it("hands off a steer with the line that names its target, and queues it without one", async () => {
  const { send, get, host } = world([chat("a-chat", { busy: true })]);
  await send({
    ...ask(true),
    message: 'Fix the "login" bug\nin auth.ts — then run 👨‍👩‍👧 tests and report',
  });
  const target = rust.state.rows[0].id as MessageId;
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(host, "a-chat", target, "steer", deliver);
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send(
    {
      message: "Keep the API stable",
      send_to: "mono-a",
      wanting_reply: false,
      intent: "steer",
      reply_to: target,
    },
    "r2",
  );
  const steer = rust.state.rows[1].id as MessageId;
  const item = get("a-chat").queuedMessages?.find(
    (queued) => queued.id === steer,
  );
  expect(item?.text).toContain("Keep the API stable");
  expect(item?.text).not.toContain("[steer");
  await handOff(host, "a-chat", steer, "steer", deliver);
  expect(deliver.mock.calls[1][0].text).toBe(
    `<mono_message message_id="${steer}" from="Boss" mono_id="giga" role="giga" wanting_reply="false" reply_to="${target}">\n[steer → #1 · "Fix the \\"login\\" bug in auth.ts — then…"]\nKeep the API stable\n</mono_message>`,
  );
});

// Design 16.12.5. Defect: a whole-turn steer arrives with no line, or with a
// reply_to that names nothing to fetch, or its queued bubble shows the line.
it("hands off a whole-turn steer with its line and no reply_to", async () => {
  const { send, get, host } = world([chat("a-chat", { busy: true })]);
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send({
    message: "Stop and commit",
    send_to: "mono-a",
    wanting_reply: false,
    intent: "steer",
    scope: "turn",
  });
  const id = rust.state.rows[0].id as MessageId;
  expect(get("a-chat").queuedMessages?.[0].text).toBe(
    `<mono_message message_id="${id}" from="Boss" mono_id="giga" role="giga" wanting_reply="false">\nStop and commit\n</mono_message>`,
  );
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(host, "a-chat", id, "steer", deliver);
  expect(deliver.mock.calls[0][0].text).toBe(
    `<mono_message message_id="${id}" from="Boss" mono_id="giga" role="giga" wanting_reply="false">\n[steer → whole turn]\nStop and commit\n</mono_message>`,
  );
});

// PR I4 review n3. Handles restart at #1 in each turn, so the handle a
// target holds from an earlier turn of the reader's own session names
// another message in the turn the steer joins. Defect: the line names the
// target by that handle instead of its message id.
it("names a steer's target by its message id when its handle is from an earlier turn of the session", async () => {
  rust.state.rows = [ended({})];
  const [target] = rust.state.rows;
  const { send, host } = world([chat("a-chat", { busy: true })]);
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  await send({
    message: "Use the cached result",
    send_to: "mono-a",
    wanting_reply: false,
    intent: "steer",
    reply_to: target.id,
  });
  const steer = rust.state.rows[1].id as MessageId;
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(host, "a-chat", steer, "steer", deliver);
  expect(target.slot).toMatchObject({ turnId: "t-gone", handle: "#1" });
  expect(deliver.mock.calls[0][0].turnId).toBe("run-a-chat");
  expect(deliver.mock.calls[0][0].text).toBe(
    `<mono_message message_id="${steer}" from="Boss" mono_id="giga" role="giga" wanting_reply="false" reply_to="${target.id}">\n[steer → ${target.id} · "Is CI green?"]\nUse the cached result\n</mono_message>`,
  );
});

// PR 7 review r1 B2, and the PR I4 n3 re-check. A rule (b) late answer
// joins the Giga's running turn, while its target holds #1 in the Mono's
// chat; the turn ids here are equal, so only the session tells them apart.
// Defect: the line names the target by a handle the Giga's turn does not
// hold, because the hand-off reads the target's session as the reader's.
it("names a late answer's target by its message id in the Giga's turn", async () => {
  const target = ended({
    slot: { turnId: "run-giga-chat", handle: "#1", deliveredAt: 1 },
  });
  rust.state.rows = [
    target,
    ended({
      id: "m_bbbbbbbbbbbb",
      requestId: "late",
      fromMonoId: "mono-a",
      fromSessionId: "a-chat",
      to: { kind: "mono", monoId: "giga" },
      addressed: "giga",
      body: "It is green now",
      hop: 1,
      intent: "steer",
      replyTo: target.id,
      status: "delivered",
      wantingReply: false,
      handledBySession: "giga-chat",
      delivery: "steer",
      slot: null,
    }),
  ];
  const { host } = world([chat("giga-chat", { busy: true }), chat("a-chat")]);
  const deliver = vi.fn((_: HandedOff) => true);
  await handOff(
    host,
    "giga-chat",
    "m_bbbbbbbbbbbb" as MessageId,
    "steer",
    deliver,
  );
  expect(deliver.mock.calls[0][0].turnId).toBe("run-giga-chat");
  expect(deliver.mock.calls[0][0].text).toContain(
    `\n[steer → ${target.id} · "Is CI green?"]\nIt is green now\n`,
  );
});

// PR 4a review m7 (T02), design 5.5 rule 3. Defect: a hand-off takes the item
// out before assign_slot holds the row, so for that time neither an item nor
// a slot holds it.
it("keeps the item queued until assign_slot answers", async () => {
  const { send, get, host } = world([chat("a-chat")]);
  await send(ask());
  const [item] = get("a-chat").queuedMessages ?? [];
  let assign!: () => void;
  rust.state.assigning = new Promise<void>((resolve) => (assign = resolve));
  const deliver = vi.fn(() => {
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return true;
  });
  const handing = handOff(
    host,
    "a-chat",
    item.id as MessageId,
    "dispatch",
    deliver,
  );
  await vi.waitFor(() =>
    expect(rust.state.commands).toContain("mono_message_assign_slot"),
  );
  expect(ids(get("a-chat"))).toEqual([item.id]);
  expect(deliver).not.toHaveBeenCalled();
  assign();
  expect(await handing).toBe(true);
  expect(deliver).toHaveBeenCalledTimes(1);
  expect(ids(get("a-chat"))).toEqual([]);
});

// Review r5. Defect: a window that loads a session while another window's
// opener for it waits for authorize takes the row from that opener (the load
// frees its slot, or the stale item the load brought is handed off), so the
// session gets the message twice.
it("leaves a waiting opener's row to its window when a second window loads the session", async () => {
  const first = world([chat("a-chat")]);
  await first.send(ask(true));
  // What the database has: the first window saved the item before its hand-off.
  const saved = first.get("a-chat");
  const [item] = saved.queuedMessages ?? [];
  let providerTook!: (accepted: boolean) => void;
  const started = await dispatchOpener(
    first.host,
    "a-chat",
    item.id as MessageId,
    (_, took) => {
      providerTook = took;
      first.host.update("a-chat", (session) =>
        dequeueQueuedMessage(session, item.id),
      );
      // The turn now waits in queueSessionOperation for control_authorize_turn.
      return true;
    },
  );
  expect(started).toBe(true);
  const turn = rust.state.rows[0].slot?.turnId;
  const second = world([], [saved]);
  await second.open("a-chat");
  await second.host.reconciled("a-chat");
  expect(ids(second.get("a-chat"))).toEqual([item.id]);
  const deliver = vi.fn(() => true);
  expect(
    await handOff(
      second.host,
      "a-chat",
      item.id as MessageId,
      "dispatch",
      deliver,
    ),
  ).toBe(false);
  expect(deliver).not.toHaveBeenCalled();
  expect(ids(second.get("a-chat"))).toEqual([]);
  expect(rust.state.rows[0].slot).toMatchObject({
    turnId: turn,
    deliveredAt: null,
  });
  providerTook(true);
  await vi.waitFor(() => expect(rust.state.rows[0].slot?.deliveredAt).toBe(1));
});

// PR 4a review m4. Defect: the load of an idle chat queues the routed row
// before the route looks, so the call reports `queue` though the row opens the
// next turn.
it("reports submit for an idle Mono chat that no window had loaded", async () => {
  const { send, get } = world([], [chat("a-chat")]);
  expect(await send(ask())).toMatchObject({ delivery: "submit" });
  expect(ids(get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
});

// PR 4a review m6. Defect: the route queues its row before the reconcile that
// its load started queues the chat's older row, so the newer message opens
// the turn first.
it("queues a chat's older row ahead of the routed one when the route loads the chat", async () => {
  rust.state.rows = [ended({ status: "open", slot: null })];
  const { send, get } = world([], [chat("a-chat")]);
  expect(await send(ask())).toMatchObject({
    message_id: "m_bbbbbbbbbbbb",
    delivery: "queue",
  });
  expect(ids(get("a-chat"))).toEqual(["m_aaaaaaaaaaaa", "m_bbbbbbbbbbbb"]);
});

// PR 4a review m3. Defect: a retried call reports the stored row's delivery,
// still `queue` before the hand-off, though the first call reported `submit`;
// or it does not wait for a first call that still routes.
it("reports a duplicate's delivery as its first call in this window did, else as stored", async () => {
  let opened!: () => void;
  const routing = world([], [chat("a-chat")], {
    opening: new Promise<void>((resolve) => (opened = resolve)),
  });
  const first = routing.send(ask());
  await vi.waitFor(() => expect(routing.open).toHaveBeenCalled());
  const again = routing.send(ask());
  opened();
  const reported = await first;
  expect(reported).toMatchObject({ delivery: "submit" });
  expect(await again).toEqual({ ...(reported as object), duplicate: true });
  // A window that never routed it reports what the row stores.
  expect(await world([chat("a-chat")]).send(ask())).toMatchObject({
    delivery: "queue",
    duplicate: true,
  });
});

// Design 16.6 (PR I2 review r1 M2). Defect: a project steer with no turn in
// this window becomes a queue item that nothing steers, so the call says
// `steer` and an opener drops the steer later; or new work steers. The
// session loads here, so its load queues the steer row first.
it("drops a project-session steer at once when this window runs no turn of it", async () => {
  rust.state.route = { sessionId: "project", via: "steer" };
  const { send, get, open, steerProject } = world([], [chat("project")]);
  expect(
    await send({ ...ask(false, "steer"), send_to: "project" }, "r1"),
  ).toMatchObject({ status: "dropped", delivery: "dropped" });
  expect(open).toHaveBeenCalledWith("project");
  expect(rust.state.commands).toContain("mono_message_drop");
  expect(rust.state.commands).not.toContain("mono_message_assign_slot");
  expect(ids(get("project"))).toEqual([]);
  rust.state.route = { sessionId: "project", via: "queue" };
  await send({ ...ask(), send_to: "project" }, "r2");
  expect(steerProject).not.toHaveBeenCalled();
  expect(get("project").queuedMessages).toEqual([
    expect.objectContaining({ id: "m_bbbbbbbbbbbb", waitForIdle: true }),
  ]);
});

// Design 16.12.3.3 (whole-turn review r1 M1). Defect: a steer that Rust
// dropped after the commit, because its turn ended while the gate ran, is
// answered as queued, or gets an item; or the boundary loses its scope.
it("answers dropped for a whole-turn steer whose turn ended while the gate ran", async () => {
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  rust.state.turnEnded = true;
  const { send, get } = world([
    chat("a-chat", { busy: true, turnReadiness: "ready" }),
  ]);
  expect(
    await send({
      message: "Stop and commit",
      send_to: "mono-a",
      wanting_reply: false,
      intent: "steer",
      scope: "turn",
    }),
  ).toMatchObject({ status: "dropped", delivery: "dropped" });
  expect(rust.state.submitted[0]).toMatchObject({
    input: { intent: "steer", scope: "turn" },
  });
  expect(ids(get("a-chat"))).toEqual([]);
  expect(rust.state.commands).not.toContain("mono_message_assign_slot");
});

// Design 16.6. Defect: reconcile puts a waiting steer row back as an item
// that waits for the turn end, so the drain opens a turn with it.
it("reconciles a steer row as an item that steers", async () => {
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  const { send, get, host } = world([chat("a-chat", { busy: true })]);
  await send(ask(false, "steer"));
  host.update("a-chat", (session) =>
    dequeueQueuedMessage(session, "m_aaaaaaaaaaaa"),
  );
  await reconcile(host, "a-chat");
  const [item] = get("a-chat").queuedMessages ?? [];
  expect(item.id).toBe("m_aaaaaaaaaaaa");
  expect(item.waitForIdle).toBeUndefined();
});

// PR 4a review m7 (T21, T22). Defect: a rejected call reaches the CLI without
// the row it wrote or the open handles it lists.
it("answers a rejected reply with its row and its open handles", async () => {
  const hint =
    "Several handles are open: #1 (from Boss: Is CI green?), #2 (from Boss: Ship it?). Pass to.";
  const open_handles = [
    { handle: "#1", message_id: "m_aaaaaaaaaaaa", from: "Boss" },
    { handle: "#2", message_id: "m_bbbbbbbbbbbb", from: "Boss" },
  ];
  rust.state.failure = {
    code: "ambiguous_reply",
    hint,
    message_id: "m_cccccccccccc",
    open_handles,
  };
  const { run } = world([], [], { caller: "mono-a" });
  const failed = await run("messages.reply", { message: "Done" }).then(
    () => undefined,
    (error: unknown) => error,
  );
  expect(appErrorResponse(failed)).toEqual({
    ok: false,
    code: "ambiguous_reply",
    hint,
    error: hint,
    message_id: "m_cccccccccccc",
    open_handles,
  });
});

// PR 4a review m7 (T26) and design 2 step 6. Defect: a reply's `to` is refused
// as an unknown field or dropped before Rust reads it, so Rust answers
// ambiguous_reply; or the call reaches Rust without the turn it answers in.
it("passes a reply's handles and its turn to Rust, one handle as a list", async () => {
  rust.state.route = { sessionId: "giga-chat", via: "queue" };
  const { run } = world([chat("giga-chat", { busy: true })], [], {
    caller: "mono-a",
  });
  await run("messages.reply", { message: "Done", to: "#2" }, "re-1");
  await run("messages.reply", { message: "Both", to: ["#1", "#2"] }, "re-2");
  expect(rust.state.submitted.map((call) => call.input)).toEqual([
    { message: "Done", wanting_reply: false, to: ["#2"] },
    { message: "Both", wanting_reply: false, to: ["#1", "#2"] },
  ]);
  expect(rust.state.submitted[0].caller).toEqual({
    kind: "mono_chat",
    monoId: "mono-a",
    sessionId: "a-chat",
    turnId: "t",
  });
});

// Design 5.3. Defect: the pump queues a second notice when it runs again, also
// after the notice was sent and a crash lost its mark, or it never marks the
// row, so every later run tries again. Design U10. Defect: a waiting notice
// shows as a bubble in the transcript instead of its queue row.
it("queues a sender notice once across runs and marks its row notified", async () => {
  rust.state.rows = [ended({})];
  rust.state.failedMarks = 1;
  const { host, get } = world([chat("giga-chat", { busy: true })]);
  // The first claim fails, so that run queues nothing.
  await expect(reconcile(host, "giga-chat")).rejects.toThrow(
    "database is locked",
  );
  await reconcile(host, "giga-chat");
  const notices = get("giga-chat").queuedMessages ?? [];
  expect(notices).toEqual([
    expect.objectContaining({
      appRequestId: "notice-m_aaaaaaaaaaaa-unanswered",
      waitForIdle: true,
    }),
  ]);
  expect(notices[0].text).toBe(
    '<mono_notice message_id="m_aaaaaaaaaaaa" status="unanswered" to="A">\nIs CI green?\n</mono_notice>',
  );
  expect(get("giga-chat").blocks).toEqual([]);
  expect(rust.state.rows[0].notifiedAt).not.toBeNull();
  // A lost mark while the notice still waits in the queue.
  rust.state.rows[0].notifiedAt = null;
  await reconcile(host, "giga-chat");
  expect(get("giga-chat").queuedMessages).toHaveLength(1);
  // The notice opens the next turn, and a crash loses the mark again.
  host.update("giga-chat", (session) =>
    acknowledgeMonoMessage(session, notices[0], {
      mode: "new-turn",
      cards: queuedAppDelivery(notices[0]),
    }),
  );
  expect(get("giga-chat").blocks).toEqual([
    expect.objectContaining({
      id: "notice-m_aaaaaaaaaaaa-unanswered",
      appRequestId: "notice-m_aaaaaaaaaaaa-unanswered",
    }),
  ]);
  rust.state.rows[0].notifiedAt = null;
  await reconcile(host, "giga-chat");
  expect(get("giga-chat").queuedMessages).toBeUndefined();
  expect(rust.state.rows[0].notifiedAt).not.toBeNull();
});

// Review r2 of design 5.3 and PR 4a review m19. Defect: every window that has
// the sender's chat loaded runs the pump, so each queues the notice into its
// own copy of the chat.
it("gives a pending notice one notice item across two windows that have the chat", async () => {
  rust.state.rows = [ended({})];
  const other = world([chat("giga-chat")], [], { label: "second" });
  const owner = world([chat("giga-chat")], [], { label: "main" });
  bus.current = "main";
  listenForMonoNotices(owner.host);
  bus.current = "second";
  listenForMonoNotices(other.host);
  bus.emit("mono-message-notices", { sessions: ["giga-chat"] });
  await vi.waitFor(() => expect(rust.state.rows[0].notifiedAt).not.toBeNull());
  expect(ids(owner.get("giga-chat"))).toEqual([
    "notice-m_aaaaaaaaaaaa-unanswered",
  ]);
  expect(other.get("giga-chat").queuedMessages).toBeUndefined();
});

// PR 4a review m19. Defect: every window that has the session loaded
// reconciles it, so each queues the same unqueued row into its own copy, and
// a copy that is not busy starts a second turn of the session.
it("queues an unqueued row only in the window that owns the session", async () => {
  rust.state.rows = [ended({ status: "open", slot: null, delivery: "queue" })];
  const second = world([chat("a-chat")], [], { label: "second" });
  const main = world([chat("a-chat")], [], { label: "main" });
  await reconcile(second.host, "a-chat");
  await reconcile(main.host, "a-chat");
  expect(ids(main.get("a-chat"))).toEqual(["m_aaaaaaaaaaaa"]);
  expect(second.get("a-chat").queuedMessages).toBeUndefined();
});

// PR 5 review r2 m1. Defect: the owner of the chat changes while a window's
// notice pump runs (a window whose label sorts first loads the chat), and
// both windows queue the same notice into their own copy of the chat.
it("queues a notice in one window when the chat's owner changes during the pump", async () => {
  rust.state.rows = [ended({})];
  const second = world([chat("giga-chat")], [], { label: "second" });
  const first = reconcile(second.host, "giga-chat");
  const main = world([chat("giga-chat")], [], { label: "main" });
  const later = reconcile(main.host, "giga-chat");
  await Promise.all([first, later]);
  expect([
    ...ids(second.get("giga-chat")),
    ...ids(main.get("giga-chat")),
  ]).toEqual(["notice-m_aaaaaaaaaaaa-unanswered"]);
});

// Review r4 of design 5.3. Defect: the notice goes to the session that sent
// the message, a habit run here, which no one reads as the Mono's chat, or it
// is queued once for each event that names the chat.
it("queues the notice for a dropped habit-run message into the Mono's chat once", async () => {
  rust.state.rows = [
    ended({
      fromSessionId: "giga-habit-run",
      status: "dropped",
      wantingReply: false,
      slot: null,
    }),
  ];
  const { host, get } = world([chat("giga-chat"), chat("giga-habit-run")]);
  listenForMonoNotices(host);
  bus.emit("mono-message-notices", { sessions: ["giga-chat"] });
  bus.emit("mono-message-notices", { sessions: ["giga-chat"] });
  await vi.waitFor(() => expect(rust.state.rows[0].notifiedAt).not.toBeNull());
  expect(ids(get("giga-chat"))).toEqual(["notice-m_aaaaaaaaaaaa-dropped"]);
  expect(get("giga-habit-run").queuedMessages).toBeUndefined();
});

// Design 5.3. Defect: a pump whose chat left the window marks the row
// notified though no notice was queued, so the sender is never told.
it("keeps a notice pending when its chat is no longer in the window", async () => {
  rust.state.rows = [ended({})];
  const { host } = world([]);
  await reconcile(host, "giga-chat");
  expect(rust.state.rows[0].notifiedAt).toBeNull();
});

// Design 7, 5.3 (review r3), E12. Defect: resetting a Mono's chat only
// revokes its token, so the messages queued for it stay open with no chat to
// take them and the Giga, held in its own turn, never hears they were
// dropped; or it hears more than once.
it("resetting A's chat gives the held Giga one notice per dropped message", async () => {
  const { host, get, send } = world([
    chat("giga-chat", { busy: true }),
    chat("a-chat", { busy: true }),
  ]);
  listenForMonoNotices(host);
  await send(ask(true), "r1");
  await send(ask(false), "r2");
  expect(ids(get("a-chat"))).toEqual(["m_aaaaaaaaaaaa", "m_bbbbbbbbbbbb"]);
  const reset = (current: Session) =>
    resetMonoSession(current, {
      stop: async () => current,
      remove: () => deleteSession(current.id),
      replace: () => {},
    });
  await reset(get("a-chat"));
  await vi.waitFor(() =>
    expect(ids(get("giga-chat"))).toEqual([
      "notice-m_aaaaaaaaaaaa-dropped",
      "notice-m_bbbbbbbbbbbb-dropped",
    ]),
  );
  expect(rust.state.rows.map((entry) => entry.status)).toEqual([
    "dropped",
    "dropped",
  ]);
  expect(get("giga-chat").busy).toBe(true);
  // The old chat's deletion clears it again.
  await reset(get("a-chat"));
  await Promise.resolve();
  expect(ids(get("giga-chat"))).toHaveLength(2);
});

// Design 5.5 rule 8 (review r5). Defect: Remove takes away the item of a row
// that a hand-off holds, so the text reaches the agent while its sender is
// told it was dropped; or Remove never drops a waiting row, so reconcile
// queues it again.
it("Remove keeps the item of a row a hand-off holds and drops a waiting one", async () => {
  const { host, get, send } = world([chat("a-chat", { busy: true })]);
  await send(ask(true), "r1");
  await send(ask(false), "r2");
  const [held, waiting] = get("a-chat").queuedMessages!;
  let deliver: (accepted: boolean) => void = () => {};
  const handing = handOff(
    host,
    "a-chat",
    held.id as MessageId,
    "steer",
    () => new Promise<boolean>((resolve) => (deliver = resolve)),
  );
  await vi.waitFor(() => expect(rust.state.rows[0].slot).not.toBeNull());
  expect(await removeQueued(host, "a-chat", held.id)).toBe(false);
  expect(await removeQueued(host, "a-chat", waiting.id)).toBe(true);
  expect(ids(get("a-chat"))).toEqual([held.id]);
  expect(rust.state.rows.map((entry) => entry.status)).toEqual([
    "open",
    "dropped",
  ]);
  deliver(true);
  await handing;
});
