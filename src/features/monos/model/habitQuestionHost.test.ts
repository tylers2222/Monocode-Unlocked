// @vitest-environment happy-dom
import {
  act,
  createElement,
  useRef,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi, type Mock } from "vitest";
import type { Block, Session } from "../../sessions/model/session";
import type { UserQuestionReply } from "../../sessions/model/userQuestion";
import { answerHabitQuestion, endHabitQuestion } from "./habitQuestion";
import type * as HostModule from "./habitQuestionHost";
import type * as HabitsModule from "./monoHabits";
import type { ApprovalRelay } from "./monoHabits";

let hostModule: typeof HostModule;
let habits: typeof HabitsModule;

// The binding is module state: each scenario loads a fresh module, so it
// starts from an app that has not bound its store yet. A static import would
// share one binding across the scenarios.
let root: Root | undefined;

beforeEach(async () => {
  vi.resetModules();
  hostModule = await import("./habitQuestionHost");
  // The same fresh module the host reads its relays from.
  habits = await import("./monoHabits");
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
});

afterEach(() => {
  act(() => root?.unmount());
  root = undefined;
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

const laterCard: Block = { ...card, id: "card-2" };

const mono = {
  id: "mono-1",
  title: "Garden",
  blocks: [userLine, card, laterCard],
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

type Store = {
  shown: Session[];
  sessions: RefObject<Session[]>;
  setSessions: Dispatch<SetStateAction<Session[]>>;
  persist: Mock<(session: Session | undefined) => void>;
  append: Mock<(monoId: string, block: Block) => void>;
};

// Renders a React store and binds it the way the app does on every render.
function mountStore(initial: Session[] = [otherChat, mono]): Store {
  const store = {
    persist: vi.fn<(session: Session | undefined) => void>(),
    append: vi.fn<(monoId: string, block: Block) => void>(),
  } as Store;
  function App() {
    const [sessions, setSessions] = useState<Session[]>(initial);
    const sessionsRef = useRef(sessions);
    sessionsRef.current = sessions;
    hostModule.setHabitQuestionHost(
      sessionsRef,
      setSessions,
      store.persist,
      store.append,
    );
    Object.assign(store, {
      shown: sessions,
      sessions: sessionsRef,
      setSessions,
    });
    return null;
  }
  root = createRoot(document.createElement("div"));
  act(() => root?.render(createElement(App)));
  return store;
}

// Defect: a settle before the app binds its store throws, or writes somewhere.
it("does nothing before the app binds its store", () => {
  const update = vi.fn(stop);
  expect(() => hostModule.settleHabitCard(relay(), update)).not.toThrow();
  expect(update).not.toHaveBeenCalled();
});

// Defect: the settle changes the wrong chat or block, drops the others, does
// not save and show the settled chat, or writes into the old chat or card.
it("settles only the card the relay points at, then shows and saves the chat", () => {
  const store = mountStore();
  const before = store.shown;
  const untouched = structuredClone(before);

  act(() => hostModule.settleHabitCard(relay(), stop));

  const settled = { ...mono, blocks: [userLine, stop(card), laterCard] };
  expect(settled.blocks[1].habitQuestion?.end).toEqual({ kind: "stopped" });
  expect(store.shown).toEqual([otherChat, settled]);
  expect(store.shown[0]).toBe(otherChat);
  expect(store.shown[1].blocks[0]).toBe(userLine);
  expect(store.shown[1].blocks[2]).toBe(laterCard);
  expect(store.persist).toHaveBeenCalledTimes(1);
  expect(store.persist).toHaveBeenCalledWith(settled);
  expect(store.append).not.toHaveBeenCalled();
  expect(before).toEqual(untouched);
});

// Defect: the settle replaces React's state with the runtime snapshot, so a UI
// change queued in the same tick is lost, or the runtime snapshot misses the
// settle until React renders.
it("keeps a UI change React has queued but not rendered", () => {
  const store = mountStore();

  act(() => {
    store.setSessions((sessions) =>
      sessions.map((session) =>
        session.id === mono.id ? { ...session, title: "Draft title" } : session,
      ),
    );
    hostModule.settleHabitCard(relay(), stop);
    expect(store.sessions.current[1].blocks[1]).toEqual(stop(card));
  });

  expect(store.shown).toEqual([
    otherChat,
    {
      ...mono,
      title: "Draft title",
      blocks: [userLine, stop(card), laterCard],
    },
  ]);
  expect(store.shown[0]).toBe(otherChat);
});

// Defect: a relay whose chat or card is gone throws, calls the update with
// nothing, or writes the store anyway.
it("leaves the store alone when the relay's chat or card is gone", () => {
  const store = mountStore();
  const before = store.shown;
  const update = vi.fn(stop);

  act(() => {
    hostModule.settleHabitCard(relay({ monoId: "mono-gone" }), update);
    hostModule.settleHabitCard(relay({ blockId: "card-gone" }), update);
  });

  expect(update).not.toHaveBeenCalled();
  expect(store.shown).toBe(before);
  expect(store.persist).not.toHaveBeenCalled();
});

const run = {
  id: "run-1",
  title: "Garden round run",
  blocks: [userLine],
} as Session;

const yes: UserQuestionReply = {
  kind: "answered",
  answers: { water: ["yes"] },
};

type Answer = Mock<
  (sessionId: string, requestId: number, reply: UserQuestionReply) => void
>;

// The run asked its question 7; the Mono's card asks it under the relay's id.
function askedInMono(): number {
  return habits.relayApproval({
    monoId: "mono-1",
    runId: "run-1",
    requestId: 7,
    blockId: "card-1",
  });
}

// Defect: a question that is no habit's is rerouted, answered twice, answered
// with other ids or reply, settles a card, or the app's panes get a new answer
// function on each render.
it("passes a question that is no habit's straight to the app's answer path", () => {
  const store = mountStore([otherChat, mono, run]);
  const relayId = askedInMono();
  const before = store.shown;
  const onQuestionReply: Answer = vi.fn();
  const answer = hostModule.habitQuestionReply(onQuestionReply);
  expect(hostModule.habitQuestionReply(onQuestionReply)).toBe(answer);

  // Another chat; the Mono's own question; the run's other question.
  for (const [sessionId, requestId] of [
    ["chat-1", relayId],
    ["mono-1", 3],
    ["run-1", 8],
  ] as const) {
    onQuestionReply.mockClear();
    act(() => answer(sessionId, requestId, yes));
    expect(onQuestionReply).toHaveBeenCalledTimes(1);
    expect(onQuestionReply).toHaveBeenCalledWith(sessionId, requestId, yes);
    expect(onQuestionReply.mock.calls[0][2]).toBe(yes);
  }

  expect(store.shown).toBe(before);
  expect(store.persist).not.toHaveBeenCalled();
});

// Defect: the habit's answer misses the run, reaches it under the Mono's id,
// reaches it twice, leaves the Mono's card open, settles it outside the store's
// one write path, or does not save the Mono's chat.
it.each([
  ["the Mono's card", (relayId: number) => ["mono-1", relayId] as const],
  ["the run's own card", () => ["run-1", 7] as const],
])("answers the run once from %s and settles the Mono's card", (_, ask) => {
  const store = mountStore([otherChat, mono, run]);
  const [sessionId, requestId] = ask(askedInMono());
  const before = store.shown;
  const untouched = structuredClone(before);
  const onQuestionReply: Answer = vi.fn();
  const answer = hostModule.habitQuestionReply(onQuestionReply);

  act(() => answer(sessionId, requestId, yes));

  expect(onQuestionReply).toHaveBeenCalledTimes(1);
  expect(onQuestionReply).toHaveBeenCalledWith("run-1", 7, yes);
  const settled = {
    ...mono,
    blocks: [userLine, answerHabitQuestion(card, yes), laterCard],
  };
  expect(settled.blocks[1].habitQuestion?.end).toEqual({
    kind: "answered",
    answer: "Yes",
  });
  expect(store.shown).toEqual([otherChat, settled, run]);
  expect(store.sessions.current).toEqual([otherChat, settled, run]);
  expect(store.persist).toHaveBeenCalledTimes(1);
  expect(store.persist).toHaveBeenCalledWith(settled);
  expect(before).toEqual(untouched);
});

// Defect: the card reads as answered though the run that asked is gone, so
// nothing got the answer.
it("leaves the Mono's card open when the run that asked is gone", () => {
  const store = mountStore([otherChat, mono]);
  const relayId = askedInMono();
  const before = store.shown;
  const onQuestionReply: Answer = vi.fn();

  const answer = hostModule.habitQuestionReply(onQuestionReply);
  act(() => answer("mono-1", relayId, yes));

  expect(onQuestionReply).toHaveBeenCalledWith("run-1", 7, yes);
  expect(store.shown).toBe(before);
  expect(store.persist).not.toHaveBeenCalled();
});

// Defect: an answer before the app binds its store throws or is lost.
it("answers before the app binds its store, without settling anything", () => {
  const relayId = askedInMono();
  const onQuestionReply: Answer = vi.fn();
  const answer = hostModule.habitQuestionReply(onQuestionReply);

  expect(() => answer("chat-1", 4, yes)).not.toThrow();
  expect(() => answer("mono-1", relayId, yes)).not.toThrow();

  expect(onQuestionReply.mock.calls).toEqual([
    ["chat-1", 4, yes],
    ["run-1", 7, yes],
  ]);
});
