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
import { TURN_FINISHED_ERROR } from "../../../app/model/appSessionSend";
import {
  createTurnIds,
  dispatchOpener,
  ensureQueued,
  getMonos,
  handOff,
  reconcile,
  replyMessage,
  routeMessages,
  sendMessage,
  type MonoMessagingHost,
} from "./monoRoute";
import {
  renderMonoMessage,
  type MessageId,
  type MonoId,
  type SessionId,
  type TurnId,
} from "./monoMessages";

/**
 * `mono_messages` as the Rust commands keep it: `submit` accepts every call
 * and routes it to `route`; the gate itself is tested in `mono_gate.rs`. The
 * slot rules are those of `assign_slot`, `release` and `mark_delivery`.
 */
const rust = vi.hoisted(() => {
  type Row = {
    id: string;
    requestId: string;
    fromMonoId: string;
    fromSessionId: string;
    body: string;
    hop: number;
    status: "open" | "delivered";
    wantingReply: boolean;
    handledBySession: string;
    delivery: string;
    slot: { turnId: string; deliveredAt: number | null } | null;
  };
  const state = {
    rows: [] as Row[],
    route: { sessionId: "a-chat", via: "queue" },
    /** The window Rust names as the owner of the route's session, if not this one. */
    ownerWindow: undefined as string | undefined,
    commands: [] as string[],
    submitted: [] as Record<string, unknown>[],
  };
  const row = (id: string) => state.rows.find((entry) => entry.id === id);
  const invoke = async (command: string, args: Record<string, never>) => {
    state.commands.push(command);
    if (command === "mono_message_submit") state.submitted.push(args);
    const held = (entry: Row | undefined, turnId: string) =>
      entry?.slot?.turnId === turnId && entry.slot.deliveredAt === null;
    switch (command) {
      case "mono_message_submit": {
        const { caller, requestId, input } = args as {
          caller: { monoId: string; sessionId: string };
          requestId: string;
          input: { message: string; wanting_reply: boolean };
        };
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
          body: input.message,
          hop: 0,
          status: input.wanting_reply ? "open" : "delivered",
          wantingReply: input.wanting_reply,
          handledBySession: state.route.sessionId,
          delivery: "queue",
          slot: null,
        };
        state.rows.push(accepted);
        return {
          outcome: "accepted",
          messages: [accepted],
          answered: answering
            ? [{ handle: "#1", message_id: answering.id }]
            : [],
          duplicate: false,
          route: state.route,
          ...(state.ownerWindow ? { ownerWindow: state.ownerWindow } : {}),
        };
      }
      case "mono_message_assign_slot": {
        const entry = row(args.id);
        if (!entry) return false;
        if (entry.slot) return held(entry, args.turnId);
        entry.slot = { turnId: args.turnId, deliveredAt: null };
        entry.delivery = args.kind;
        return true;
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
      case "mono_messages_unqueued":
        return state.rows.filter(
          (entry) => entry.handledBySession === args.sessionId && !entry.slot,
        );
    }
    throw new Error(`unexpected command ${command}`);
  };
  return { state, invoke };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: rust.invoke }));

beforeEach(() => {
  rust.state.rows = [];
  rust.state.route = { sessionId: "a-chat", via: "queue" };
  rust.state.ownerWindow = undefined;
  rust.state.commands = [];
  rust.state.submitted = [];
});

const chat = (id: string, change: Partial<Session> = {}): Session => ({
  ...newSession("claude", "/Users/me", "claude:opus"),
  id,
  ...change,
});

/** A window with `loaded` sessions; `open` loads one of `unloaded` and reconciles it. */
function world(
  loaded: Session[],
  unloaded: Session[] = [],
  { caller = "giga", stored = [] as Session[] } = {},
) {
  let sessions = loaded;
  const steerProject = vi.fn(async () => {});
  const open = vi.fn(async (id: string) => {
    const found = unloaded.find((session) => session.id === id) ?? null;
    if (found) {
      sessions = [...sessions, found];
      await reconcile(host, id);
    }
    return found;
  });
  const host: MonoMessagingHost = {
    session: (id) => sessions.find((session) => session.id === id),
    update: (id, change) => {
      sessions = sessions.map((session) =>
        session.id === id ? change(session) : session,
      );
    },
    isMonoChat: (id) => id.endsWith("-chat"),
    sender: () => ({ name: "Boss", role: "giga" }),
    turnId: createTurnIds(() => 0),
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
    ownProject: () => true,
    monos: () => [
      { id: "giga" as MonoId, name: "Boss", role: "giga" },
      { id: "mono-a" as MonoId, name: "A", role: "mono" },
    ],
    steerProject,
  };
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
  await handOff(
    host,
    "a-chat",
    { id: item.id as MessageId, text: item.text },
    "dispatch",
    () => {
      host.update("a-chat", (session) =>
        dequeueQueuedMessage(session, item.id),
      );
      return true;
    },
  );
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
  const message = { id: item.id as MessageId, text: item.text };
  expect(await handOff(host, "a-chat", message, "dispatch", deliver)).toBe(
    true,
  );
  // A stale copy of the item, as a reload or a second reconcile leaves it.
  host.update("a-chat", (session) =>
    ensureQueued(session, message, "queue", true),
  );
  expect(await handOff(host, "a-chat", message, "dispatch", deliver)).toBe(
    false,
  );
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
  const message = { id: item.id as MessageId, text: item.text };
  const accepted = await handOff(host, "a-chat", message, "dispatch", () => {
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
  const message = { id: item.id as MessageId, text: item.text };
  // A steer into turn 0 is in flight when the drain opens turn 1 with it.
  let finishSteer!: (accepted: boolean) => void;
  const steering = handOff(
    host,
    "a-chat",
    message,
    "steer",
    () => new Promise<boolean>((resolve) => (finishSteer = resolve)),
  );
  await vi.waitFor(() => expect(finishSteer).toBeDefined());
  const deliver = vi.fn(() => true);
  expect(await handOff(host, "a-chat", message, "dispatch", deliver)).toBe(
    false,
  );
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
  const message = { id: item.id as MessageId, text: item.text };
  let providerTook!: (accepted: boolean) => void;
  const started = await dispatchOpener(host, "a-chat", message, (took) => {
    providerTook = took;
    // submitSession's commit takes the item out and starts the turn.
    host.update("a-chat", (session) => dequeueQueuedMessage(session, item.id));
    return true;
  });
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
  await dispatchOpener(
    host,
    "a-chat",
    { id: item.id as MessageId, text: item.text },
    (took) => {
      providerTook = took;
      host.update("a-chat", (session) =>
        dequeueQueuedMessage(session, item.id),
      );
      return true;
    },
  );
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
  await handOff(
    host,
    "giga-chat",
    { id: item.id as MessageId, text: item.text },
    "steer",
    () => true,
  );
  expect(
    await run("messages.reply", { message: "CI is green" }, "re-1"),
  ).toEqual({
    ...first,
    duplicate: true,
  });
  expect(get("giga-chat").queuedMessages).toBeUndefined();
});

it("renders the tag the receiving agent reads", () => {
  expect(
    renderMonoMessage(
      {
        id: "m_aaaaaaaaaaaa" as MessageId,
        fromMonoId: "giga" as MonoId,
        body: "Is CI green?",
        wantingReply: true,
      },
      { name: 'Boss "B"', role: "giga" },
    ),
  ).toBe(
    '<mono_message message_id="m_aaaaaaaaaaaa" from="Boss &#34;B&#34;" mono_id="giga" role="giga" wanting_reply="true">\nIs CI green?\n</mono_message>',
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
