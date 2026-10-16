// @vitest-environment happy-dom
import { beforeEach, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import {
  dequeueQueuedMessage,
  steerableQueuedMessage,
} from "../../sessions/model/messageQueue";
import { sanitizeSessionForPersist } from "../../sessions/data/sessionStore";
import {
  handleAgentApp,
  type AgentAppHost,
} from "../../agent-app/model/agentApp";
import { appErrorResponse } from "../../agent-app/model/appActionError";
import {
  queuedAppDelivery,
  TURN_FINISHED_ERROR,
} from "../../../app/model/appSessionSend";
import { acknowledgeMonoMessage } from "./monoMessaging";
import {
  createLoadReconciles,
  createOwnerDeliveries,
  createReportedDeliveries,
  dispatchOpener,
  listenForMonoNotices,
  opensTurn,
  listenForMonoRoutes,
  ensureQueued,
  getMonos,
  handOff,
  reconcile,
  removeQueued,
  replyMessage,
  routeMessages,
  sendMessage,
  type HandedOff,
  type MonoMessagingHost,
} from "./monoRoute";
import {
  renderMonoMessage,
  renderMonoNotice,
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
    to: { kind: "mono"; monoId: string } | null;
    addressed: string;
    body: string;
    hop: number;
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
    /** `assign_slot` runs once this settles. */
    assigning: undefined as Promise<void> | undefined,
    /** How many `mark_notified` calls fail before one succeeds. */
    failedMarks: 0,
    /** Each Mono's current chat (`monos.data_json.sessionId`). */
    chats: {} as Record<string, string>,
    /** The sessions each window has loaded (`control_loaded_sessions`). */
    loaded: {} as Record<string, string[]>,
    commands: [] as string[],
    submitted: [] as Record<string, unknown>[],
  };
  const row = (id: string) => state.rows.find((entry) => entry.id === id);
  const held = (entry: Row, turnId: string) =>
    entry.slot?.turnId === turnId && entry.slot.deliveredAt === null;
  const invoke = async (command: string, args: Record<string, never>) => {
    state.commands.push(command);
    switch (command) {
      case "mono_message_submit": {
        state.submitted.push(args);
        const { caller, requestId, input } = args as {
          caller: { monoId: string; sessionId: string };
          requestId: string;
          input: { message: string; wanting_reply: boolean; send_to?: string };
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
            messages: prior,
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
          fromMonoId: caller.monoId,
          fromSessionId: caller.sessionId,
          to: input.send_to ? { kind: "mono", monoId: input.send_to } : null,
          addressed: input.send_to ?? "",
          body: input.message,
          hop: 0,
          status: input.wanting_reply ? "open" : "delivered",
          wantingReply: input.wanting_reply,
          handledBySession: state.route.sessionId,
          delivery: "queue",
          slot: null,
          notifiedAt: null,
        };
        state.rows.push(accepted);
        return {
          outcome: "accepted",
          messages: [accepted],
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
        if (entry.slot) return held(entry, args.turnId) ? entry : null;
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
        return entry;
      }
      case "mono_message_release": {
        const entry = row(args.id);
        if (entry && held(entry, args.turnId)) entry.slot = null;
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
        return state.rows.filter(
          (entry) =>
            entry.handledBySession === args.sessionId &&
            !entry.slot &&
            (entry.status === "open" || entry.status === "delivered"),
        );
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
      // `clear_session`: the session's questions close without a notice, and
      // the rows it still had to take are dropped and leave it.
      case "mono_messages_clear_session": {
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
    }
    throw new Error(`unexpected command ${command}`);
  };
  return { state, invoke };
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
  rust.state.assigning = undefined;
  rust.state.failedMarks = 0;
  rust.state.chats = { giga: "giga-chat", "mono-a": "a-chat" };
  rust.state.loaded = {};
  rust.state.commands = [];
  rust.state.submitted = [];
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
    caller = "giga",
    stored = [] as Session[],
    ownerDeliveries = createOwnerDeliveries(50),
    label = "main",
    opening = Promise.resolve(),
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
      caller: {
        kind: "mono_chat",
        monoId: (caller === "giga" ? "giga" : "mono-a") as MonoId,
        sessionId: (caller === "giga" ? "giga-chat" : "a-chat") as SessionId,
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
  const app = {
    messages: {
      getMonos: () => getMonos(host),
      send: (requestId: string, input: never) =>
        sendMessage(host, requestId, input),
      reply: (requestId: string, input: never) =>
        replyMessage(host, requestId, input),
    },
  } as unknown as AgentAppHost;
  const source = chat(caller === "giga" ? "giga-chat" : "a-chat");
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

const ask = (wanting_reply = false, action_type = "queue") => ({
  message: "Review PR 12",
  send_to: "mono-a",
  wanting_reply,
  action_type,
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
  expect(steerableQueuedMessage(get("a-chat"))).toBeUndefined();
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
  expect(steerableQueuedMessage(get("a-chat"))?.id).toBe("m_aaaaaaaaaaaa");
});

// Defect: the second steer reaches the running turn first (head placement).
it("steers two messages into a starting turn in the order they came, once it is ready", async () => {
  rust.state.route = { sessionId: "a-chat", via: "steer" };
  const { send, get, host } = world([
    chat("a-chat", { busy: true, turnReadiness: "starting" }),
  ]);
  await send({ ...ask(), message: "First" }, "r1");
  await send({ ...ask(), message: "Second" }, "r2");
  expect(steerableQueuedMessage(get("a-chat"))).toBeUndefined();
  host.update("a-chat", (session) => ({ ...session, turnReadiness: "ready" }));
  const first = steerableQueuedMessage(get("a-chat"));
  expect(first?.text).toContain("First");
  host.update("a-chat", (session) => dequeueQueuedMessage(session, first!.id));
  expect(steerableQueuedMessage(get("a-chat"))?.text).toContain("Second");
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

// Defect: a steer that finds the turn over loses the message, or keeps its
// slot so nothing ever queues it again.
it("queues a project-session steer again when the turn just finished", async () => {
  rust.state.route = { sessionId: "project", via: "steer" };
  const { send, get, steerProject } = world([chat("project", { busy: true })]);
  steerProject.mockRejectedValueOnce(new Error(TURN_FINISHED_ERROR));
  expect(await send(ask(false, "steer"))).toMatchObject({ delivery: "queue" });
  expect(rust.state.rows[0]).toMatchObject({
    status: "delivered",
    slot: null,
  });
  expect(get("project").queuedMessages).toEqual([
    expect.objectContaining({ id: "m_aaaaaaaaaaaa", waitForIdle: true }),
  ]);
  expect(get("project").queuedMessages?.[0].blockId).toBeUndefined();
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
  await routeMessages(owner.host, rust.state.rows as never, {
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
    body: "Is CI green?",
    wantingReply: true,
  };
  const sender = { name: 'Boss "B"', role: "giga" as const };
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
  const delivery = await routeMessages(owner.host, rust.state.rows as never, {
    sessionId: "a-chat" as SessionId,
    via: "queue",
  });
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
    messages: rust.state.rows,
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

// Defect: a project steer that finds no turn of the session in this window is
// neither steered nor queued, so it waits for the next load.
it("queues a project-session steer when this window runs no turn of it", async () => {
  rust.state.route = { sessionId: "project", via: "steer" };
  const { send, get, steerProject } = world([chat("project")]);
  expect(await send(ask(false, "steer"))).toMatchObject({ delivery: "submit" });
  expect(steerProject).not.toHaveBeenCalled();
  expect(get("project").queuedMessages).toEqual([
    expect.objectContaining({ id: "m_aaaaaaaaaaaa", waitForIdle: true }),
  ]);
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
// row, so every later run tries again.
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
      remove: async () => {},
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
