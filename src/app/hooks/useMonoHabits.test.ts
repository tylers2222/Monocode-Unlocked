// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Habit } from "../../features/monos/model/monoHabits";
import type { Block, Session } from "../../features/sessions/model/session";
import type { UserQuestionPrompt } from "../../features/sessions/model/userQuestion";

let stored: Habit[] = [];
let fileVersion = 0;
const MONO = {
  id: "mono-1",
  sessionId: "mono",
  mascot: "skull",
  color: "#fff",
  projects: ["/code/app"],
};

vi.mock("../../features/monos/model/mono", async (original) => ({
  ...(await original<object>()),
  listMonos: () => [MONO],
  findMono: (id: string) => (id === MONO.id ? MONO : undefined),
  monoLook: () => ({
    name: "Skull",
    mascot: "skull",
    color: "#fff",
    projects: [{ path: "/code/app", name: "app" }],
  }),
}));
vi.mock("../../features/monos/model/monoFiles", async (original) => {
  const actual =
    await original<typeof import("../../features/monos/model/monoFiles")>();
  return {
    ...actual,
    loadMonoFiles: vi.fn(async () => ({
      id: "a",
      dir: "/data/monos/a",
      soul: "Be brief.",
      soulHash: "s",
      memory: "",
      memoryHash: "m",
      memoryPath: "/data/monos/a/MEMORY.md",
      topics: [],
    })),
    readAgentFile: vi.fn(async () => ({
      text: JSON.stringify(stored),
      hash: String(fileVersion),
    })),
    // Model the native store's atomic compare-and-save, including conflicts
    // between independent window schedulers.
    writeAgentFile: vi.fn(async (_monoId, _path, text, expectedHash) => {
      if (expectedHash !== String(fileVersion))
        throw new actual.MonoFileConflict();
      stored = JSON.parse(text) as Habit[];
      return String(++fileVersion);
    }),
  };
});

const { useMonoHabits } = await import("./useMonoHabits");
const { shouldPersistSession } =
  await import("../../features/sessions/data/sessionStore");
const {
  checkHabitsNow,
  isHabitRun,
  loadHabits,
  noteHabitRunLaunch,
  relayApproval,
} = await import("../../features/monos/model/monoHabits");
const { loadMonoFiles } = await import("../../features/monos/model/monoFiles");

let root: Root;
let extraRoots: Root[] = [];
const originalWindow = window;
beforeEach(() => {
  fileVersion = 0;
  extraRoots = [];
  vi.clearAllMocks();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  root = createRoot(document.createElement("div"));
});
afterEach(() => {
  vi.stubGlobal("window", originalWindow);
  act(() => root.unmount());
  for (const extra of extraRoots) act(() => extra.unmount());
  vi.unstubAllGlobals();
});

function setup(
  reply: string,
  approve?: { wait: Promise<void> },
  waitingHabit = false,
) {
  stored = [
    {
      id: "h1",
      name: "Morning CI check",
      instructions: "Tell me if CI is red.",
      schedule: {
        scheduleKind: "daily",
        time: "09:00",
        minute: 0,
        dayOfWeek: 1,
      },
      enabled: true,
      createdAt: 0,
      nextRunAt: Date.now() - 60_000,
    },
  ];
  if (waitingHabit)
    stored.push({ ...stored[0], id: "h2", name: "Waiting habit" });
  const host = makeHost(reply, approve);
  mountHost(host);
  return host;
}

function makeHost(reply: string, approve?: { wait: Promise<void> }) {
  // The Mono's chat lives in the home folder, apart from its projects.
  const mono = {
    id: "mono",
    cwd: "/Users/me",
    harness: "claude",
    model: "claude:x",
    blocks: [],
  } as unknown as Session;
  const sessions: Session[] = [mono];
  const host = {
    sessions: () => sessions,
    open: vi.fn(async () => mono),
    add: vi.fn((session: Session) => sessions.push(session)),
    run: vi.fn(async (id: string, prompt: string) => {
      const run = sessions.find((session) => session.id === id)!;
      run.providerSessionId = "claude-transcript-1";
      if (approve) {
        run.blocks = [
          { id: "u", role: "user", text: prompt },
          {
            id: "ask",
            role: "approval",
            text: "",
            tool: { title: "git push" },
            approval: { requestId: 7 },
          },
        ];
        await approve.wait;
        run.blocks[1] = {
          ...run.blocks[1],
          approval: { requestId: 7, decided: "allow" },
        };
      }
      run.blocks = [
        ...run.blocks,
        { id: "u", role: "user", text: prompt },
        {
          id: "t",
          role: "assistant",
          text: "",
          tool: { title: "gh run list" },
        },
        { id: "a", role: "assistant", text: reply },
      ];
      return { status: "completed" as const, text: reply };
    }),
    stop: vi.fn(async () => {}),
    post: vi.fn(),
    // Like the app: the request is relayed, so later polls skip it.
    askApproval: vi.fn(
      (monoId: string, _habit: Habit, runId: string, block: Block) =>
        relayApproval({
          monoId,
          runId,
          requestId: block.approval!.requestId,
          blockId: "b",
        }),
    ),
    endApprovals: vi.fn(),
    askQuestion: vi.fn(
      (
        monoId: string,
        _habit: Habit,
        runId: string,
        _runAt: number,
        prompt: UserQuestionPrompt,
      ) =>
        relayApproval({
          monoId,
          runId,
          requestId: prompt.requestId,
          blockId: "q",
        }),
    ),
    endQuestions: vi.fn(),
  };
  return host;
}

function mountHost(
  host: ReturnType<typeof makeHost>,
  target = root,
  surface = originalWindow,
) {
  function Probe() {
    useMonoHabits(host);
    return null;
  }
  vi.stubGlobal("window", surface);
  act(() => target.render(createElement(Probe)));
  vi.stubGlobal("window", originalWindow);
}

/** Separate native windows have separate owner slots and share the file store. */
function isolatedWindow(): Window {
  let owner: unknown;
  return new Proxy(originalWindow, {
    get(target, key) {
      if (key === "__monoHabitsScheduler") return owner;
      const value = Reflect.get(target, key);
      return typeof value === "function" ? value.bind(target) : value;
    },
    set(target, key, value) {
      if (key === "__monoHabitsScheduler") {
        owner = value;
        return true;
      }
      return Reflect.set(target, key, value);
    },
    deleteProperty(target, key) {
      if (key === "__monoHabitsScheduler") {
        owner = undefined;
        return true;
      }
      return Reflect.deleteProperty(target, key);
    },
  });
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => (resolve = done));
  return { promise, resolve };
}

it("runs a due habit in a kept session and posts what it found", async () => {
  const host = setup("CI is red on main since #712.");
  await vi.waitFor(() => expect(host.post).toHaveBeenCalled());
  const [monoId, habit, text, name] = host.post.mock.calls[0];
  expect([monoId, habit.id, text, name]).toEqual([
    "mono",
    "h1",
    "CI is red on main since #712.",
    "Skull",
  ]);
  const prompt = host.run.mock.calls[0][1] as string;
  expect(prompt.startsWith("/operator ")).toBe(true);
  expect(prompt).toContain("Habit: Morning CI check");
  const run = host.add.mock.calls[0][0];
  // Nobody watches a habit's run, so it never stops to ask before each step.
  expect(run.runtimeMode).toBe("full-access");
  // Saved like any session the Mono starts, and out of the sidebar like them.
  expect(run.ephemeral).toBeUndefined();
  expect(run.sidebarHidden).toBe(true);
  expect(
    shouldPersistSession(host.sessions().find((s) => s.id === run.id)!),
  ).toBe(true);
  // Found by tools that list the sessions of the Mono's chat.
  expect(run.id.startsWith("app-mono-habit-")).toBe(true);
  expect(host.endApprovals).toHaveBeenCalledWith(run.id);
  expect(host.endQuestions).toHaveBeenCalledWith(run.id, { kind: "stopped" });
  await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("posted"));
  expect(stored[0].runs?.[0]).toMatchObject({
    outcome: "posted",
    report: "CI is red on main since #712.",
  });
  // A finished run is kept, not stopped or deleted.
  expect(host.stop).not.toHaveBeenCalled();
  expect(host.sessions().some((s) => s.id === run.id)).toBe(true);
  expect(isHabitRun(run.id)).toBe(false);
  expect(stored[0].nextRunAt).toBeGreaterThan(Date.now());
});

it("starts a habit run in the Mono's first project, not its chat's folder", async () => {
  const host = setup("NOTHING_TO_REPORT");
  await vi.waitFor(() => expect(host.add).toHaveBeenCalled());
  expect(host.add.mock.calls[0][0].cwd).toBe("/code/app");
});

it("records a finished run's session so its transcript can be found", async () => {
  const started = Date.now();
  const host = setup("NOTHING_TO_REPORT");
  await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("quiet"));
  const run = host.add.mock.calls[0][0];
  // Read back through habits.json as the app and audit tools see it.
  const [habit] = await loadHabits(MONO.id);
  const record = habit.runs![0];
  expect(record).toMatchObject({
    outcome: "quiet",
    session: {
      id: run.id,
      harness: "claude",
      model: run.model,
      providerSessionId: "claude-transcript-1",
      cwd: "/code/app",
    },
  });
  expect(record.at).toBeGreaterThanOrEqual(started);
  expect(record.endedAt).toBeGreaterThanOrEqual(record.at);
  expect(record.durationMs).toBe(record.endedAt! - record.at);
});

it("posts nothing when the run has nothing to say", async () => {
  const host = setup("NOTHING_TO_REPORT");
  await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("quiet"));
  expect(host.post).not.toHaveBeenCalled();
  expect(host.stop).not.toHaveBeenCalled();
});

it("puts the sessions a run started on its report", async () => {
  const host = setup("Started #910 in a background session.");
  const run = host.run.getMockImplementation()!;
  const launch = {
    sessionId: "app-910",
    cwd: "/code/app",
    title: "Fix #910",
    harness: "claude" as const,
    model: "claude:x",
  };
  host.run.mockImplementation(async (id: string, prompt: string) => {
    noteHabitRunLaunch(id, launch);
    // A retried request must not list it twice.
    noteHabitRunLaunch(id, launch);
    return run(id, prompt);
  });
  await vi.waitFor(() => expect(host.post).toHaveBeenCalled());
  expect(host.post.mock.calls[0][2]).toBe(
    "Started #910 in a background session.",
  );
  expect(host.post.mock.calls[0][5]).toEqual([launch]);
});

it("posts the sessions a quiet run started", async () => {
  const host = setup("NOTHING_TO_REPORT");
  const run = host.run.getMockImplementation()!;
  host.run.mockImplementation(async (id: string, prompt: string) => {
    noteHabitRunLaunch(id, {
      sessionId: "app-1",
      cwd: "/code/app",
      title: "Look into CI",
      harness: "claude",
      model: "claude:x",
    });
    return run(id, prompt);
  });
  await vi.waitFor(() => expect(host.post).toHaveBeenCalled());
  expect(host.post.mock.calls[0][2]).toBe("Started a session.");
  expect(host.post.mock.calls[0][5]).toHaveLength(1);
});

it("lets a long-running habit finish and post after thirty minutes", async () => {
  vi.useFakeTimers();
  const hold = deferred();
  const host = setup("PR review complete.");
  const run = host.run.getMockImplementation()!;
  host.run.mockImplementationOnce(async (id, prompt) => {
    await hold.promise;
    return run(id, prompt);
  });
  try {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30 * 60 * 1000);
    });
    expect(host.run).toHaveBeenCalledTimes(1);
    expect(host.stop).not.toHaveBeenCalled();
    expect(stored[0].lastOutcome).toBeUndefined();

    await act(async () => hold.resolve());
    expect(host.post).toHaveBeenCalledWith(
      "mono",
      expect.objectContaining({ id: "h1" }),
      "PR review complete.",
      "Skull",
      [],
      [],
    );
    expect(stored[0].lastOutcome).toBe("posted");
    expect(host.stop).not.toHaveBeenCalled();
  } finally {
    await act(async () => hold.resolve());
    vi.useRealTimers();
  }
});

it("stops and records a habit that is still running after one hour", async () => {
  vi.useFakeTimers();
  const hold = deferred();
  const host = setup("PR review complete.");
  host.run.mockImplementationOnce(async (id) => {
    host.sessions().find((s) => s.id === id)!.busy = true;
    await hold.promise;
    return { status: "completed", text: "PR review complete." };
  });
  try {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60 * 60 * 1000 - 2_000);
    });
    expect(host.run).toHaveBeenCalledTimes(1);
    expect(host.stop).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    // Stopped, but kept so what it did can still be read.
    expect(host.stop).toHaveBeenCalledTimes(1);
    expect(host.sessions()).toHaveLength(2);
    expect(host.post).not.toHaveBeenCalled();
    expect(stored[0]).toMatchObject({
      lastOutcome: "failed",
      lastError: "It took too long and was stopped.",
    });
    expect(host.endQuestions).toHaveBeenCalledWith(
      host.add.mock.calls[0][0].id,
      { kind: "stopped" },
    );
  } finally {
    await act(async () => hold.resolve());
    vi.useRealTimers();
  }
});

const RESET: UserQuestionPrompt = {
  requestId: 4,
  title: "Rate limit",
  questions: [
    {
      id: "reset",
      prompt: "Spend a saved reset?",
      multiSelect: false,
      allowCustom: false,
      options: [
        { id: "yes", label: "Yes" },
        { id: "no", label: "No" },
      ],
    },
  ],
};

// Defect: the question is never put to the user, is put before the run asks
// it, is put again on every check, or a run nobody answered for twelve hours
// ends its card as stopped instead of unanswered.
it(
  "puts the run's question to the user once and ends it unanswered after twelve hours",
  { timeout: 30_000 },
  async () => {
    vi.useFakeTimers();
    const hold = deferred();
    const startedAt = Date.now();
    const host = setup("Asked about the reset.");
    host.run.mockImplementationOnce(async (id) => {
      const run = host.sessions().find((s) => s.id === id)!;
      run.busy = true;
      // Asked after the first check, which sees no question yet.
      setTimeout(() => (run.pendingQuestion = RESET), 3_000);
      await hold.promise;
      return { status: "completed", text: "Asked about the reset." };
    });
    try {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(2_500);
      });
      expect(host.run).toHaveBeenCalledTimes(1);
      expect(host.askQuestion).not.toHaveBeenCalled();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(2_000);
      });
      const runId = host.add.mock.calls[0][0].id;
      expect(host.askQuestion).toHaveBeenCalledTimes(1);
      const [monoId, habit, askedRun, runAt, prompt] =
        host.askQuestion.mock.calls[0];
      expect([monoId, habit.id, askedRun, prompt]).toEqual([
        "mono",
        "h1",
        runId,
        RESET,
      ]);
      expect(runAt).toBe(startedAt);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(12 * 60 * 60 * 1000);
      });
      expect(host.askQuestion).toHaveBeenCalledTimes(1);
      expect(host.endQuestions).toHaveBeenCalledTimes(1);
      expect(host.endQuestions).toHaveBeenCalledWith(runId, {
        kind: "timed-out",
      });
      // Its relays are still there to settle: endApprovals drops them.
      expect(host.endQuestions.mock.invocationCallOrder[0]).toBeLessThan(
        host.endApprovals.mock.invocationCallOrder[0],
      );
      expect(host.stop).toHaveBeenCalledTimes(1);
      expect(stored[0]).toMatchObject({
        lastOutcome: "failed",
        lastError: "Nobody answered it.",
      });
    } finally {
      await act(async () => hold.resolve());
      vi.useRealTimers();
    }
  },
);

// Defect: a check that finds no run session (its chat was closed) throws
// while looking for its question, so the run is recorded as failed.
it("keeps watching a run whose session has left the list", async () => {
  vi.useFakeTimers();
  const host = setup("Checked the garden.");
  host.run.mockImplementationOnce(async (id) => {
    const sessions = host.sessions();
    sessions.splice(
      sessions.findIndex((s) => s.id === id),
      1,
    );
    await new Promise((resolve) => setTimeout(resolve, 5_000));
    return { status: "completed", text: "Checked the garden." };
  });
  try {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(host.askQuestion).not.toHaveBeenCalled();
    expect(stored[0].lastOutcome).toBe("posted");
  } finally {
    vi.useRealTimers();
  }
});

it("puts an approval the run still asks for to the user, once", async () => {
  vi.useFakeTimers();
  const hold = deferred();
  const host = setup("Pushed the fix.", { wait: hold.promise });
  // The clock moves by `ms` while the run checks on itself once, as when the
  // Mac sleeps that long.
  const jump = (ms: number) =>
    act(async () => {
      vi.setSystemTime(Date.now() + ms - 2_000);
      await vi.advanceTimersByTimeAsync(2_000);
    });
  try {
    // The first check relays the approval; the 12 h wait starts here.
    await jump(2_000);
    expect(host.askApproval).toHaveBeenCalledTimes(1);
    const [monoId, habit, runId, block] = host.askApproval.mock.calls[0];
    expect([monoId, habit.id, block.approval.requestId]).toEqual([
      "mono",
      "h1",
      7,
    ]);
    expect(runId).toBe(host.add.mock.calls[0][0].id);

    await jump(12 * 60 * 60 * 1000 - 2_000);
    // Still waiting on the user 2 s before 12 h: not asked again.
    expect(host.askApproval).toHaveBeenCalledTimes(1);
    expect(host.endApprovals).not.toHaveBeenCalled();
    expect(stored[0].lastOutcome).toBeUndefined();

    await jump(2_000);
    expect(host.endApprovals).toHaveBeenCalledWith(runId);
    expect(stored[0]).toMatchObject({
      lastOutcome: "failed",
      lastError: "Nobody answered it.",
    });

    await jump(2_000);
    expect(host.askApproval).toHaveBeenCalledTimes(1);
    expect(host.endApprovals).toHaveBeenCalledTimes(1);
    expect(host.post).not.toHaveBeenCalled();
  } finally {
    await act(async () => hold.resolve());
    vi.useRealTimers();
  }
});

it("starts a habit asked to run now without waiting for the next minute", async () => {
  const host = setup("CI is red.");
  await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("posted"));
  const scheduled = stored[0].nextRunAt;
  stored = [{ ...stored[0], runRequested: true }];
  checkHabitsNow();
  await vi.waitFor(() => expect(host.run).toHaveBeenCalledTimes(2));
  await vi.waitFor(() => expect(stored[0].runs).toHaveLength(2));
  expect(stored[0].runRequested).toBeUndefined();
  expect(stored[0].nextRunAt).toBe(scheduled);
});

it("launches a due habit only once across independent window schedulers", async () => {
  const first = setup("NOTHING_TO_REPORT");
  const second = makeHost("NOTHING_TO_REPORT");
  const extra = createRoot(document.createElement("div"));
  extraRoots.push(extra);
  mountHost(second, extra, isolatedWindow());
  await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("quiet"));
  await act(async () => {});
  expect(first.run.mock.calls.length + second.run.mock.calls.length).toBe(1);
  expect(stored[0].runs).toHaveLength(1);
});

it.each(["removed", "paused", "rescheduled"] as const)(
  "does not launch a waiting habit that was %s during an earlier run",
  async (change) => {
    const hold = deferred();
    const host = setup("NOTHING_TO_REPORT", undefined, true);
    host.run.mockImplementationOnce(async () => {
      await hold.promise;
      return { status: "completed", text: "NOTHING_TO_REPORT" };
    });
    try {
      await vi.waitFor(() => expect(host.run).toHaveBeenCalledTimes(1));
      stored = stored.flatMap((habit) => {
        if (habit.id !== "h2") return [habit];
        if (change === "removed") return [];
        return [
          {
            ...habit,
            ...(change === "paused"
              ? { enabled: false }
              : { nextRunAt: Date.now() + 60_000 }),
          },
        ];
      });
      fileVersion++;
    } finally {
      hold.resolve();
    }
    await vi.waitFor(() => expect(stored[0].lastOutcome).toBe("quiet"));
    await act(async () => {});
    expect(host.run).toHaveBeenCalledTimes(1);
  },
);

it("rechecks a habit removed while its files were being prepared", async () => {
  const hold = deferred();
  const load = vi.mocked(loadMonoFiles);
  const files = await load(MONO.id);
  load.mockImplementationOnce(async () => {
    await hold.promise;
    return files;
  });
  const host = setup("NOTHING_TO_REPORT");
  try {
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    stored = [];
    fileVersion++;
  } finally {
    hold.resolve();
  }
  await act(async () => {});
  expect(host.run).not.toHaveBeenCalled();
  expect(host.add).not.toHaveBeenCalled();
});

it("uses a waiting habit's latest instructions when it launches", async () => {
  const hold = deferred();
  const host = setup("NOTHING_TO_REPORT", undefined, true);
  host.run.mockImplementationOnce(async () => {
    await hold.promise;
    return { status: "completed", text: "NOTHING_TO_REPORT" };
  });
  try {
    await vi.waitFor(() => expect(host.run).toHaveBeenCalledTimes(1));
    stored = stored.map((habit) =>
      habit.id === "h2"
        ? { ...habit, instructions: "Inspect only the release branch." }
        : habit,
    );
    fileVersion++;
  } finally {
    hold.resolve();
  }
  await vi.waitFor(() => expect(host.run).toHaveBeenCalledTimes(2));
  expect(host.run.mock.calls[1][1]).toContain(
    "Inspect only the release branch.",
  );
  await vi.waitFor(() => expect(stored[1].lastOutcome).toBe("quiet"));
});
