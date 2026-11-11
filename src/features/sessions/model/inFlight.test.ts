import { describe, expect, it } from "vitest";
import { newTab } from "../../workspace/model/layout";
import {
  CONTINUE_PROMPT,
  INTERRUPT_MESSAGE,
  canAutoContinue,
  hasInFlightSessions,
  inFlightRefs,
  isInFlightSession,
  markQuitSessions,
  markTurnInterrupted,
  quitWhileBusyMessage,
  shouldWriteInFlightSnapshot,
  workspaceFromResumed,
} from "./inFlight";
import { newSession, type Session } from "./session";

function chat(
  cwd: string,
  patch: Partial<Session> = {},
): Session {
  const session = newSession("cursor", cwd);
  session.blocks = [{ id: "u1", role: "user", text: "hello" }];
  return { ...session, ...patch, blocks: patch.blocks ?? session.blocks };
}

describe("isInFlightSession", () => {
  it("is true for a busy turn, a live approval, or a parked question", () => {
    expect(isInFlightSession(chat("/tmp/a"))).toBe(false);
    expect(isInFlightSession(chat("/tmp/a", { busy: true }))).toBe(true);
    expect(
      isInFlightSession(
        chat("/tmp/a", {
          blocks: [
            { id: "u1", role: "user", text: "hello" },
            {
              id: "a1",
              role: "approval",
              text: "run rm",
              approval: { requestId: 1 },
            },
          ],
        }),
      ),
    ).toBe(true);
    expect(
      isInFlightSession(
        chat("/tmp/a", {
          pendingQuestion: {
            requestId: 4,
            questions: [
              {
                id: "q1",
                prompt: "Which file?",
                multiSelect: false,
                allowCustom: true,
                options: [{ id: "a.ts", label: "a.ts" }],
              },
            ],
          },
        }),
      ),
    ).toBe(true);
  });
});

describe("inFlightRefs", () => {
  it("walks open tabs first, then parked busy sessions", () => {
    const parked = chat("/tmp/parked", { busy: true });
    const openBusy = chat("/tmp/open", { busy: true });
    const idle = chat("/tmp/idle");
    const blank = newSession("cursor", "/tmp/blank");
    blank.busy = true;

    const tabs = [newTab(idle.id), newTab(openBusy.id)];
    expect(
      inFlightRefs([parked, openBusy, idle, blank], tabs),
    ).toEqual([
      { sessionId: openBusy.id, cwd: "/tmp/open" },
      { sessionId: parked.id, cwd: "/tmp/parked" },
    ]);
  });

  it("skips chats that were never persisted", () => {
    const blank = newSession("cursor", "/tmp/a");
    blank.busy = true;
    expect(inFlightRefs([blank], [newTab(blank.id)])).toEqual([]);
    expect(hasInFlightSessions([blank])).toBe(true);
  });

  it("does not resume a session whose worktree was removed", () => {
    const removed = chat("/tmp/a", {
      busy: true,
      worktreeCwd: "/tmp/a-worktrees/feature",
      worktreeRemoved: true,
    });
    expect(isInFlightSession(removed)).toBe(false);
    expect(inFlightRefs([removed], [newTab(removed.id)])).toEqual([]);
  });
});

describe("markTurnInterrupted", () => {
  it("seals the stream, cancels open tools, and appends a system note", () => {
    const interrupted = markTurnInterrupted(
      chat("/tmp/a", {
        busy: true,
        blocks: [
          { id: "u1", role: "user", text: "hello", startedAt: 1_000 },
          { id: "a1", role: "assistant", text: "Working", streaming: true },
          {
            id: "t1",
            role: "tool",
            text: "edit",
            streaming: true,
            tool: { status: "running", title: "edit" },
          },
          {
            id: "p1",
            role: "approval",
            text: "allow?",
            approval: { requestId: 7 },
          },
        ],
      }),
    );

    expect(interrupted.busy).toBe(false);
    expect(interrupted.blocks).toEqual([
      expect.objectContaining({
        id: "u1",
        durationMs: expect.any(Number),
      }),
      expect.objectContaining({
        id: "a1",
        streaming: false,
        text: "Working",
      }),
      expect.objectContaining({
        id: "t1",
        streaming: false,
        tool: expect.objectContaining({ status: "cancelled" }),
      }),
      expect.objectContaining({
        role: "system",
        text: INTERRUPT_MESSAGE,
      }),
    ]);
  });

  // ADE-195, from the e2e-macos-1 ade89-c failure: the quit cut the turn a
  // completion notice opened before the agent had its prompt. A Continue
  // does not carry the notice, so it goes back to the head of the queue.
  const notice = {
    id: "mono-completion-r1",
    role: "user" as const,
    text: "MonoCode completion notification: a session you are monitoring has now stopped.",
    appRequestId: "mono-completion-r1",
    internal: true,
    startedAt: 1_000,
    monoSessionCompletion: {
      sessionId: "s-late",
      title: "Agent session",
      status: "cancelled" as const,
    },
  };
  const mono = (patch: Partial<Session>) =>
    chat("/tmp/a", {
      busy: true,
      providerSessionId: "p1",
      ...patch,
      blocks: [
        { id: "u0", role: "user", text: "watch the work" },
        { id: "a0", role: "assistant", text: "Started." },
        ...(patch.blocks ?? [notice]),
      ],
    });

  it("queues again a notice whose turn the agent never answered", () => {
    const later = {
      id: "mono-completion-r2",
      text: "MonoCode completion notification: later",
      attachments: [],
      pausedBy: "quit" as const,
    };
    const restored = markTurnInterrupted(
      mono({ queuedMessages: [later], queueStatus: "paused" }),
    );

    expect(restored.busy).toBe(false);
    expect(restored.blocks.map((block) => block.id)).toEqual(["u0", "a0"]);
    expect(restored.queuedMessages).toEqual([
      expect.objectContaining({
        id: "mono-completion-r1",
        text: notice.text,
        attachments: [],
        monoSessionCompletion: notice.monoSessionCompletion,
        pausedBy: "quit",
      }),
      later,
    ]);
    expect(restored.queueStatus).toBe("paused");
    expect(canAutoContinue(restored)).toBe(false);
  });

  // r4 M2: a running queue is paused by the quit, so the relaunch wakes it.
  it("gives the notice its quit reason when the queue was running", () => {
    const typed = { id: "q1", text: "wait", attachments: [] };
    const restored = markTurnInterrupted(
      mono({ queuedMessages: [typed], queueStatus: "active" }),
    );
    expect(restored.queuedMessages).toEqual([
      expect.objectContaining({ id: "mono-completion-r1", pausedBy: "quit" }),
      typed,
    ]);
    expect(restored.queueStatus).toBe("paused");
  });

  // r4 M1: other app-delivered turns (a Mono message, an ask) carry an
  // appRequestId too, but no completion: the quit marks them as usual.
  it("marks a cut app-delivered turn that is no completion notice", () => {
    const typed = { id: "q1", text: "wait", attachments: [] };
    const restored = markTurnInterrupted(
      mono({
        blocks: [
          {
            id: "m1",
            role: "user",
            text: "A Mono message",
            appRequestId: "mono-message-1",
          },
        ],
        queuedMessages: [typed],
      }),
    );
    expect(restored.blocks.map((block) => block.id)).toEqual([
      "u0",
      "a0",
      "m1",
      expect.any(String),
    ]);
    expect(restored.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
    expect(restored.queuedMessages).toEqual([typed]);
  });

  it("keeps a queue the user paused paused when it requeues the notice", () => {
    const typed = { id: "q1", text: "wait", attachments: [] };
    const restored = markTurnInterrupted(
      mono({ queuedMessages: [typed], queueStatus: "paused" }),
    );
    expect(restored.queuedMessages?.[0]).toEqual(
      expect.not.objectContaining({ pausedBy: "quit" }),
    );
    expect(restored.queuedMessages?.[1]).toBe(typed);
  });

  it("marks a notice's turn interrupted once the agent answered", () => {
    const answered = markTurnInterrupted(
      mono({
        blocks: [notice, { id: "a1", role: "assistant", text: "Review" }],
      }),
    );
    expect(answered.queuedMessages).toBeUndefined();
    expect(answered.blocks.map((block) => block.id)).toContain(notice.id);
    expect(canAutoContinue(answered)).toBe(true);
  });

  // r1 M1: a graceful quit marks the session when it saves it, and the
  // relaunch marks the saved record again.
  // r1 M1: a graceful quit marks the session when it saves it. The relaunch
  // marks only the chats in the saved in-flight list (recordToSession loads
  // every record idle), so a requeued notice must not be in that list.
  it("requeues the notice once across the quit save and the relaunch", () => {
    const cutTurn = chat("/tmp/b", { busy: true });
    const idle = chat("/tmp/c");
    const sessions = [mono({}), cutTurn, idle];
    const quit = markQuitSessions(
      sessions,
      sessions.map((session) => newTab(session.id)),
    );
    // r4 M4: a chat the quit did not cut is saved as it was.
    expect(quit.sessions[2]).toBe(idle);
    expect(quit.cut.map((ref) => ref.sessionId)).toEqual([
      sessions[0].id,
      cutTurn.id,
    ]);
    expect(quit.refs).toEqual([{ sessionId: cutTurn.id, cwd: "/tmp/b" }]);

    const listed = new Set(quit.refs.map((ref) => ref.sessionId));
    const [notice1, cut1] = quit.sessions.map((saved) => {
      const record = { ...saved, busy: false };
      return listed.has(record.id) ? markTurnInterrupted(record) : record;
    });
    expect(notice1.blocks.map((block) => block.id)).toEqual(["u0", "a0"]);
    expect(notice1.queuedMessages?.map((message) => message.id)).toEqual([
      "mono-completion-r1",
    ]);
    expect(
      notice1.blocks.some((block) => block.text === INTERRUPT_MESSAGE),
    ).toBe(false);
    expect(canAutoContinue(notice1)).toBe(false);
    expect(
      cut1.blocks.filter((block) => block.text === INTERRUPT_MESSAGE),
    ).toHaveLength(1);
  });

  // r2 M1: a relaunch after a kill loads every record idle. A cut turn with
  // a notice waiting at the queue head still gets the note and a Continue.
  it("marks a reloaded cut turn with a notice waiting at the queue head", () => {
    const waiting = {
      id: "mono-completion-r3",
      text: notice.text,
      attachments: [],
      monoSessionCompletion: notice.monoSessionCompletion,
    };
    const restored = markTurnInterrupted(
      mono({
        busy: false,
        blocks: [{ id: "u1", role: "user", text: "go on" }],
        queuedMessages: [waiting],
      }),
    );
    expect(restored.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
    expect(restored.queuedMessages).toEqual([waiting]);
    expect(canAutoContinue(restored)).toBe(true);
  });

  it("keeps a system note that follows the notice's opener", () => {
    const error = {
      id: "e1",
      role: "system" as const,
      text: "provider failed",
      notice: "error" as const,
    };
    const restored = markTurnInterrupted(mono({ blocks: [notice, error] }));
    expect(restored.blocks.map((block) => block.id)).toEqual([
      "u0",
      "a0",
      "e1",
    ]);
    expect(restored.queuedMessages?.[0]?.id).toBe("mono-completion-r1");
  });

  it("marks the turn interrupted when the notice opener has no request id", () => {
    const restored = markTurnInterrupted(
      mono({ blocks: [{ ...notice, appRequestId: undefined }] }),
    );
    expect(restored.queuedMessages).toBeUndefined();
    expect(restored.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
  });

  it("wakes an empty paused queue with the requeued notice", () => {
    const restored = markTurnInterrupted(
      mono({ queuedMessages: [], queueStatus: "paused" }),
    );
    expect(restored.queuedMessages).toEqual([
      expect.objectContaining({ id: "mono-completion-r1", pausedBy: "quit" }),
    ]);
  });

  it("marks a busy, asking or idle turn with a waiting notice interrupted", () => {
    const waiting = {
      id: "mono-completion-r3",
      text: notice.text,
      attachments: [],
      monoSessionCompletion: notice.monoSessionCompletion,
    };
    const busy = markTurnInterrupted(
      mono({
        blocks: [{ id: "u1", role: "user", text: "go on" }],
        queuedMessages: [waiting],
      }),
    );
    expect(busy.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
    const asking = markTurnInterrupted(
      mono({
        busy: false,
        blocks: [{ id: "u1", role: "user", text: "go on" }],
        pendingQuestion: { requestId: 1, questions: [], autoResolveAt: 1000 },
        queuedMessages: [waiting],
      }),
    );
    expect(asking.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
    expect(asking.queuedMessages).toEqual([waiting]);
    const typed = { id: "q1", text: "wait", attachments: [] };
    const idle = markTurnInterrupted(
      mono({
        busy: false,
        blocks: [{ id: "u1", role: "user", text: "go on" }],
        queuedMessages: [typed],
      }),
    );
    expect(idle.blocks.at(-1)?.text).toBe(INTERRUPT_MESSAGE);
  });

  it("does not append the interrupt note twice for the same turn", () => {
    const once = markTurnInterrupted(chat("/tmp/a", { busy: true }));
    const twice = markTurnInterrupted(once);
    expect(
      twice.blocks.filter((block) => block.text === INTERRUPT_MESSAGE),
    ).toHaveLength(1);
  });

  it("appends a new note when a later turn is quit after Continue", () => {
    const first = markTurnInterrupted(
      chat("/tmp/a", { busy: true, providerSessionId: "p1" }),
    );
    const continued: Session = {
      ...first,
      blocks: [
        ...first.blocks,
        { id: "c1", role: "user", text: CONTINUE_PROMPT },
        { id: "a2", role: "assistant", text: "resumed" },
        { id: "u2", role: "user", text: "edit the readme" },
        {
          id: "t2",
          role: "tool",
          text: "read",
          tool: { status: "completed", title: "read" },
        },
      ],
      busy: true,
    };
    const second = markTurnInterrupted(continued);
    expect(
      second.blocks.filter((block) => block.text === INTERRUPT_MESSAGE),
    ).toHaveLength(2);
    expect(second.blocks[second.blocks.length - 1]?.text).toBe(INTERRUPT_MESSAGE);
    expect(canAutoContinue(second)).toBe(true);
  });

  it("leaves finished tools alone", () => {
    const interrupted = markTurnInterrupted(
      chat("/tmp/a", {
        busy: true,
        blocks: [
          { id: "u1", role: "user", text: "hello" },
          {
            id: "t1",
            role: "tool",
            text: "read",
            tool: { status: "completed", title: "read" },
          },
        ],
      }),
    );
    expect(interrupted.blocks[1]?.tool?.status).toBe("completed");
  });
});

describe("quitWhileBusyMessage", () => {
  it("mentions resume on reopen", () => {
    expect(quitWhileBusyMessage(1)).toContain("1 chat is still running");
    expect(quitWhileBusyMessage(3)).toContain("3 chats are still running");
  });
});

describe("workspaceFromResumed", () => {
  it("opens one tab per restored chat", () => {
    const first = chat("/tmp/a");
    const second = chat("/tmp/b");
    const workspace = workspaceFromResumed([first, second]);
    expect(workspace?.projectCwd).toBe("/tmp/a");
    expect(workspace?.sessions.map((session) => session.id)).toEqual([
      first.id,
      second.id,
    ]);
    expect(workspace?.tabs).toHaveLength(2);
    expect(workspace?.tabs[0]?.focusedId).toBe(first.id);
    expect(workspace?.activeTabId).toBe(workspace?.tabs[0]?.id);
  });

  it("returns null when there is nothing to restore", () => {
    expect(workspaceFromResumed([])).toBeNull();
  });
});

describe("canAutoContinue", () => {
  it("needs a provider thread and an interrupt note as the last block", () => {
    const interrupted = markTurnInterrupted(
      chat("/tmp/a", { busy: true, providerSessionId: "p1" }),
    );
    expect(canAutoContinue(interrupted)).toBe(true);
    expect(canAutoContinue(chat("/tmp/a", { providerSessionId: "p1" }))).toBe(
      false,
    );
    expect(canAutoContinue(markTurnInterrupted(chat("/tmp/a", { busy: true })))).toBe(
      false,
    );
    expect(
      canAutoContinue({
        ...interrupted,
        blocks: [
          ...interrupted.blocks,
          { id: "c1", role: "user", text: "Continue from where you left off." },
        ],
      }),
    ).toBe(false);
    expect(canAutoContinue({ ...interrupted, busy: true })).toBe(false);
    expect(canAutoContinue({ ...interrupted, worktreeRemoved: true })).toBe(
      false,
    );
  });
});

describe("shouldWriteInFlightSnapshot", () => {
  it("does not wipe a disk snapshot on the first idle paint", () => {
    expect(shouldWriteInFlightSnapshot("", [], null, false)).toBe(false);
  });

  it("writes when a chat becomes in-flight", () => {
    expect(
      shouldWriteInFlightSnapshot("a", [{ sessionId: "a", cwd: "/tmp" }], null, false),
    ).toBe(true);
  });

  it("clears after this process has seen an in-flight chat", () => {
    expect(shouldWriteInFlightSnapshot("", [], "a", true)).toBe(true);
  });

  it("skips unchanged keys", () => {
    expect(
      shouldWriteInFlightSnapshot("a", [{ sessionId: "a", cwd: "/tmp" }], "a", true),
    ).toBe(false);
  });
});
