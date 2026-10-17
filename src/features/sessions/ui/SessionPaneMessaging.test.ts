// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionPane, type SessionPaneProps } from "./SessionPane";
import { clearComposerDraft, setComposerDraft } from "../model/draftCache";
import type { Block, Session } from "../model/session";
import type { TurnId } from "../../monos/model/monoMessages";
import { PickTargetActions } from "./MessageTargets";
import { appendPreparingHandoff } from "../model/handoff";
import {
  isLiveHarness,
  registerHarness,
  type HarnessAdapter,
} from "../../../integrations/harness/core/registry";
import { antigravityAdapter } from "../../../integrations/harness/providers/antigravity/antigravityAdapter";
import { devinAdapter } from "../../../integrations/harness/providers/devin/devinAdapter";
import { grokAdapter } from "../../../integrations/harness/providers/grok/grokAdapter";

const probes = vi.hoisted(() => ({
  pick: vi.fn(),
  transcript: vi.fn(),
  runs: [],
}));
vi.mock("./Composer", () => ({ Composer: () => null }));
vi.mock("../hooks/useFileDrop", () => ({ useFileDrop: () => false }));
vi.mock("../model/attachments", async (original) => ({
  ...(await original<typeof import("../model/attachments")>()),
  pickAttachments: probes.pick,
}));
// The real hover actions for each user block, read from the pane's context.
vi.mock("./AgentTranscript", () => ({
  AgentTranscript: (props: {
    blocks: Block[];
    onAddToChat: (text: string) => void;
  }) => {
    probes.transcript(props);
    return createElement(
      "div",
      null,
      createElement(
        "button",
        { onClick: () => props.onAddToChat("Selected response") },
        "Quote response",
      ),
      props.blocks
        .filter((block) => block.role === "user")
        .map((block) =>
          createElement(
            "div",
            { key: block.id, "data-block": block.id },
            createElement(PickTargetActions, { block }),
          ),
        ),
    );
  },
}));
vi.mock("../../orchestration/model/orchestration", async (original) => ({
  ...(await original<
    typeof import("../../orchestration/model/orchestration")
  >()),
  orchestrator: {
    subscribe: () => () => {},
    snapshot: () => probes.runs,
    hydrate: async () => {},
  },
}));
vi.mock("../../monos/model/mono", async (original) => ({
  ...(await original<typeof import("../../monos/model/mono")>()),
  monoForSession: () => ({
    id: "mono",
    sessionId: "chat",
    name: "Captain",
    mascot: "cat",
    color: "#6ba",
    projects: [],
  }),
}));
let container: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn(() => 1),
  );
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  localStorage.clear();
  probes.transcript.mockClear();
  clearComposerDraft("chat");
  probes.pick.mockReset().mockResolvedValue([
    {
      id: "file",
      name: "note.txt",
      kind: "file",
      mimeType: "text/plain",
      size: 12,
      path: "/tmp/note.txt",
    },
  ]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  clearComposerDraft("chat");
  vi.unstubAllGlobals();
});
const noop = () => {};
function props(): SessionPaneProps {
  return {
    session: {
      id: "chat",
      title: "Chat",
      cwd: "/repo",
      harness: "codex",
      model: "",
      modelSettings: {},
      runtimeMode: "supervised",
      blocks: [{ id: "user", role: "user", text: "Hello" }],
    },
    visible: true,
    focused: true,
    inSplit: false,
    composerFocused: false,
    recents: [],
    onFocus: noop,
    onClose: noop,
    onCwdChange: noop,
    onBranchChange: noop,
    onWorkspaceModeChange: noop,
    onWorktreeBaseChange: noop,
    onModelChange: noop,
    onModelSettingsChange: noop,
    onRuntimeModeChange: noop,
    onSubmit: noop,
    onSaveDraft: noop,
    onRemoveDraft: noop,
    onStop: noop,
    onCompactContext: () => false,
    onPlaceSessionInFolder: noop,
    onDeleteQueuedMessage: noop,
    onEditQueuedMessage: noop,
    onQueuedMessageEditingChange: noop,
    onSteerQueuedMessage: noop,
    onResumeQueue: noop,
    onUsageLimitResume: noop,
    onUsageLimitResumeAtReset: noop,
    onUsageLimitDismiss: noop,
    onApproval: noop,
    onQuestionReply: noop,
    onOpenFile: noop,
    onOpenDiff: noop,
    onOpenPlan: noop,
    onBuildPlan: noop,
    onNewTerminal: noop,
  };
}
function render(pane: SessionPaneProps) {
  act(() => root.render(createElement(SessionPane, pane)));
}
function field() {
  return container.querySelector<HTMLTextAreaElement>(
    '[aria-label="Message Captain"]',
  )!;
}

it("keeps the same input, current draft and attachments when a question appears and closes", async () => {
  setComposerDraft("chat", "Initial draft");
  const pane = props();
  pane.onSubmit = vi.fn();
  render(pane);
  const input = field();
  const inputProps = Object.entries(input).find(([key]) =>
    key.startsWith("__reactProps"),
  )![1];
  act(() => inputProps.onChange({ target: { value: "Fresh draft" } }));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Attach files"]')!
      .click(),
  );
  const questioned = {
    ...pane,
    session: {
      ...pane.session,
      pendingQuestion: {
        requestId: 1,
        questions: [
          {
            id: "q",
            prompt: "Which project?",
            multiSelect: false,
            allowCustom: true,
            options: [],
          },
        ],
      },
    },
  };
  render(questioned);
  expect(container.querySelector("[data-question-form]")).not.toBeNull();
  expect(field()).toBe(input);
  expect(field().value).toBe("Fresh draft");
  expect(container.querySelector('[title="/tmp/note.txt"]')).not.toBeNull();
  render(pane);
  expect(field()).toBe(input);
  act(() =>
    container.querySelector<HTMLButtonElement>('[aria-label="Send"]')!.click(),
  );
  expect(pane.onSubmit).toHaveBeenCalledWith(
    "chat",
    "Fresh draft",
    [expect.objectContaining({ id: "file" })],
    {},
  );
});

it("routes transcript quotes to the Mono draft", () => {
  render(props());
  const quote = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent === "Quote response")!;
  act(() => quote.click());
  expect(field().value).toBe("> Selected response\n\n");
});

it("shows usage recovery above the Mono input and routes the reset option", () => {
  setComposerDraft("chat", "Keep my draft");
  const pane = props();
  pane.session.usageLimit = { resetsAt: Date.now() + 3600_000 };
  pane.onUsageLimitResumeAtReset = vi.fn();
  render(pane);
  expect(container.querySelector("[data-usage-limit]")?.textContent).toContain("usage limit reached");
  expect(container.querySelector('[aria-label="Choose another model"]')).not.toBeNull();
  expect(field().value).toBe("Keep my draft");
  const reset = [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.trim() === "Resume at reset")!;
  act(() => reset.click());
  expect(pane.onUsageLimitResumeAtReset).toHaveBeenCalledWith("chat", true);
  render({ ...pane, session: { ...pane.session, usageLimit: undefined } });
  expect(container.querySelector("[data-usage-limit]")).toBeNull();
  expect(field().value).toBe("Keep my draft");
});

it("shows pending messages in the conversation and routes retry from the bubble", () => {
  const pane = props();
  pane.onResumeQueue = vi.fn();
  pane.session = {
    ...pane.session,
    busy: true,
    queueStatus: "paused",
    queuedMessages: [
      {
        id: "first",
        text: "Follow up",
        attachments: [],
        error: "Connection lost",
      },
    ],
  };
  render(pane);
  expect(container.querySelector("[data-message-queue]")).toBeNull();
  const transcript = probes.transcript.mock.calls.at(-1)![0];
  expect(
    transcript.blocks.map((block: { text: string }) => block.text),
  ).toEqual(["Hello", "Follow up"]);
  expect(transcript.messageDeliveries.get("first")).toEqual({
    status: "failed",
    error: "Connection lost",
  });
  act(() => transcript.onRetryMessage("first"));
  expect(pane.onResumeQueue).toHaveBeenCalledWith("chat");
  expect(field()).not.toBeNull();
});

it("keeps a completion notification queued beside the Mono's composer", () => {
  const pane = props();
  pane.session = {
    ...pane.session,
    busy: true,
    queuedMessages: [
      {
        id: "notification",
        text: "Hidden completion review prompt",
        attachments: [],
        monoSessionCompletion: {
          sessionId: "worker",
          title: "API fix",
          status: "completed",
        },
      },
    ],
  };
  render(pane);
  const queue = container.querySelector("[data-message-queue]")!;
  expect(queue.textContent).toContain("Session completed: API fix");
  expect(queue.textContent).not.toContain("Hidden completion review prompt");
  expect(queue.textContent).not.toContain("Steer");
  expect(field()).not.toBeNull();
});

it("keeps ready completion notifications out of the composer while they deliver", () => {
  const pane = props();
  const notification = {
    id: "notification",
    text: "Hidden completion review prompt",
    attachments: [],
    monoSessionCompletion: {
      sessionId: "worker",
      title: "API fix",
      status: "completed" as const,
    },
  };
  pane.session = {
    ...pane.session,
    busy: false,
    queueStatus: "active",
    queuedMessages: [notification],
  };
  render(pane);
  expect(container.querySelector("[data-message-queue]")).toBeNull();
  expect(container.textContent).not.toContain(
    "Hidden completion review prompt",
  );
  expect(pane.session.queuedMessages).toEqual([notification]);
  expect(probes.transcript.mock.calls.at(-1)![0].messageDeliveries.size).toBe(
    0,
  );
  expect(field()).not.toBeNull();
});

it("keeps failed completion notifications visible and lets the user retry them", () => {
  const pane = props();
  pane.onResumeQueue = vi.fn();
  pane.session = {
    ...pane.session,
    busy: false,
    queueStatus: "paused",
    queuedMessages: [
      {
        id: "notification",
        text: "Hidden completion review prompt",
        attachments: [],
        error: "Connection lost",
        monoSessionCompletion: {
          sessionId: "worker",
          title: "API fix",
          status: "completed",
        },
      },
    ],
  };
  render(pane);
  const queue = container.querySelector("[data-message-queue]")!;
  expect(queue.textContent).toContain("Session completed: API fix");
  expect(queue.textContent).toContain("Connection lost");
  expect(queue.textContent).not.toContain("Hidden completion review prompt");
  const retry = [...queue.querySelectorAll<HTMLButtonElement>("button")].find(
    (button) => button.textContent?.trim() === "Retry",
  )!;
  act(() => retry.click());
  expect(pane.onResumeQueue).toHaveBeenCalledWith("chat");
});

it("keeps rapid sends out of the queue and steer interface", () => {
  const pane = props();
  pane.session = {
    ...pane.session,
    busy: true,
    blocks: [
      ...pane.session.blocks,
      { id: "first", role: "user", text: "Also this", sentAt: 1000 },
      { id: "second", role: "user", text: "And that", sentAt: 1100 },
    ],
    queuedMessages: [
      { id: "first", blockId: "first", text: "Also this", attachments: [] },
      { id: "second", blockId: "second", text: "And that", attachments: [] },
    ],
  };
  render(pane);
  const transcript = probes.transcript.mock.calls.at(-1)![0];
  expect(transcript.blocks).toBe(pane.session.blocks);
  expect(transcript.messageDeliveries.size).toBe(2);
  expect(container.querySelector("[data-message-queue]")).toBeNull();
  expect(
    container.querySelector('[aria-label="Edit queued message"]'),
  ).toBeNull();
  expect(container.textContent).not.toContain("Steer");
});

// A harness that can steer, as composerIntent.test registers it, and three
// real adapters that cannot.
registerHarness({ id: "codex", live: true } as unknown as HarnessAdapter);
for (const adapter of [grokAdapter, devinAdapter, antigravityAdapter])
  registerHarness(adapter);

/** The Mono chat busy in turn 2, which a steer could join. */
function busyChat(extra: Partial<Session> = {}): SessionPaneProps {
  const pane = props();
  pane.session = {
    ...pane.session,
    busy: true,
    turnReadiness: "ready",
    blocks: [
      {
        id: "opener",
        role: "user",
        text: "Build the login page",
        startedAt: 1,
        turnId: "turn-2" as TurnId,
      },
    ],
    ...extra,
  };
  return pane;
}
/** The user's own message that missed turn 1: the queue holds new text. */
const held: Partial<Session> = {
  queuedMessages: [
    {
      id: "held",
      text: "use the staging DB",
      attachments: [],
      waitForIdle: true,
      missed: true,
    },
  ],
};
function hoverAction(block: string, intent: "reply" | "steer") {
  return container.querySelector<HTMLButtonElement>(
    `[data-block="${block}"] [data-pick-intent="${intent}"]`,
  );
}
function barKind() {
  return (
    container
      .querySelector("[data-composer-intent]")
      ?.getAttribute("data-composer-intent") ?? null
  );
}
/** The intents the bar's label menu offers; it closes again after. */
function menuOptions() {
  const label = container.querySelector<HTMLButtonElement>(
    '[data-composer-intent] [aria-haspopup="menu"]',
  )!;
  act(() => label.click());
  const options = [
    ...container.querySelectorAll('[role="menuitemradio"]'),
  ].map((option) => option.getAttribute("data-intent"));
  act(() => label.click());
  return options;
}
function typeDraft(text: string) {
  const inputProps = Object.entries(field()).find(([key]) =>
    key.startsWith("__reactProps"),
  )![1];
  act(() => inputProps.onChange({ target: { value: text } }));
}

// PR I6 review r2 M2 and m5. Defect: while the queue holds the user's text,
// the label menu still offers Steering and a picked Steer still reads as
// one, though the pump holds that steer and fails it when its turn ends; or
// the hover set is not the one the pane passes; or a message that waits by
// choice (✕), which holds nothing, hides Steer.
it("offers Steer, in the hover and the label menu, only while a steer sent now reaches the turn", () => {
  render(
    busyChat({
      queuedMessages: [
        {
          id: "later",
          text: "after this, write docs",
          attachments: [],
          waitForIdle: true,
        },
      ],
    }),
  );
  expect(hoverAction("opener", "steer")).not.toBeNull();
  render(busyChat());
  expect(hoverAction("opener", "steer")).not.toBeNull();
  act(() => hoverAction("opener", "reply")!.click());
  expect(menuOptions()).toEqual(["steer", "reply", "continue"]);
  act(() => hoverAction("opener", "steer")!.click());
  expect(barKind()).toBe("picked");

  render(busyChat(held));
  expect(hoverAction("opener", "steer")).toBeNull();
  expect(barKind()).toBe("gone");
  // PR I6 review r3 n2: the turn still runs, so nothing says it finished.
  expect(container.textContent).toContain("Can't steer now");
  expect(container.textContent).not.toContain("Not running");
  act(() => hoverAction("opener", "reply")!.click());
  expect(barKind()).toBe("picked");
  expect(menuOptions()).toEqual(["reply", "continue"]);
});

// PR I6 review r2 M3, design 16.8 (`canSteerHarness`). Defect: a session
// whose harness cannot steer offers Steer, which App refuses mid-turn or the
// Mono pump never sends.
it.each(["grok", "devin", "antigravity"] as const)(
  "offers no Steer on %s, whose harness cannot steer",
  (harness) => {
    expect(isLiveHarness(harness)).toBe(true);
    render(busyChat({ harness }));
    typeDraft("stop, wrong branch");
    expect(barKind()).toBeNull();
    expect(hoverAction("opener", "steer")).toBeNull();
    act(() => hoverAction("opener", "reply")!.click());
    expect(menuOptions()).toEqual(["reply", "continue"]);
  },
);

// PR I6 review r2 m2. Defect: a Mono chat whose queue is paused says
// Steering, though the pump holds every steer until the queue resumes. PR I6
// review r3 m1: or its text goes out as a plain message, which the pump
// steers if the user resumes the queue before the turn ends.
it("shows no Steering and no hover Steer while a Mono chat's queue is paused, and sends the text to wait for the turn end", () => {
  const pane = busyChat({ queueStatus: "paused" });
  pane.onSubmit = vi.fn(() => true);
  render(pane);
  typeDraft("stop, wrong branch");
  expect(barKind()).toBeNull();
  expect(hoverAction("opener", "steer")).toBeNull();
  act(() =>
    container
      .querySelector("form")!
      .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
  );
  expect(pane.onSubmit).toHaveBeenCalledExactlyOnceWith(
    "chat",
    "stop, wrong branch",
    [],
    { waitForIdle: true },
  );
});

// PR I6 review r3 M1. Defect: the Mono chat says Steering and offers Steer
// while a provider switch is armed (from the usage-limit notice) or the
// outgoing agent writes its hand-off brief, though App then refuses the text
// or cancels the turn, and the pump steers nothing.
it("shows no Steering and no hover Steer while the chat switches providers or prepares a hand-off", () => {
  const switching = busyChat({
    pendingSwitch: { from: "claude", fromModel: "sonnet", fromSettings: {} },
  });
  const handingOff = busyChat();
  handingOff.session = appendPreparingHandoff(
    handingOff.session,
    "claude",
    "codex",
  );
  for (const pane of [switching, handingOff]) {
    render(pane);
    typeDraft("stop, wrong branch");
    expect(barKind()).toBeNull();
    expect(hoverAction("opener", "steer")).toBeNull();
    expect(hoverAction("opener", "reply")).not.toBeNull();
  }
});

// PR I6 review r4 M1. Defect: while the chat's worktree is prepared for its
// first turn, the hover offers Steer and a picked steer reads as live,
// though the provider has no turn to take it.
it("offers no Steer, and a picked steer cannot steer now, while the worktree is prepared", () => {
  render(busyChat());
  act(() => hoverAction("opener", "steer")!.click());
  expect(barKind()).toBe("picked");
  render(busyChat({ turnReadiness: "starting", worktreePreparing: true }));
  expect(hoverAction("opener", "steer")).toBeNull();
  expect(barKind()).toBe("gone");
  expect(container.textContent).toContain("Can't steer now");
});

// PR I6 review r4 n2 (r3 m1 names Send as new). Defect: Send as new on a
// steer the paused queue holds goes out as a plain message, which the pump
// steers into the running turn if the user resumes the queue first.
it("sends a held steer as new to wait for the turn end while the queue is paused", () => {
  const onSubmit = vi.fn(() => true);
  render({ ...busyChat(), onSubmit });
  act(() => hoverAction("opener", "steer")!.click());
  render({ ...busyChat({ queueStatus: "paused" }), onSubmit });
  typeDraft("stop, wrong branch");
  expect(barKind()).toBe("gone");
  act(() =>
    [...container.querySelectorAll("button")]
      .find((button) => button.textContent === "Send as new")!
      .click(),
  );
  expect(onSubmit).toHaveBeenCalledExactlyOnceWith(
    "chat",
    "stop, wrong branch",
    [],
    { waitForIdle: true },
  );
});

// Decision 658, PR I6 review r4 n2. Defect: text the queue holds behind the
// user's missed message is sent as a chosen wait, which holds nothing behind
// it, so once it misses its turn the user's later text can pass it.
it("sends text held behind a missed message as a plain message while the queue is paused", () => {
  const pane = busyChat({ ...held, queueStatus: "paused" });
  pane.onSubmit = vi.fn(() => true);
  render(pane);
  typeDraft("and seed it");
  act(() =>
    container
      .querySelector("form")!
      .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
  );
  expect(pane.onSubmit).toHaveBeenCalledExactlyOnceWith(
    "chat",
    "and seed it",
    [],
    {},
  );
});

// PR I6 review r2 m5, design 16.8. Defect: the pane reads a Mono chat as a
// project session, so with the follow-up setting `queue` the bar is silent
// while the Mono pump steers the text.
it("says Steering in a Mono chat whatever the follow-up setting", () => {
  localStorage.setItem("monocode.followUpBehavior", "queue");
  render(busyChat());
  typeDraft("stop, wrong branch");
  expect(barKind()).toBe("turn");
  expect(container.textContent).toContain("everything Captain is doing");
});

// PR I6 review r2 m1. Defect: when the queue starts holding the text being
// typed, the bar says "Nothing running" while turn 2 runs.
it("says Nothing running only once the chat is idle", () => {
  render(busyChat());
  typeDraft("stop, wrong branch");
  expect(barKind()).toBe("turn");
  render(busyChat(held));
  expect(barKind()).toBeNull();
  render(busyChat({ busy: false, turnReadiness: undefined }));
  expect(barKind()).toBe("ended");
  expect(container.textContent).toContain("Nothing running");
});
