import { beforeEach, expect, it, vi } from "vitest";
import type { Block, Session } from "../../sessions/model/session";
import { endHabitQuestion } from "./habitQuestion";
import type * as HostModule from "./habitQuestionHost";
import type { ApprovalRelay } from "./monoHabits";

let hostModule: typeof HostModule;

// The binding is module state: each scenario loads a fresh module, so it
// starts from an app that has not bound its store yet. A static import would
// share one binding across the scenarios.
beforeEach(async () => {
  vi.resetModules();
  hostModule = await import("./habitQuestionHost");
});

const userLine: Block = {
  id: "user-1",
  role: "user",
  text: "Water the plants",
};

const card: Block = {
  id: "card-1",
  role: "assistant",
  text: "",
  monoHabit: { id: "habit-1", name: "Garden round", at: 1_000 },
  habitQuestion: {
    runId: "run-1",
    runAt: 1_000,
    prompt: {
      requestId: 1_000_000_000,
      questions: [
        {
          id: "water",
          prompt: "Water the plants today?",
          multiSelect: false,
          allowCustom: false,
          options: [
            { id: "yes", label: "Yes" },
            { id: "no", label: "No" },
          ],
        },
      ],
    },
  },
};

const mono = {
  id: "mono-1",
  blocks: [userLine, card],
} as Session;

const otherChat = {
  id: "chat-1",
  blocks: [{ id: "card-1", role: "assistant", text: "Same id, other chat" }],
} as Session;

const relay = (fields: Partial<ApprovalRelay> = {}): ApprovalRelay => ({
  monoId: "mono-1",
  runId: "run-1",
  requestId: 3,
  blockId: "card-1",
  ...fields,
});

const stop = (block: Block) => endHabitQuestion(block, { kind: "stopped" });

function bindStore() {
  const store = {
    sessions: { current: [otherChat, mono] },
    setSessions: vi.fn<(sessions: Session[]) => void>(),
    persist: vi.fn<(session: Session | undefined) => void>(),
    append: vi.fn<(monoId: string, block: Block) => void>(),
  };
  hostModule.setHabitQuestionHost(
    store.sessions,
    store.setSessions,
    store.persist,
    store.append,
  );
  return store;
}

// Defect: a settle before the app binds its store throws, or writes somewhere.
it("does nothing before the app binds its store", () => {
  const update = vi.fn(stop);
  expect(() => hostModule.settleHabitCard(relay(), update)).not.toThrow();
  expect(update).not.toHaveBeenCalled();
});

// Defect: the settle changes the wrong chat or block, drops the others, or
// does not save and show the settled chat.
it("settles only the card the relay points at, then shows and saves the chat", () => {
  const store = bindStore();
  const before = store.sessions.current;

  hostModule.settleHabitCard(relay(), stop);

  const settled = { ...mono, blocks: [userLine, stop(card)] };
  expect(settled.blocks[1].habitQuestion?.end).toEqual({ kind: "stopped" });
  expect(store.sessions.current).toEqual([otherChat, settled]);
  expect(store.sessions.current[0]).toBe(otherChat);
  expect(store.sessions.current[1].blocks[0]).toBe(userLine);
  expect(store.setSessions).toHaveBeenCalledTimes(1);
  expect(store.setSessions).toHaveBeenCalledWith(store.sessions.current);
  expect(store.persist).toHaveBeenCalledTimes(1);
  expect(store.persist).toHaveBeenCalledWith(settled);
  expect(store.append).not.toHaveBeenCalled();
  expect(before).toEqual([otherChat, mono]);
});

// Defect: a relay whose chat or card is gone throws, calls the update with
// nothing, or writes the store anyway.
it("leaves the store alone when the relay's chat or card is gone", () => {
  const store = bindStore();
  const update = vi.fn(stop);

  hostModule.settleHabitCard(relay({ monoId: "mono-gone" }), update);
  hostModule.settleHabitCard(relay({ blockId: "card-gone" }), update);

  expect(update).not.toHaveBeenCalled();
  expect(store.sessions.current).toEqual([otherChat, mono]);
  expect(store.setSessions).not.toHaveBeenCalled();
  expect(store.persist).not.toHaveBeenCalled();
});
