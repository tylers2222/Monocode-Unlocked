import { expect, it, vi } from "vitest";
import type {
  Handle,
  MessageId,
  SessionId,
  TurnId,
} from "../../monos/model/monoMessages";
import {
  appendSteerUser,
  appendUser,
  applyHarnessEvent,
  stopStreaming,
} from "../../../integrations/harness/core/apply";
import {
  registerHarness,
  type HarnessAdapter,
} from "../../../integrations/harness/core/registry";
import {
  acknowledgeMonoMessage,
  enqueueMonoMessage,
} from "../../monos/model/monoMessaging";
import {
  busyRoute,
  composerTarget,
  excerpt,
  intentLine,
  linkedText,
  parseMessageLink,
  parseTargetLink,
  planUserSend,
  runningItems,
  targetRef,
  type ReplyTarget,
  type RunningItem,
  type SendOrigin,
  type SendRoute,
} from "./messageIntent";
import {
  canDispatchQueuedHead,
  queuedHead,
  queuedMessageForSubmit,
  runningTurnMap,
  steerableQueuedMessage,
} from "./messageQueue";
import {
  newSession,
  type QueuedMessage,
  type Session,
  type TargetLink,
} from "./session";

// Design 16.7 step 2 and 5. Defect: a ninth word reaches the line, or a text
// that was not cut gets a false cut mark.
it("keeps the first 8 words and marks a cut only when one was made", () => {
  expect(excerpt("one two three four five six seven eight nine")).toBe(
    "one two three four five six seven eight…",
  );
  expect(excerpt("one two three four five six seven eight")).toBe(
    "one two three four five six seven eight",
  );
});

// Design 16.7 step 1. Defect: a line break inside the line, which ends it
// early for the agent that reads it.
it("puts newlines, tabs, U+2028 and other control characters on one line", () => {
  expect(excerpt("  Fix\nthe\r\n\tlogin\u2028bug\u00a0now\u0007please  ")).toBe(
    "Fix the login bug now please",
  );
});

// Design 16.7 step 3. Defect: a code unit cut splits a ZWJ sequence, so the
// line ends in half an emoji.
it("cuts at 60 grapheme clusters and keeps an emoji at the cut whole", () => {
  const family = "👨‍👩‍👧";
  expect(excerpt(`${"a".repeat(59)}${family}bbb`)).toBe(
    `${"a".repeat(59)}${family}…`,
  );
});

// Design 16.7 step 3. Defect: a text with no spaces is never cut.
it("cuts a text with no spaces at 60 clusters", () => {
  expect(excerpt("漢".repeat(200))).toBe(`${"漢".repeat(60)}…`);
});

// Design 16.7 step 3 (intents review r1). Defect: 60 clusters of many code
// points each make a quote longer than parseMessageLink keeps, so the link
// is dropped on the next load.
it("keeps a quote within 200 code points when its clusters are long", () => {
  const letter = `e${"\u0301".repeat(20)}`;
  const quote = excerpt(letter.repeat(60));
  expect([...quote].length).toBeLessThanOrEqual(200);
  expect(quote).toBe(`${letter.repeat(9)}…`);
  // 50 clusters of 4 code points fill 200 exactly, so the `…` must fit too.
  expect([...excerpt(`a\u0301\u0302\u0303`.repeat(60))].length).toBe(197);
});

// Design 16.7 step 4 (intents review r1). Defect: the stored quote shows
// backslashes to the user.
it("returns the excerpt as typed, quotes and backslashes included", () => {
  expect(excerpt('Fix the "login" bug in C:\\auth')).toBe(
    'Fix the "login" bug in C:\\auth',
  );
});

// Design 16.7 format table and 16.12.5. Defect: any change to the line the
// rules text teaches the agents to read.
it("writes the exact line for each intent, with and without a ref", () => {
  const quote = "first words…";
  expect(intentLine({ intent: "steer", quote }, "#3")).toBe(
    '[steer → #3 · "first words…"]',
  );
  expect(intentLine({ intent: "steer", quote })).toBe(
    '[steer → "first words…"]',
  );
  expect(intentLine({ intent: "reply", quote }, "m_abcdefgh2345")).toBe(
    '[reply → m_abcdefgh2345 · "first words…"]',
  );
  expect(intentLine({ intent: "reply", quote })).toBe(
    '[reply → "first words…"]',
  );
  expect(intentLine({ intent: "continue", quote }, "#3")).toBe(
    '[re #3 · "first words…"]',
  );
  expect(intentLine({ intent: "continue", quote })).toBe('[re "first words…"]');
  expect(intentLine({ intent: "steer", scope: "turn" }, "#3")).toBe(
    "[steer → whole turn]",
  );
});

// Design 16.7 step 4. Defect: a quote in the target ends the excerpt early,
// or an escape the line adds reads as part of the text.
it("escapes backslashes, then quotes, in the line", () => {
  expect(
    intentLine({ intent: "reply", quote: excerpt('say "hi" to C:\\x\\"y') }),
  ).toBe('[reply → "say \\"hi\\" to C:\\\\x\\\\\\"y"]');
});

// Design 16.7. Defect: a target with no text gives an empty pair of quotes.
it("shows a target with no text as (no text)", () => {
  expect(intentLine({ intent: "steer", quote: excerpt(" \n ") }, "#1")).toBe(
    "[steer → #1 · (no text)]",
  );
});

// Design 16.7 ref table. Defect: a handle of another session or turn names
// another message there, since handles restart in each turn.
it("names a target by its handle only in the reader's own session and turn", () => {
  const target: ReplyTarget = {
    messageId: "m_abcdefgh2345" as MessageId,
    body: "Review PR 12",
    handledBySession: "a-chat" as SessionId,
    slot: { turnId: "turn-1" as TurnId, handle: "#2" as Handle },
  };
  const reader = (sessionId: string, turnId: string) => ({
    sessionId: sessionId as SessionId,
    turnId: turnId as TurnId,
  });
  expect(targetRef(target, reader("a-chat", "turn-1"))).toBe("#2");
  expect(targetRef(target, reader("a-chat", "turn-2"))).toBe("m_abcdefgh2345");
  expect(targetRef(target, reader("giga-chat", "turn-1"))).toBe(
    "m_abcdefgh2345",
  );
  expect(targetRef({ ...target, slot: null }, reader("a-chat", "turn-1"))).toBe(
    "m_abcdefgh2345",
  );
});

// PR I4 review r1 m2. Defect: the module builds Intl.Segmenter when it
// loads, so on a webview without it (Safari before 14.1) every import of
// monoMessages throws; or excerpt throws there at its first call.
it("loads and cuts an excerpt where Intl.Segmenter is missing", async () => {
  const segmenter = Intl.Segmenter;
  vi.resetModules();
  Reflect.deleteProperty(Intl, "Segmenter");
  try {
    // A fresh load is the subject: a static import ran before the removal.
    const fresh = await import("./messageIntent");
    expect(fresh.excerpt("one two three four five six seven eight nine")).toBe(
      "one two three four five six seven eight…",
    );
    expect(fresh.excerpt("漢".repeat(200))).toBe(`${"漢".repeat(60)}…`);
  } finally {
    Object.defineProperty(Intl, "Segmenter", {
      value: segmenter,
      writable: true,
      configurable: true,
    });
  }
});

const turn1 = "turn-1" as TurnId;
const turn2 = "turn-2" as TurnId;
const queued = (id: string, text: string, waitForIdle?: true) => ({
  id,
  text,
  attachments: [],
  ...(waitForIdle ? { waitForIdle } : {}),
});
const ready = (session: Session): Session => ({
  ...session,
  turnReadiness: "ready",
});
const blockIds = (items: RunningItem[]) => items.map((item) => item.blockId);

registerHarness({ id: "codex", live: true } as unknown as HarnessAdapter);

// Design 16.8 (intents review r1, r2). Defects: a queued message counts as
// running work; position after the last opener stands in for turn
// membership, so a bubble queued in turn 1 that opens turn 2 drags turn 1's
// joined steers into turn 2.
it("lists the opener and the blocks that joined the running turn, and nothing queued or from an earlier turn", () => {
  let session = appendUser(
    newSession("codex", "/tmp"),
    "Build the login page",
    [],
    { turnId: turn1 },
  );
  const opener = session.blocks[0].id;
  const later = queued("later", "Then the signup page");
  session = enqueueMonoMessage(ready(session), later);
  session = appendSteerUser(session, "Use the blue button", [], {
    turnId: turn1,
  });
  const steer = session.blocks.at(-1)!.id;
  session = enqueueMonoMessage(session, queued("held", "After that", true));
  expect(blockIds(runningItems(session, turn1))).toEqual([opener, steer]);
  expect(runningItems(session, turn1)[0].quote).toBe("Build the login page");

  session = acknowledgeMonoMessage(stopStreaming(session), later, {
    mode: "new-turn",
    cards: { turnId: turn2 },
  });
  session = appendSteerUser(ready(session), "Add a footer", [], {
    turnId: turn2,
  });
  const footer = session.blocks.at(-1)!.id;
  expect(blockIds(runningItems(session, turn2))).toEqual(["later", footer]);
  expect(runningItems(session, undefined)).toEqual([]);
});

// Review r1 m2. Defect: a turn that runs but cannot take a steer this moment
// (it is starting, or a question is pending) loses its items, so a picked
// steer of it fails with "That work finished" while the work still runs.
it("keeps a running turn's items while it cannot take a steer yet, and joins a picked steer of it", () => {
  const session = appendUser(newSession("codex", "/tmp"), "Fix it", [], {
    turnId: turn1,
  });
  const opener = session.blocks[0].id;
  for (const held of [
    { ...session, busy: true, turnReadiness: "starting" as const },
    {
      ...session,
      busy: true,
      turnReadiness: "ready" as const,
      pendingQuestion: { id: "q" } as Session["pendingQuestion"],
    },
  ]) {
    const items = runningItems(held, turn1);
    expect(blockIds(items)).toEqual([opener]);
    const link: TargetLink = {
      intent: "steer",
      replyTo: opener,
      quote: "Fix it",
    };
    expect(
      planUserSend({
        origin: { link },
        item: undefined,
        text: "Use the blue button",
        route: { running: turn1, items, steers: "later" },
      }),
    ).toEqual({ kind: "queue", item: { link, typedInTurn: turn1 } });
  }
});

// Review r1 m3. Defect: a hidden completion prompt or an unsent draft of the
// running turn becomes a steer target.
it("never lists a hidden or unsent block of the running turn", () => {
  let session = appendUser(
    newSession("codex", "/tmp"),
    "Mono session finished: report",
    [],
    { turnId: turn1, internal: true },
  );
  session = appendSteerUser(session, "Use the blue button", [], {
    turnId: turn1,
  });
  const steer = session.blocks.at(-1)!.id;
  session = {
    ...session,
    blocks: [
      ...session.blocks,
      {
        id: "draft",
        role: "user",
        text: "Half typed",
        draft: true,
        turnId: turn1,
      },
    ],
  };
  expect(session.blocks.filter((block) => block.turnId === turn1)).toHaveLength(
    3,
  );
  expect(blockIds(runningItems(session, turn1))).toEqual([steer]);
});

// Design 16.3, 16.12.4. Defect: an illegal link persists: new with a target,
// an unknown intent, a quote the 200 bound refuses, or a link with both a
// target and the whole-turn scope.
it("keeps a stored link only in one of its two shapes", () => {
  const link = { intent: "steer", replyTo: "block-1", quote: "Fix it" };
  expect(parseMessageLink(link)).toEqual(link);
  expect(parseMessageLink({ intent: "steer", scope: "turn" })).toEqual({
    intent: "steer",
    scope: "turn",
  });
  for (const illegal of [
    { intent: "new", replyTo: "block-1", quote: "Fix it" },
    { intent: "answer", replyTo: "block-1", quote: "Fix it" },
    { intent: "reply", replyTo: "", quote: "Fix it" },
    { intent: "reply", replyTo: "b".repeat(129), quote: "Fix it" },
    { intent: "reply", replyTo: "block-1", quote: "q".repeat(201) },
    { intent: "steer", scope: "turn", replyTo: "x" },
    { intent: "reply", scope: "turn" },
    { intent: "steer", scope: "all" },
  ])
    expect(parseMessageLink(illegal)).toBeUndefined();
  expect(parseTargetLink({ intent: "steer", scope: "turn" })).toBeUndefined();
});

// Design 16.7, 16.12.5. Defect: the provider gets no line, a plain block
// gets a ref get_message cannot take, or new work gets a line.
it("puts the line before the text the provider reads", () => {
  expect(
    linkedText(
      { intent: "steer", replyTo: "block-1", quote: 'Fix the "login" bug' },
      "Keep the API",
    ),
  ).toBe('[steer → "Fix the \\"login\\" bug"]\nKeep the API');
  expect(
    linkedText(
      { intent: "reply", replyTo: "m_abcdefgh2345", quote: "Review PR 12" },
      "Done",
    ),
  ).toBe('[reply → m_abcdefgh2345 · "Review PR 12"]\nDone');
  expect(linkedText({ intent: "steer", scope: "turn" }, "Also docs")).toBe(
    "[steer → whole turn]\nAlso docs",
  );
  expect(linkedText(undefined, "New work")).toBe("New work");
});

const running = [{ blockId: "block-1", quote: "Fix it" }];
const steerLink: TargetLink = {
  intent: "steer",
  replyTo: "block-1",
  quote: "Fix it",
};
const plan = (
  route: SendRoute,
  origin?: SendOrigin,
  item?: QueuedMessage,
  text = "Keep the API",
) => planUserSend({ origin, item, text, route });
const now = (steers: "now" | "later" | "no" = "now"): SendRoute => ({
  running: turn1,
  items: running,
  steers,
});

// Design 16.8. Defects: a steer is sent as a guess after its work ended; a
// reply or a cleared steer interrupts running work.
it("steers only into the work a link names, and never steers a reply or a cleared steer", () => {
  expect(plan(now(), { link: steerLink })).toEqual({
    kind: "join",
    block: { turnId: turn1, link: steerLink },
    text: '[steer → "Fix it"]\nKeep the API',
  });
  expect(
    plan({ running: turn1, items: [], steers: "now" }, { link: steerLink }),
  ).toEqual({ kind: "target_gone" });
  expect(plan({ opens: turn2 }, { link: steerLink })).toEqual({
    kind: "target_gone",
  });
  const reply: TargetLink = { ...steerLink, intent: "reply" };
  expect(plan(now(), { link: reply })).toEqual({
    kind: "queue",
    item: { link: reply, waitForIdle: true },
  });
  expect(plan(now(), { waitForIdle: true })).toEqual({
    kind: "queue",
    item: { waitForIdle: true },
  });
  expect(plan({ opens: turn2 }, { link: reply })).toEqual({
    kind: "open",
    block: { turnId: turn2, link: reply },
    text: '[reply → "Fix it"]\nKeep the API',
  });
});

// Design 16.12.4, I5 row. Defects: the user's steer stays implicit; the line
// leaks into the block or never reaches the provider; the joined block or
// the opener misses its turn id; a busy-rule queue gets a link.
it("makes the user's own untargeted steer whole-turn, with the line only in the provider text", () => {
  expect(plan(now())).toEqual({
    kind: "join",
    block: { turnId: turn1, link: { intent: "steer", scope: "turn" } },
    text: "[steer → whole turn]\nKeep the API",
  });
  expect(plan(now("no"))).toEqual({ kind: "queue", item: {} });
  expect(plan({ opens: turn2 })).toEqual({
    kind: "open",
    block: { turnId: turn2 },
    text: "Keep the API",
  });
  const cleared = plan({ opens: turn2 }, undefined, {
    id: "q",
    text: "Keep the API",
    attachments: [],
    waitForIdle: true,
  });
  expect(cleared).toEqual({
    kind: "open",
    block: { turnId: turn2 },
    text: "Keep the API",
  });
});

// Design 16.12.4 (whole-turn review r1), review r1 m1 and decision 553.
// Defects: another sender's text, handed off through the user's path, is
// labelled as the user's steer and gets a second line, or picks up the
// user's link from its queue item.
it("gives no link and no line to text the user did not type", () => {
  const row: QueuedMessage = {
    id: "m_abcdefgh2345",
    text: "Review PR 12",
    attachments: [],
    monoMessageId: "m_abcdefgh2345" as MessageId,
  };
  const rowText = '<mono_message intent="new">Review PR 12</mono_message>';
  const cli: QueuedMessage = {
    id: "cli",
    text: "Run the tests",
    attachments: [],
    appRequestId: "req-1",
    link: steerLink,
  };
  const others: [SendOrigin | undefined, QueuedMessage | undefined][] = [
    [{ agentText: rowText }, row],
    [undefined, cli],
    [{ managed: true }, undefined],
    [{ ciRepair: { prompt: "Fix CI" } }, undefined],
    [{ appRequestId: "req-2" }, undefined],
  ];
  for (const [origin, item] of others) {
    expect(plan(now(), origin, item, rowText)).toEqual({
      kind: "join",
      block: { turnId: turn1 },
      text: rowText,
    });
    expect(plan({ opens: turn2 }, origin, item, rowText)).toEqual({
      kind: "open",
      block: { turnId: turn2 },
      text: rowText,
    });
  }
});

// Review r1 M1. Defect: a message typed during turn 1 that missed it is
// steered into turn 2, which another sender's queued message opened, and
// is labelled there as the user's whole-turn steer.
it("steers a message the user typed only into the turn it was typed in, and opens its own turn once that turn ended", () => {
  let chat: Session = {
    ...appendUser(newSession("codex", "/tmp"), "Build it", [], {
      turnId: turn1,
    }),
    busy: true,
    turnReadiness: "starting",
  };
  const w: QueuedMessage = {
    id: "m_wwwwwwwwwwww",
    text: "<mono_message>please review PR 12</mono_message>",
    attachments: [],
    waitForIdle: true,
    monoMessageId: "m_wwwwwwwwwwww" as MessageId,
  };
  chat = enqueueMonoMessage(chat, w);
  const typed = plan(
    { running: turn1, items: runningItems(chat, turn1), steers: "later" },
    undefined,
    undefined,
    "stop, use the staging DB",
  );
  expect(typed).toEqual({ kind: "queue", item: { typedInTurn: turn1 } });
  const a: QueuedMessage = {
    id: "typed-a",
    text: "stop, use the staging DB",
    attachments: [],
    ...(typed.kind === "queue" ? typed.item : {}),
  };
  chat = enqueueMonoMessage(chat, a);
  expect(steerableQueuedMessage(ready(chat), turn1)?.id).toBe(a.id);

  // Turn 1 ends before it can take the steer; W opens turn 2.
  chat = { ...stopStreaming(chat), busy: false, queueStatus: "active" };
  expect(queuedHead(chat)?.id).toBe(w.id);
  chat = acknowledgeMonoMessage(chat, w, {
    mode: "new-turn",
    cards: { turnId: turn2 },
  });
  chat = { ...chat, busy: true, turnReadiness: "ready" };
  expect(steerableQueuedMessage(chat, turn2)).toBeUndefined();

  // Turn 2 ends; the message opens its own turn with no link and no line.
  chat = { ...stopStreaming(chat), busy: false };
  expect(canDispatchQueuedHead(chat) && queuedHead(chat)?.id).toBe(a.id);
  const turn3 = "turn-3" as TurnId;
  expect(plan({ opens: turn3 }, undefined, queuedHead(chat), a.text)).toEqual({
    kind: "open",
    block: { turnId: turn3 },
    text: "stop, use the staging DB",
  });
});

// Review r1 M1. Defect: a message typed while no turn runs (a Mono chat
// with a queue that has not started) steers whichever turn opens next.
it("lets a message typed while no turn runs wait for the turn end", () => {
  expect(plan({ running: undefined, items: [], steers: "later" })).toEqual({
    kind: "queue",
    item: { waitForIdle: true },
  });
});

// PR I5b review r3 m1. Defect: in a chat compacting by hand (busy, no turn),
// the user's text, or any sender's, is planned as a steer joined into no
// turn, though nothing can take it until the compaction ends.
it("lets text sent while a chat compacts wait for the compaction to end", () => {
  const ran = appendUser(newSession("codex", "/tmp"), "Build it", [], {
    turnId: turn1,
  });
  // As App.tsx builds a manual compaction.
  const compacting = applyHarnessEvent(
    { ...stopStreaming(ran), busy: true },
    { type: "status", text: "Compacting context…" },
  );
  const route = busyRoute(compacting, runningTurnMap(), "now");
  for (const origin of [{}, { managed: true }] satisfies SendOrigin[])
    expect(plan(route, origin)).toEqual({
      kind: "queue",
      item: { waitForIdle: true },
    });
  expect(plan(route, { link: steerLink })).toEqual({ kind: "target_gone" });

  const queued: Session = {
    ...compacting,
    queuedMessages: [
      { id: "a", text: "Keep the API", attachments: [], waitForIdle: true },
    ],
  };
  expect(canDispatchQueuedHead(queued)).toBe(false);
  const done = { ...queued, busy: false };
  expect(canDispatchQueuedHead(done) && queuedHead(done)?.id).toBe("a");
});

// PR I6 review r3 m2 and n4. Defect: Steer on a queued row whose send App
// puts in the queue (a worktree in preparation, a plan row) or that finds
// no turn to join plans a queue, and App appends a copy of the row, so its
// text is sent twice.
it("keeps a queued row in its place when its Steer would only queue it again", () => {
  const running: Session = {
    ...appendUser(newSession("codex", "/tmp"), "Build it", [], {
      turnId: turn1,
    }),
    busy: true,
    turnReadiness: "ready",
  };
  const turns = runningTurnMap([[running.id, turn1]]);
  const row: QueuedMessage = { id: "q", text: "also docs", attachments: [] };
  const compacting: Session = { ...stopStreaming(running), busy: true };
  // App's busy branch plans a forced queue with "no" and a Steer with "now".
  const steers: [Session, QueuedMessage, SendRoute][] = [
    [
      { ...running, turnReadiness: "starting", worktreePreparing: true },
      row,
      busyRoute(running, turns, "no"),
    ],
    [running, { ...row, intent: "plan" }, busyRoute(running, turns, "no")],
    [compacting, row, busyRoute(compacting, runningTurnMap(), "now")],
  ];
  for (const [session, item, route] of steers) {
    const resolved = queuedMessageForSubmit(
      { ...session, queuedMessages: [item] },
      item.id,
      "steer",
    );
    expect(resolved).toBe(item);
    expect(
      planUserSend({ origin: {}, item: resolved, text: item.text, route }),
    ).toEqual({ kind: "stays" });
  }
  // The same text typed in the composer is a new queue item.
  expect(plan(busyRoute(running, turns, "no"))).toEqual({
    kind: "queue",
    item: {},
  });
});

// Design 16.12.6. Defects: a guessed target, a blocked send, or a picked
// steer whose work ended still shown as live.
it("shows the whole turn with no pick while steering, and a picked steer as gone once its work ended", () => {
  expect(composerTarget(running, null, true)).toEqual({ kind: "turn" });
  expect(
    composerTarget(
      [...running, { blockId: "block-2", quote: "Also" }],
      null,
      true,
    ),
  ).toEqual({ kind: "turn" });
  expect(composerTarget(running, null, false)).toEqual({ kind: "none" });
  expect(composerTarget(running, steerLink, true)).toEqual({
    kind: "picked",
    link: steerLink,
  });
  expect(composerTarget([], steerLink, true)).toEqual({
    kind: "gone",
    link: steerLink,
  });
  const reply: TargetLink = { ...steerLink, intent: "reply" };
  expect(composerTarget([], reply, false)).toEqual({
    kind: "picked",
    link: reply,
  });
});
