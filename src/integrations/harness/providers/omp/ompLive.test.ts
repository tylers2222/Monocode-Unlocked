import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type Mock,
} from "vitest";

const transport = vi.hoisted(() => ({
  watchers: new Map<string, (line: string) => void>(),
  exits: new Map<string, (code: number | null) => void>(),
  requests: [] as Array<{
    sessionId: string;
    command: Record<string, unknown>;
  }>,
  prompt: undefined as
    ((sessionId: string, command: Record<string, unknown>) => void) | undefined,
  fast: undefined as
    ((sessionId: string, command: Record<string, unknown>) => void) | undefined,
  model: undefined as
    ((sessionId: string, command: Record<string, unknown>) => void) | undefined,
  stats: undefined as
    ((sessionId: string, command: Record<string, unknown>) => void) | undefined,
  writeChild: vi.fn(),
  spawnChild: vi.fn(),
  forkData: undefined as unknown,
  stateSessionId: undefined as string | undefined,
  hostToolsUnknown: false,
}));

vi.mock("../../core/child", () => ({
  resolveOmpBinary: async () => ({ path: "/fake/omp" }),
  resolvePiBinary: async () => ({ path: "/fake/pi" }),
  acquireHarnessBridge: async () => () => undefined,
  spawnChild: transport.spawnChild,
  killChild: async () => undefined,
  unwatchChild: (id: string) => {
    transport.watchers.delete(id);
    transport.exits.delete(id);
  },
  watchChild: (
    id: string,
    onLine: (line: string) => void,
    onExit: (code: number | null) => void,
  ) => {
    transport.watchers.set(id, onLine);
    transport.exits.set(id, onExit);
  },
  writeChild: transport.writeChild,
}));

import {
  cancelOmpTurn,
  compactOmpContext,
  rewindOmpLastTurn,
  sendOmpTurn,
  steerOmpTurn,
  forgetOmpSession,
} from "./omp";
import { sendPiTurn, steerPiTurn, forgetPiSession } from "../pi/pi";
import { ompCommandProvider, respondQuestion } from "../pi/piFamily";
import { OMP_FLAVOR } from "../pi/piFlavor";
import {
  TurnNotReadyError,
  type HarnessEvent,
  type SendTurnInput,
} from "../../core/types";
import type { MonoTools } from "../../core/monoTools";
import { appendUser, applyHarnessEvent } from "../../core/apply";
import { newSession } from "../../../../features/sessions/model/session";
import { ATTACHMENT_ONLY_PROMPT } from "../../../../features/sessions/model/attachments";

function frame(sessionId: string, value: Record<string, unknown>) {
  transport.watchers.get(sessionId)?.(JSON.stringify(value));
}
function response(
  sessionId: string,
  command: Record<string, unknown>,
  data?: unknown,
) {
  frame(sessionId, {
    type: "response",
    id: command.id,
    command: command.type,
    success: true,
    data,
  });
}
function input(sessionId = "omp-test", text = "/workflow foo"): SendTurnInput {
  return {
    sessionId,
    text,
    cwd: "/repo",
    model: "omp:default",
    runtimeMode: "supervised",
    onEvent: (event) => events.push(event),
  };
}
let events: HarnessEvent[] = [];

beforeEach(() => {
  events = [];
  transport.requests.length = 0;
  transport.spawnChild.mockReset();
  transport.spawnChild.mockResolvedValue(undefined);
  transport.fast = undefined;
  transport.model = undefined;
  transport.stats = undefined;
  transport.forkData = undefined;
  transport.stateSessionId = undefined;
  transport.hostToolsUnknown = false;
  transport.writeChild.mockReset();
  transport.writeChild.mockImplementation(
    async (sessionId: string, line: string) => {
      const command = JSON.parse(line);
      transport.requests.push({ sessionId, command });
      if (command.type === "extension_ui_response") return;
      if (command.type === "prompt")
        return transport.prompt?.(sessionId, command);
      if (command.type === "set_fast_mode" && transport.fast)
        return transport.fast(sessionId, command);
      if (command.type === "set_model" && transport.model)
        return transport.model(sessionId, command);
      if (command.type === "get_session_stats" && transport.stats)
        return transport.stats(sessionId, command);
      if (command.type === "set_host_tools" && transport.hostToolsUnknown)
        return frame(sessionId, {
          type: "response",
          id: command.id,
          command: command.type,
          success: false,
          error: `Unknown command: ${command.type}`,
        });
      response(
        sessionId,
        command,
        command.type === "get_state"
          ? { sessionId: transport.stateSessionId ?? "provider-session" }
          : command.type === "get_available_commands"
            ? { commands: [{ name: "workflow", source: "custom" }] }
            : command.type === "get_fork_messages"
              ? { messages: [{ entryId: "entry", text: "latest" }] }
              : command.type === "fork"
                ? (transport.forkData ?? {})
                : {},
      );
    },
  );
});
afterEach(async () => {
  for (const id of [...transport.watchers.keys()]) {
    await forgetOmpSession(id);
    await forgetPiSession(id);
  }
});

async function started(turnInput = input()) {
  let settled = false;
  const turn = sendOmpTurn(turnInput).finally(() => {
    settled = true;
  });
  // Attach a rejection handler before emitting an asynchronous RPC error.
  void turn.catch(() => undefined);
  await vi.waitFor(() =>
    expect(
      transport.requests.some(
        (r) =>
          r.sessionId === turnInput.sessionId && r.command.type === "prompt",
      ),
    ).toBe(true),
  );
  const request = transport.requests
    .filter(
      (r) => r.sessionId === turnInput.sessionId && r.command.type === "prompt",
    )
    .at(-1)!.command;
  return { turn, request, settled: () => settled };
}

describe("Pi-family edit recovery", () => {
  it("reads fork cancellation from the RPC data payload", async () => {
    const running = await started();
    frame("omp-test", {
      type: "prompt_result",
      id: running.request.id,
      agentInvoked: false,
    });
    await running.turn;

    transport.forkData = { cancelled: true };
    await expect(
      rewindOmpLastTurn({
        sessionId: "omp-test",
        cwd: "/repo",
        model: "omp:default",
        runtimeMode: "supervised",
        onEvent: () => undefined,
      }),
    ).rejects.toThrow("Edit cancelled");
  });

  it("rebinds to the provider session created by fork", async () => {
    const running = await started();
    frame("omp-test", {
      type: "prompt_result",
      id: running.request.id,
      agentInvoked: false,
    });
    await running.turn;

    transport.stateSessionId = "forked-provider-session";
    const events: HarnessEvent[] = [];
    await expect(
      rewindOmpLastTurn({
        sessionId: "omp-test",
        cwd: "/repo",
        model: "omp:default",
        runtimeMode: "supervised",
        onEvent: (event) => events.push(event),
      }),
    ).resolves.toEqual({ submitted: false });

    expect(events).toContainEqual({
      type: "session.providerBound",
      providerSessionId: "forked-provider-session",
    });
  });
});

describe("OMP command lifecycle over the real RPC multiplexer", () => {
  it.each([["pi", sendPiTurn], ["omp", sendOmpTurn]] as const)(
    "forwards %s extension result details into the subagent trail",
    async (flavor, send) => {
      const sessionId = `${flavor}-subagent`;
      const turn = send({ ...input(sessionId, "Investigate auth"), model: `${flavor}:default` });
      await vi.waitFor(() => expect(transport.requests.some((r) => r.sessionId === sessionId && r.command.type === "prompt")).toBe(true));
      try {
        frame(sessionId, { type: "tool_execution_start", toolCallId: "spawn", toolName: "task", args: { agent: "scout", task: "Check auth" } });
        const result = { content: [{ type: "text", text: "Found auth" }], details: { results: [
          { agent: "scout", task: "Check auth", exitCode: 0, messages: [{ role: "assistant", content: [{ type: "text", text: "Reading auth" }] }] },
        ] } };
        frame(sessionId, { type: "tool_execution_update", toolCallId: "spawn", partialResult: result });
        frame(sessionId, { type: "tool_execution_end", toolCallId: "spawn", result, isError: false });
        const session = events.reduce(applyHarnessEvent, newSession(flavor, "/repo"));
        const row = session.blocks.find((block) => block.tool?.callId === "spawn");
        expect(row?.agentRun?.steps).toEqual([expect.objectContaining({ kind: "message", text: "Reading auth" })]);
        expect(row?.tool?.status).toBe("completed");
        expect(row?.tool?.detail).toBe("Found auth");
      } finally {
        frame(sessionId, { type: "agent_end" });
        await turn;
      }
    },
  );

  it.each([["pi", sendPiTurn], ["omp", sendOmpTurn]] as const)(
    "keeps a %s tool finished when a progress update arrives after its end",
    async (flavor, send) => {
      const sessionId = `${flavor}-late-update`;
      const turn = send({
        ...input(sessionId, "Run it"),
        model: `${flavor}:default`,
      });
      await vi.waitFor(() =>
        expect(
          transport.requests.some(
            (r) => r.sessionId === sessionId && r.command.type === "prompt",
          ),
        ).toBe(true),
      );
      const text = (value: string) => ({
        content: [{ type: "text", text: value }],
      });
      const update = {
        type: "tool_execution_update",
        toolCallId: "sh",
        partialResult: text("building"),
      };
      try {
        frame(sessionId, {
          type: "tool_execution_start",
          toolCallId: "sh",
          toolName: "bash",
          args: { command: "make" },
        });
        frame(sessionId, update);
        frame(sessionId, {
          type: "tool_execution_end",
          toolCallId: "sh",
          result: text("built"),
          isError: false,
        });
        // omp#12875: steering during bash can deliver an update after the end.
        frame(sessionId, update);
        const row = events
          .reduce(applyHarnessEvent, newSession(flavor, "/repo"))
          .blocks.find((block) => block.tool?.callId === "sh");
        expect(row?.tool).toMatchObject({
          status: "completed",
          detail: "built",
        });
        expect(events).toContainEqual(
          expect.objectContaining({
            type: "tool.updated",
            status: "running",
            detail: "building",
          }),
        );
      } finally {
        frame(sessionId, { type: "agent_end" });
        await turn;
      }
    },
  );

  it.each([
    ["pi", sendPiTurn, steerPiTurn],
    ["omp", sendOmpTurn, steerOmpTurn],
  ] as const)(
    "delivers attachment-only prompts and steering through %s",
    async (flavor, send, steer) => {
      const sessionId = `${flavor}-attachments`;
      const attachments = [
        {
          id: "pdf",
          name: "report.pdf",
          kind: "file" as const,
          mimeType: "application/pdf",
          size: 100,
          path: "/tmp/report.pdf",
        },
      ];
      const turnInput = {
        ...input(sessionId, ""),
        model: `${flavor}:default`,
        attachments,
      };
      const turn = send(turnInput);
      void turn.catch(() => undefined);
      try {
        await vi.waitFor(() =>
          expect(
            transport.requests.some(
              (r) => r.sessionId === sessionId && r.command.type === "prompt",
            ),
          ).toBe(true),
        );
        expect(
          transport.requests.find(
            (r) => r.sessionId === sessionId && r.command.type === "prompt",
          )?.command.message,
        ).toBe(
          `${ATTACHMENT_ONLY_PROMPT}\n\nAttached file (read from disk): "/tmp/report.pdf"`,
        );
        await steer(turnInput);
        expect(
          transport.requests.find(
            (r) => r.sessionId === sessionId && r.command.type === "steer",
          )?.command.message,
        ).toBe(
          `${ATTACHMENT_ONLY_PROMPT}\n\nAttached file (read from disk): "/tmp/report.pdf"`,
        );
      } finally {
        frame(sessionId, { type: "agent_end" });
        await turn;
      }
    },
  );

  it("applies fast mode through OMP RPC before prompting", async () => {
    const running = await started({
      ...input(),
      modelSettings: { fast: "true" },
    });
    const fast = transport.requests.find(
      (request) => request.command.type === "set_fast_mode",
    );
    const promptIndex = transport.requests.findIndex(
      (request) => request.command.type === "prompt",
    );

    expect(fast?.command).toMatchObject({
      type: "set_fast_mode",
      enabled: true,
    });
    expect(transport.requests.indexOf(fast!)).toBeLessThan(promptIndex);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("keeps fast mode in sync with OMP config updates", async () => {
    const running = await started();
    frame("omp-test", {
      type: "config_update",
      fastModeEnabled: true,
    });

    expect(events).toContainEqual({
      type: "session.configChanged",
      modelSettings: { fast: "true" },
    });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("falls back cleanly when the current model cannot use fast mode", async () => {
    transport.fast = (sessionId, command) => {
      frame(sessionId, {
        type: "response",
        id: command.id,
        command: command.type,
        success: false,
        error: "Fast mode is unavailable for the current model.",
      });
    };
    const running = await started({
      ...input(),
      modelSettings: { fast: "true" },
    });

    expect(
      transport.requests.filter(
        (request) => request.command.type === "set_fast_mode",
      ),
    ).toHaveLength(1);
    expect(events).toContainEqual({
      type: "session.configChanged",
      modelSettings: { fast: "false" },
    });
    expect(events).toContainEqual({
      type: "status",
      text: "Fast mode is unavailable for the current model.",
    });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("reflects command-driven model/settings and session changes in MonoCode", async () => {
    const running = await started();
    frame("omp-test", {
      type: "config_update",
      model: { provider: "anthropic", id: "new-model" },
      thinkingLevel: "high",
    });
    frame("omp-test", {
      type: "session_info_update",
      sessionId: "new-provider-session",
      title: "New session",
    });
    const config = events.find((e) => e.type === "session.configChanged")!;
    expect(config).toEqual({
      type: "session.configChanged",
      model: "omp:anthropic/new-model",
      modelSettings: { thinking: "high" },
    });
    expect(events).toContainEqual({
      type: "session.providerBound",
      providerSessionId: "new-provider-session",
    });
    const session = {
      ...newSession("omp"),
      modelSettings: { existing: "keep" },
    };
    const updated = applyHarnessEvent(session, config);
    expect(updated.model).toBe("omp:anthropic/new-model");
    expect(updated.modelSettings).toEqual({
      existing: "keep",
      thinking: "high",
    });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });
  it("sends arguments unchanged and renders local output without waiting for agent_end", async () => {
    transport.prompt = (id, command) => {
      frame(id, {
        type: "command_output",
        text: "\u001b[32mSelected reviewer: careful\u001b[0m",
      });
      response(id, command, { agentInvoked: false });
    };
    await sendOmpTurn(input());
    expect(
      transport.requests.find((r) => r.command.type === "prompt")?.command
        .message,
    ).toBe("/workflow foo");
    expect(events).toContainEqual({
      type: "status",
      text: "Selected reviewer: careful",
    });
    expect(events.filter((e) => e.type === "message.completed")).toHaveLength(
      1,
    );
    expect(transport.spawnChild.mock.calls[0].slice(0, 6)).toEqual([
      "omp-test",
      "/fake/omp",
      ["--mode", "rpc"],
      "/repo",
      undefined,
      "omp",
    ]);
  });

  // ADE-117. Defect: a session that chose mcp "none" starts omp with its MCP
  // servers, a default session's start changes, or Pi gets an omp-only choice.
  it("starts omp without MCP servers only for a session that chose none", async () => {
    for (const turn of [
      sendOmpTurn({ ...input("omp-none"), mcp: "none" }),
      sendOmpTurn(input("omp-default")),
      sendPiTurn({ ...input("pi-none"), model: "pi:default", mcp: "none" }),
    ])
      void turn.catch(() => undefined);
    await vi.waitFor(() => expect(transport.spawnChild).toHaveBeenCalledTimes(3));
    const calls = transport.spawnChild.mock.calls;
    expect(calls.map((call) => [call[0], call[2], call[8]])).toEqual([
      ["omp-none", ["--mode", "rpc"], "none"],
      ["omp-default", ["--mode", "rpc"], undefined],
      ["pi-none", ["--mode", "rpc"], undefined],
    ]);
  });

  it("reports when OMP accepts a turn", async () => {
    const onAccepted = vi.fn();
    transport.prompt = (id, command) => {
      response(id, command, { agentInvoked: false });
    };

    await sendOmpTurn({ ...input(), onAccepted });

    expect(onAccepted).toHaveBeenCalledOnce();
  });

  it.each(["before", "after"])(
    "handles prompt_result %s its acknowledgement",
    async (order) => {
      transport.prompt = (id, command) => {
        if (order === "before")
          frame(id, {
            type: "prompt_result",
            id: command.id,
            agentInvoked: false,
          });
        response(id, command);
      };
      const running = await started();
      if (order === "after")
        frame("omp-test", {
          type: "prompt_result",
          id: running.request.id,
          agentInvoked: false,
        });
      await running.turn;
      expect(events.filter((e) => e.type === "message.completed")).toHaveLength(
        1,
      );
    },
  );

  it("ignores results belonging to another request and nonterminal agent_end", async () => {
    const running = await started();
    frame("omp-test", {
      type: "prompt_result",
      id: "another-request",
      agentInvoked: false,
    });
    frame("omp-test", {
      type: "prompt_result",
      id: running.request.id,
      agentInvoked: true,
    });
    frame("omp-test", { type: "agent_end", isTerminal: false });
    await Promise.resolve();
    expect(running.settled()).toBe(false);
    expect(events.some((e) => e.type === "message.completed")).toBe(false);
    frame("omp-test", { type: "agent_end", isTerminal: true });
    await running.turn;
  });

  it("keeps normal chat and agent-invoking commands active until terminal completion", async () => {
    transport.prompt = (id, command) =>
      response(id, command, { agentInvoked: true });
    const running = await started(input("omp-test", "Explain this code"));
    expect(running.request.message).toBe("Explain this code");
    expect(running.settled()).toBe(false);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("reports prompt errors delivered after the acknowledgement", async () => {
    const running = await started();
    frame("omp-test", {
      type: "response",
      command: "prompt",
      id: running.request.id,
      success: false,
      error: "Workflow failed",
    });
    await expect(running.turn).rejects.toThrow("Workflow failed");
    expect(events).toContainEqual({
      type: "session.error",
      message: "Workflow failed",
    });
  });

  it("cancels an unacknowledged command and ignores its late completion during the next turn", async () => {
    transport.prompt = () => undefined;
    const first = await started();
    await cancelOmpTurn("omp-test");
    await first.turn;
    transport.requests.length = 0;
    transport.prompt = (id, command) => response(id, command);
    const second = await started(input("omp-test", "hello"));
    frame("omp-test", {
      type: "prompt_result",
      id: first.request.id,
      agentInvoked: false,
    });
    frame("omp-test", {
      type: "response",
      command: "prompt",
      id: first.request.id,
      success: true,
      data: { agentInvoked: false },
    });
    expect(second.settled()).toBe(false);
    frame("omp-test", { type: "agent_end" });
    await second.turn;
  });

  it("uses prompt for a slash command submitted while streaming, without finishing the main turn", async () => {
    const running = await started(input("omp-test", "hello"));
    transport.prompt = (id, command) =>
      response(id, command, { agentInvoked: false });
    await steerOmpTurn({ ...input(), text: "/usage" });
    expect(transport.requests.at(-1)?.command).toMatchObject({
      type: "prompt",
      message: "/usage",
      streamingBehavior: "steer",
    });
    expect(running.settled()).toBe(false);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("keeps Pi's normal retry and completion path intact", async () => {
    let settled = false;
    const turn = sendPiTurn({
      ...input("pi-test", "/skill:architect foo"),
      model: "pi:default",
    }).then(() => {
      settled = true;
    });
    await vi.waitFor(() =>
      expect(transport.requests.some((r) => r.command.type === "prompt")).toBe(
        true,
      ),
    );
    frame("pi-test", { type: "agent_end", willRetry: true });
    await Promise.resolve();
    expect(settled).toBe(false);
    frame("pi-test", { type: "agent_settled" });
    await turn;
    expect(events.filter((e) => e.type === "message.completed")).toHaveLength(
      1,
    );
  });

  it("surfaces displayed OMP advisor notes and hides internal messages", async () => {
    const running = await started();
    const advisor = {
      role: "custom",
      customType: "advisor",
      display: true,
      content: "raw advisory envelope",
      details: {
        notes: [
          { note: "Minor note", severity: "nit" },
          { note: "Stop here", severity: "blocker" },
        ],
      },
    };
    frame("omp-test", { type: "message_start", message: advisor });
    frame("omp-test", { type: "message_end", message: advisor });
    frame("omp-test", {
      type: "message_start",
      message: {
        role: "custom",
        customType: "xdev-mount-notice",
        display: false,
        content: "internal mount details",
      },
    });
    frame("omp-test", { type: "advisor_yielded" });

    expect(events).toContainEqual({
      type: "interjection",
      text: "Minor note\n\nStop here",
      customType: "advisor",
      severity: "blocker",
    });
    expect(
      events.filter((event) => event.type === "interjection"),
    ).toHaveLength(1);
    expect(
      events.some(
        (event) => "text" in event && event.text === "internal mount details",
      ),
    ).toBe(false);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("emits a transient status when the OMP advisor yields", async () => {
    const running = await started();
    const start = events.length;
    frame("omp-test", { type: "advisor_yielded" });
    expect(events.slice(start)).toEqual([
      { type: "status", text: expect.any(String) },
    ]);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("uses generic content for other displayed OMP custom messages", async () => {
    const running = await started();
    frame("omp-test", {
      type: "message_start",
      message: {
        role: "custom",
        customType: "extension-notice",
        display: true,
        content: [
          { type: "text", text: "Extension changed the plan." },
          { type: "image", data: "ignored", mimeType: "image/png" },
          { type: "text", text: "Review it." },
        ],
      },
    });

    expect(events).toContainEqual({
      type: "interjection",
      text: "Extension changed the plan.\nReview it.",
      customType: "extension-notice",
    });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });

  it("does not reinterpret Pi custom messages as OMP interjections", async () => {
    const turn = sendPiTurn({
      ...input("pi-test", "hello"),
      model: "pi:default",
    });
    await vi.waitFor(() =>
      expect(transport.requests.some((r) => r.command.type === "prompt")).toBe(
        true,
      ),
    );
    frame("pi-test", {
      type: "message_start",
      message: {
        role: "custom",
        customType: "advisor",
        display: true,
        content: "Pi-owned custom frame",
      },
    });
    expect(events.some((event) => event.type === "interjection")).toBe(false);
    frame("pi-test", { type: "agent_end" });
    await turn;
  });
});

// PR 5 review m2 and n1. Defect: a Stop before the prompt reached omp ends the
// send as sent, so the registry confirms a Mono opener omp never got. Pi runs
// on the same piFamily path.
describe("Pi-family turn result", () => {
  it("omp turn result: a turn cancelled while its process spawns is cancelled, and a run turn is sent", async () => {
    let releaseSpawn: () => void = () => undefined;
    transport.spawnChild.mockImplementationOnce(
      () => new Promise<void>((resolve) => { releaseSpawn = resolve; }),
    );
    const cancelled = sendOmpTurn({
      ...input("omp-result", "never sent"),
      turnId: "t-cancelled",
    });
    await vi.waitFor(() => expect(transport.spawnChild).toHaveBeenCalledOnce());
    await cancelOmpTurn("omp-result");
    releaseSpawn();
    expect(await cancelled).toBe("cancelled");
    expect(transport.requests.some((r) => r.command.type === "prompt")).toBe(
      false,
    );

    transport.prompt = (id, command) => response(id, command);
    const running = await started({
      ...input("omp-result", "hello"),
      turnId: "t-sent",
    });
    frame("omp-result", { type: "agent_end" });
    expect(await running.turn).toBe("sent");
  });

  // n1. Defect: the turn counted as sent before its set_model, so a Stop that
  // failed set_model reported a turn whose prompt omp never got as sent.
  it("omp turn result: a turn cancelled while it sets its model is cancelled", async () => {
    transport.prompt = (id, command) => response(id, command);
    const base = { ...input("omp-model"), model: "omp:anthropic/claude-a" };
    const first = await started({ ...base, text: "first", turnId: "t-first" });
    const queued = sendOmpTurn({
      ...base,
      text: "never sent",
      turnId: "t-cancelled",
    });
    // omp moves the session to another model during the first turn, so the
    // queued turn sets its own model again before its prompt.
    frame("omp-model", {
      type: "config_update",
      model: { provider: "anthropic", id: "claude-b" },
    });
    let setModel: Record<string, unknown> | undefined;
    transport.model = (_sessionId, command) => {
      setModel = command;
    };
    frame("omp-model", { type: "agent_end" });
    expect(await first.turn).toBe("sent");
    await vi.waitFor(() => expect(setModel).toBeDefined());
    await cancelOmpTurn("omp-model");
    frame("omp-model", {
      type: "response",
      id: setModel!.id,
      command: "set_model",
      success: false,
      error: "Model switch aborted",
    });
    expect(await queued).toBe("cancelled");
    expect(
      transport.requests.filter(
        (r) => r.sessionId === "omp-model" && r.command.type === "prompt",
      ),
    ).toHaveLength(1);
  });
});

// ADE-82. A run omp starts on its own, after the app turn ended, streams
// with no app turn open; only its own settle can seal what it streamed.
describe("OMP run started by omp itself", () => {
  async function afterAppTurn(
    startHarnessTurn?: SendTurnInput["startHarnessTurn"],
  ): Promise<number> {
    const running = await started({
      ...input("omp-test", "hello"),
      startHarnessTurn,
    });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
    return events.length;
  }
  /** The registry's side: each call opens a turn, whose end is a spy. */
  function harnessTurns() {
    const ends: Array<Mock<() => void>> = [];
    const start = vi.fn(() => {
      const end = vi.fn<() => void>();
      ends.push(end);
      return end;
    });
    return { start, ends };
  }
  function idleReasoningRun() {
    frame("omp-test", {
      type: "message_update",
      assistantMessageEvent: {
        type: "thinking_delta",
        delta: "Weighing the job result.",
      },
    });
    frame("omp-test", { type: "agent_end" });
  }

  it("seals reasoning that ends the run", async () => {
    const before = await afterAppTurn();
    idleReasoningRun();
    let session = newSession("omp", "/repo");
    for (const event of events.slice(before))
      session = applyHarnessEvent(session, event);
    const reasoning = session.blocks.filter((b) => b.role === "reasoning");
    expect(reasoning).toHaveLength(1);
    expect(reasoning[0].streaming).toBeFalsy();
  });

  it("leaves a refused steer deferred when the run only reasons", async () => {
    const before = await afterAppTurn();
    await expect(
      steerOmpTurn({ ...input(), text: "also this" }),
    ).rejects.toBeInstanceOf(TurnNotReadyError);
    let session = {
      ...appendUser(newSession("omp", "/repo"), "hello"),
      turnReadiness: "deferred" as const,
    };
    idleReasoningRun();
    for (const event of events.slice(before))
      session = applyHarnessEvent(session, event);
    expect(session.turnReadiness).toBe("deferred");
  });

  // ADE-138. Defect: a run omp starts by itself opens no turn, so its app
  // calls are refused; the app turn's own run, or a frame that starts no
  // run, opens one; or the run's turn outlives its settle or a stop.
  it("opens one turn per run it starts and ends it with the run", async () => {
    const turns = harnessTurns();
    const running = await started({
      ...input("omp-test", "hello"),
      startHarnessTurn: turns.start,
    });
    frame("omp-test", { type: "agent_start" });
    frame("omp-test", { type: "agent_end" });
    await running.turn;
    frame("omp-test", { type: "turn_start" });
    expect(turns.start).not.toHaveBeenCalled();

    frame("omp-test", { type: "agent_start" });
    frame("omp-test", { type: "agent_start" });
    expect(turns.start).toHaveBeenCalledTimes(1);
    expect(turns.ends[0]).not.toHaveBeenCalled();
    frame("omp-test", { type: "agent_end" });
    await vi.waitFor(() => expect(turns.ends[0]).toHaveBeenCalledTimes(1));

    frame("omp-test", { type: "agent_start" });
    await forgetOmpSession("omp-test");
    expect(turns.ends[1]).toHaveBeenCalledTimes(1);
    expect(turns.ends[0]).toHaveBeenCalledTimes(1);
  });

  // Review r1 B3. Defect: a Stop while the app turn settles leaves it
  // settling, so the settle of a later run omp starts returns early: that
  // run's turn stays open, and its output is never sealed.
  it("ends a later run's turn after a Stop during the app turn's settle", async () => {
    const turns = harnessTurns();
    const running = await started({
      ...input("omp-test", "hello"),
      startHarnessTurn: turns.start,
    });
    let stats: unknown;
    transport.stats = (_sessionId, command) => {
      stats = command;
    };
    frame("omp-test", { type: "agent_end" });
    await vi.waitFor(() => expect(stats).toBeDefined());
    await cancelOmpTurn("omp-test");
    await running.turn;
    await compactOmpContext(input("omp-test"));

    const before = events.length;
    frame("omp-test", { type: "agent_start" });
    idleReasoningRun();
    await vi.waitFor(() => expect(turns.ends[0]).toHaveBeenCalledTimes(1));
    let session = newSession("omp", "/repo");
    for (const event of events.slice(before))
      session = applyHarnessEvent(session, event);
    const reasoning = session.blocks.filter((b) => b.role === "reasoning");
    expect(reasoning).toHaveLength(1);
    expect(reasoning[0].streaming).toBeFalsy();
  });

  // Review r1 m3. Defect: omp exits during a run it started by itself and
  // the run's turn stays open, so a child that outlives omp keeps the token.
  it("ends a run's turn when omp exits", async () => {
    const turns = harnessTurns();
    await afterAppTurn(turns.start);
    frame("omp-test", { type: "agent_start" });
    transport.exits.get("omp-test")?.(1);
    expect(turns.ends[0]).toHaveBeenCalledTimes(1);
  });

  // Review r1 m3. Defect: the next app turn starts while a run omp started
  // is open, and that run's turn stays open beside it.
  it("ends a run's turn when the next app turn starts", async () => {
    const turns = harnessTurns();
    await afterAppTurn(turns.start);
    frame("omp-test", { type: "agent_start" });
    const next = sendOmpTurn({
      ...input("omp-test", "again"),
      startHarnessTurn: turns.start,
    });
    await vi.waitFor(() =>
      expect(
        transport.requests.filter((r) => r.command.type === "prompt"),
      ).toHaveLength(2),
    );
    expect(turns.ends[0]).toHaveBeenCalledTimes(1);
    frame("omp-test", { type: "agent_end" });
    await next;
    frame("omp-test", { type: "agent_start" });
    expect(turns.start).toHaveBeenCalledTimes(2);
  });

  // Review r1 m2. Defect: a rewind can drop the /operator turn, yet a run
  // omp starts before the next send gets the access of the send before.
  it("gives a run no turn after a rewind, until the next send", async () => {
    const turns = harnessTurns();
    await afterAppTurn(turns.start);
    frame("omp-test", { type: "agent_start" });
    await rewindOmpLastTurn({
      sessionId: "omp-test",
      cwd: "/repo",
      model: "omp:default",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });
    expect(turns.ends[0]).toHaveBeenCalledTimes(1);
    frame("omp-test", { type: "agent_end" });
    frame("omp-test", { type: "agent_start" });
    expect(turns.start).toHaveBeenCalledTimes(1);
  });
});

describe("OMP live command inventories", () => {
  it("loads through the active process and isolates push updates by session and directory", async () => {
    const a: unknown[] = [],
      b: unknown[] = [],
      otherCwd: unknown[] = [];
    const unsub = [
      ompCommandProvider.subscribe!(
        { sessionId: "omp-test", cwd: "/repo/" },
        (commands) => a.push(commands),
      ),
      ompCommandProvider.subscribe!(
        { sessionId: "other", cwd: "/repo" },
        (commands) => b.push(commands),
      ),
      ompCommandProvider.subscribe!(
        { sessionId: "omp-test", cwd: "/elsewhere" },
        (commands) => otherCwd.push(commands),
      ),
    ];
    const running = await started();
    await expect(
      ompCommandProvider.discover({ sessionId: "omp-test", cwd: "/repo" }),
    ).resolves.toMatchObject([{ name: "workflow" }]);
    frame("omp-test", {
      type: "available_commands_update",
      commands: [{ name: "review", source: "extension" }],
    });
    frame("omp-test", { type: "available_commands_update", commands: null });
    expect(a).toHaveLength(1);
    expect(b).toHaveLength(0);
    expect(otherCwd).toHaveLength(0);
    await expect(
      ompCommandProvider.discover({ sessionId: "omp-test", cwd: "/repo" }),
    ).resolves.toMatchObject([{ name: "review" }]);
    for (const unsubscribe of unsub) unsubscribe();
    frame("omp-test", { type: "available_commands_update", commands: [] });
    expect(a).toHaveLength(1);
    frame("omp-test", { type: "agent_end" });
    await running.turn;
  });
});

describe("OMP workflow dialogs", () => {
  it("returns the actual selected option with its original text", async () => {
    const running = await started();
    frame("omp-test", {
      type: "extension_ui_request",
      id: "choose",
      method: "select",
      title: "Choose reviewer",
      options: ["fast", "\u001b[32mcareful\u001b[0m"],
    });
    const question = events.find((e) => e.type === "question.asked");
    expect(question?.questions[0]?.options.map((o) => o.label)).toEqual([
      "fast",
      "careful",
    ]);
    respondQuestion(OMP_FLAVOR, "omp-test", question!.requestId, {
      kind: "answered",
      answers: { choose: ["1"] },
    });
    await vi.waitFor(() =>
      expect(transport.requests.at(-1)?.command).toEqual({
        type: "extension_ui_response",
        id: "choose",
        value: "\u001b[32mcareful\u001b[0m",
      }),
    );
    frame("omp-test", {
      type: "prompt_result",
      id: running.request.id,
      agentInvoked: false,
    });
    await running.turn;
  });

  it.each(["input", "editor"])(
    "returns %s text to the workflow",
    async (method) => {
      const running = await started();
      frame("omp-test", {
        type: "extension_ui_request",
        id: "text",
        method,
        title: "Instructions",
      });
      const question = events.find((e) => e.type === "question.asked");
      respondQuestion(OMP_FLAVOR, "omp-test", question!.requestId, {
        kind: "answered",
        answers: {},
        custom: { text: "Review security\nand correctness" },
      });
      await vi.waitFor(() =>
        expect(transport.requests.at(-1)?.command).toMatchObject({
          type: "extension_ui_response",
          value: "Review security\nand correctness",
        }),
      );
      frame("omp-test", {
        type: "prompt_result",
        id: running.request.id,
        agentInvoked: false,
      });
      await running.turn;
    },
  );

  it("resolves pending questions when the user stops a command", async () => {
    const running = await started();
    frame("omp-test", {
      type: "extension_ui_request",
      id: "text",
      method: "input",
      title: "Instructions",
    });
    await cancelOmpTurn("omp-test");
    await running.turn;
    expect(transport.requests.map((r) => r.command)).toContainEqual({
      type: "extension_ui_response",
      id: "text",
      cancelled: true,
    });
    expect(events.some((e) => e.type === "question.resolved")).toBe(true);
  });
});

describe("OMP Mono messaging host tools", () => {
  const monoTools = (call: MonoTools["call"]): MonoTools => ({
    exe: "/fake/monocode",
    tools: [
      {
        name: "get_monos",
        action: "messages.get_monos",
        description: "List Monos",
        inputSchema: { type: "object", properties: {}, additionalProperties: false },
      },
    ],
    call,
  });
  const sent = (sessionId: string, type: string) =>
    transport.requests.filter(
      (r) => r.sessionId === sessionId && r.command.type === type,
    );
  const done = (sessionId: string, request: Record<string, unknown>) =>
    frame(sessionId, { type: "prompt_result", id: request.id, agentInvoked: false });

  // Defect: omp never learns the tools, hides them (plan mode, discoverable),
  // gets them again every turn, or a project session gets them.
  it("registers essential host tools once for a Mono session and never for a project session", async () => {
    const tools = monoTools(async () => ({ ok: true }));
    const first = await started({ ...input("mono-chat"), monoTools: tools });
    done("mono-chat", first.request);
    await first.turn;
    const second = sendOmpTurn({ ...input("mono-chat"), monoTools: tools });
    await vi.waitFor(() => expect(sent("mono-chat", "prompt")).toHaveLength(2));
    done("mono-chat", sent("mono-chat", "prompt")[1].command);
    await second;
    const registered = sent("mono-chat", "set_host_tools");
    expect(registered).toHaveLength(1);
    expect(registered[0].command.tools).toEqual([
      expect.objectContaining({
        name: "get_monos",
        parameters: tools.tools[0].inputSchema,
        loadMode: "essential",
      }),
    ]);
    const project = await started(input("project"));
    done("project", project.request);
    await project.turn;
    expect(sent("project", "set_host_tools")).toHaveLength(0);
  });

  // Defect: an omp without `set_host_tools` fails every Mono turn instead of
  // running it with the app CLI only.
  it("runs a Mono turn without host tools when omp does not know set_host_tools", async () => {
    transport.hostToolsUnknown = true;
    const call = vi.fn<MonoTools["call"]>(async () => ({ ok: true }));
    const running = await started({ ...input("mono-chat"), monoTools: monoTools(call) });
    frame("mono-chat", {
      type: "host_tool_call",
      id: "host_1",
      toolCallId: "toolu_01",
      toolName: "get_monos",
      arguments: {},
    });
    await vi.waitFor(() =>
      expect(sent("mono-chat", "host_tool_result")).toHaveLength(1),
    );
    expect(call).not.toHaveBeenCalled();
    done("mono-chat", running.request);
    await running.turn;
    expect(running.settled()).toBe(true);
  });

  // Defect: a host tool call never reaches the app, reuses omp's frame id or
  // tool call id as the request id, or omp waits forever for its result.
  it("answers a host tool call through the app with a fresh valid request id", async () => {
    const call = vi.fn<MonoTools["call"]>(async () => ({ ok: true, monos: [] }));
    const running = await started({ ...input("mono-chat"), monoTools: monoTools(call) });
    frame("mono-chat", {
      type: "host_tool_call",
      id: "host_1",
      toolCallId: "toolu_01:abc/def",
      toolName: "get_monos",
      arguments: {},
    });
    frame("mono-chat", {
      type: "host_tool_call",
      id: "host_2",
      toolCallId: "toolu_02",
      toolName: "delete_monos",
      arguments: {},
    });
    await vi.waitFor(() =>
      expect(sent("mono-chat", "host_tool_result")).toHaveLength(2),
    );
    expect(call).toHaveBeenCalledTimes(1);
    const [action, callInput, requestId] = call.mock.calls[0];
    expect([action, callInput]).toEqual(["messages.get_monos", {}]);
    expect(requestId).toMatch(/^[A-Za-z0-9_-]{1,128}$/);
    expect(requestId).not.toBe("host_1");
    const results = sent("mono-chat", "host_tool_result").map((r) => r.command);
    expect(results.find((r) => r.id === "host_1")).toEqual({
      type: "host_tool_result",
      id: "host_1",
      result: { content: [{ type: "text", text: '{"ok":true,"monos":[]}' }] },
    });
    expect(results.find((r) => r.id === "host_2")).toMatchObject({ isError: true });
    done("mono-chat", running.request);
    await running.turn;
  });
});
