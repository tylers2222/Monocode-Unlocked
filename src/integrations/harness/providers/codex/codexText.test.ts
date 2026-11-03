import { afterEach, beforeEach, expect, it, vi } from "vitest";

type Request = {
  id?: number;
  method: string;
  params?: Record<string, unknown>;
};

const sent: Request[] = [];
let onLine: ((line: string) => void) | undefined;
let output = "Generated text";
let threadCount = 0;
let failTurn = false;
let missingResume = false;
// Rust places a helper and names the folder (ADE-189); it is never "/repo".
const spawnHelperChild = vi.hoisted(() =>
  vi.fn(async (..._args: unknown[]) => "/placed"),
);
const prepareCodexMonoContext = vi.hoisted(() =>
  vi.fn(async (_input: unknown) => ({
    config: { sqlite_home: "/private/mono" },
    hasThread: true,
  })),
);
vi.mock("./codexStore", () => ({ prepareCodexMonoContext }));

function receive(message: unknown) {
  onLine?.(JSON.stringify(message));
}

vi.mock("../../core/child", () => ({
  resolveCodexBinary: async () => ({ path: "/fake/codex" }),
  spawnHelperChild,
  restoreMonoCodexAgentState: async () => undefined,
  killChild: async () => undefined,
  unwatchChild: () => undefined,
  watchChild: (_id: string, line: (value: string) => void) => {
    onLine = line;
  },
  writeChild: async (_id: string, line: string) => {
    const request = JSON.parse(line) as Request;
    sent.push(request);
    if (request.id === undefined) return;
    if (request.method === "thread/start") {
      receive({
        id: request.id,
        result: { thread: { id: `thread_${++threadCount}` } },
      });
    } else if (request.method === "thread/resume") {
      receive(
        missingResume
          ? { id: request.id, error: { message: "thread not found" } }
          : {
              id: request.id,
              result: { thread: { id: request.params?.threadId } },
            },
      );
    } else if (request.method === "turn/start") {
      receive({ id: request.id, result: { turn: { id: "turn_1" } } });
      setTimeout(() => {
        receive({
          method: "item/agentMessage/delta",
          params: { delta: output },
        });
        receive({
          method: "turn/completed",
          params: {
            turn: {
              id: "turn_1",
              status: failTurn ? "failed" : "completed",
              ...(failTurn ? { error: { message: "Generation failed" } } : {}),
            },
          },
        });
      }, 0);
    } else {
      receive({ id: request.id, result: {} });
    }
  },
}));

vi.mock("../../../../platform/tauri/fs", () => ({
  gitStagedContext: async () => ({
    branch: "helpers",
    summary: "1 file changed",
    patch: "diff",
  }),
  gitRangeContext: async () => ({
    base: "main",
    head: "helpers",
    commitSummary: "Keep helper history clean",
    diffSummary: "1 file changed",
    diffPatch: "diff",
  }),
}));

const { runCodexTextPrompt, stopCodexTextPrompt, warmupCodexText } =
  await import("./codexText");
const { generateCodexSessionTitle } = await import("./codexTitle");
const {
  generateCodexCommitMessage,
  generateCodexBranchName,
  generateCodexPrContent,
} = await import("./codexGit");

function threadStarts() {
  return sent.filter((request) => request.method === "thread/start");
}

beforeEach(() => {
  sent.length = 0;
  onLine = undefined;
  output = "Generated text";
  threadCount = 0;
  failTurn = false;
  missingResume = false;
  spawnHelperChild.mockClear();
  prepareCodexMonoContext.mockClear();
});

afterEach(async () => {
  await stopCodexTextPrompt();
});

it.each([
  {
    name: "session titles",
    response: { title: "Keep Helper History Clean", workItem: null },
    run: () =>
      generateCodexSessionTitle({
        sessionId: "session",
        cwd: "/repo",
        message: "Keep helper history clean",
      }),
    expected: { title: "Keep Helper History Clean", workItem: null },
  },
  {
    name: "commit messages",
    response: { subject: "Keep helper history clean", body: "" },
    run: () => generateCodexCommitMessage("/repo"),
    expected: "Keep helper history clean",
  },
  {
    name: "branch names",
    response: { branch: "keep-helper-history-clean" },
    run: () => generateCodexBranchName("/repo", "Keep helper history clean"),
    expected: "keep-helper-history-clean",
  },
  {
    name: "PR content",
    response: {
      title: "Keep helper history clean",
      body: "## Summary\n- Use unsaved threads",
    },
    run: () => generateCodexPrContent("/repo"),
    expected: {
      title: "Keep helper history clean",
      body: "## Summary\n- Use unsaved threads",
      base: "main",
      head: "helpers",
    },
  },
])(
  "generates $name without saving a Codex conversation",
  async ({ response, run, expected }) => {
    output = JSON.stringify(response);
    await expect(run()).resolves.toEqual(expected);
    expect(threadStarts()).toHaveLength(1);
    expect(threadStarts()[0].params).toMatchObject({
      ephemeral: true,
      cwd: "/placed",
    });
    expect(sent.some((request) => request.method === "thread/resume")).toBe(
      false,
    );
  },
);

it("keeps warmup and subsequent prompts unsaved across model changes", async () => {
  await warmupCodexText("/repo");
  await expect(
    runCodexTextPrompt({
      cwd: "/repo",
      model: "test-model",
      prompt: "Generate text",
    }),
  ).resolves.toBe(output);
  // The warm child of the same folder is reused for another model.
  expect(spawnHelperChild).toHaveBeenCalledTimes(1);
  await expect(
    runCodexTextPrompt({ cwd: "/repo", prompt: "Generate more text" }),
  ).resolves.toBe(output);
  expect(threadStarts()).toHaveLength(3);
  expect(
    threadStarts().every((request) => request.params?.ephemeral === true),
  ).toBe(true);

  // ADE-189: a warm child is never told another folder; a prompt for one
  // starts its own child there.
  await warmupCodexText("/repo");
  spawnHelperChild.mockResolvedValueOnce("/placed-other");
  await runCodexTextPrompt({ cwd: "/other", prompt: "Elsewhere" });
  expect(spawnHelperChild).toHaveBeenCalledTimes(4);
  expect(spawnHelperChild.mock.calls[3]?.[4]).toBe("/other");
  expect(threadStarts().at(-1)?.params).toMatchObject({ cwd: "/placed-other" });

  // A warm child started for no session never serves a session's prompt.
  await warmupCodexText("/repo");
  const spawns = spawnHelperChild.mock.calls.length;
  await runCodexTextPrompt({ cwd: "/repo", owner: "chat-x", prompt: "Owned" });
  expect(spawnHelperChild).toHaveBeenCalledTimes(spawns + 1);
  expect(spawnHelperChild.mock.calls[spawns]?.[0]).toBe("chat-x");
});

it("does not resume a stored thread for an unsaved text request", async () => {
  await runCodexTextPrompt({
    cwd: "/repo",
    threadId: "stored_thread",
    prompt: "Generate text",
  });
  expect(sent.some((request) => request.method === "thread/resume")).toBe(
    false,
  );
  expect(threadStarts()[0].params).toMatchObject({ ephemeral: true });
});

it("allows side questions to explicitly retain and resume their thread", async () => {
  const onThreadId = vi.fn();
  await warmupCodexText("/repo");
  await runCodexTextPrompt({
    cwd: "/repo",
    ephemeral: false,
    onThreadId,
    prompt: "Side question",
  });
  expect(threadStarts()).toHaveLength(2);
  expect(threadStarts()[0].params).toMatchObject({ ephemeral: true });
  expect(threadStarts()[1].params).not.toHaveProperty("ephemeral");
  expect(onThreadId).toHaveBeenCalledWith("thread_2");

  await runCodexTextPrompt({
    cwd: "/repo",
    ephemeral: false,
    threadId: "thread_2",
    prompt: "Follow-up",
  });
  expect(
    sent.find((request) => request.method === "thread/resume")?.params,
  ).toMatchObject({ threadId: "thread_2" });
  expect(threadStarts()).toHaveLength(2);
});

it("retains resumable behavior when a side question's saved thread is missing", async () => {
  missingResume = true;
  await runCodexTextPrompt({
    cwd: "/repo",
    ephemeral: false,
    threadId: "missing_thread",
    prompt: "Side question",
  });
  expect(threadStarts()).toHaveLength(1);
  expect(threadStarts()[0].params).not.toHaveProperty("ephemeral");
});

it("keeps a failed generation and its retry unsaved", async () => {
  failTurn = true;
  await expect(
    runCodexTextPrompt({ cwd: "/repo", prompt: "Generate text" }),
  ).rejects.toThrow("Generation failed");
  failTurn = false;
  await expect(
    runCodexTextPrompt({ cwd: "/repo", prompt: "Retry" }),
  ).resolves.toBe(output);
  expect(threadStarts()).toHaveLength(2);
  expect(
    threadStarts().every((request) => request.params?.ephemeral === true),
  ).toBe(true);
});

it("isolates Mono side questions from regular Codex threads and resumes them after restart", async () => {
  await runCodexTextPrompt({
    cwd: "/repo",
    ephemeral: false,
    prompt: "Regular side question",
  });
  expect(spawnHelperChild.mock.calls[0][7]).toBeUndefined();
  expect(prepareCodexMonoContext).not.toHaveBeenCalled();
  await runCodexTextPrompt({
    cwd: "/repo",
    owner: "chat-x",
    ephemeral: false,
    codexStore: "mono",
    prompt: "Mono side question",
  });
  expect(spawnHelperChild.mock.calls[1][7]).toBe("mono");
  // ADE-189: the store child is placed for the prompt's session too.
  expect(prepareCodexMonoContext).toHaveBeenCalledWith(
    expect.objectContaining({
      sessionId: "monocode-codex-text",
      owner: "chat-x",
    }),
  );
  expect(threadStarts()[1].params).toMatchObject({
    config: { sqlite_home: "/private/mono" },
  });
  expect(threadStarts()[1].params).not.toHaveProperty("ephemeral");
  await stopCodexTextPrompt();
  await runCodexTextPrompt({
    cwd: "/repo",
    ephemeral: false,
    codexStore: "mono",
    threadId: "thread_2",
    prompt: "Continue",
  });
  expect(sent.find((m) => m.method === "thread/resume")?.params).toMatchObject({
    threadId: "thread_2",
    config: { sqlite_home: "/private/mono" },
  });
  expect(threadStarts()).toHaveLength(2);
});

it("reports private side-question resume failures without replacing saved context", async () => {
  missingResume = true;
  await expect(
    runCodexTextPrompt({
      cwd: "/repo",
      ephemeral: false,
      codexStore: "mono",
      threadId: "saved-mono",
      prompt: "Continue",
    }),
  ).rejects.toThrow("thread not found");
  expect(threadStarts()).toHaveLength(0);
});
