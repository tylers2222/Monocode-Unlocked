import { afterEach, describe, expect, it, vi } from "vitest";
import {
  resetHarnessModelOverlays,
  setHarnessModels,
} from "../../../features/sessions/model/models";
import type { HarnessId } from "../../../features/sessions/model/session";
import {
  HARNESS_IDLE_PARK_MS,
  bindHarnessSession,
  cancelHarnessTurn,
  canCompactHarnessContext,
  canRunHarnessTextPrompt,
  runHarnessTextPrompt,
  canRewindHarnessLastTurn,
  compactHarnessContext,
  configureHarnessIdlePark,
  enforceHarnessIdleLimit,
  isLiveHarness,
  listHarnesses,
  refreshHarnessCatalogs,
  registerHarness,
  resetHarnessIdlePark,
  sendHarnessTurn,
  type HarnessAdapter,
} from "./registry";
import type { SendTurnInput, SteerTurnInput } from "./types";
import { registerBuiltinHarnesses } from "./register";

/**
 * Outside the app the registry calls no Tauri command. `inApp` runs it as in
 * the app, where `answer` plays the Rust side of each command.
 */
const tauri = vi.hoisted(() => ({
  inApp: false,
  calls: [] as [string, Record<string, unknown> | undefined][],
  answer: (_command: string, _args: Record<string, unknown>) => {},
}));

vi.mock(import("@tauri-apps/api/core"), async (importOriginal) => {
  const original = await importOriginal();
  return {
    ...original,
    isTauri: () => tauri.inApp,
    invoke: (async (command: string, args?: Record<string, unknown>) => {
      if (!tauri.inApp) return original.invoke(command, args);
      tauri.calls.push([command, args]);
      tauri.answer(command, args ?? {});
    }) as typeof original.invoke,
  };
});

/**
 * The app's one opener of turn `turnId` in `sessionId`: a row whose text
 * opened that turn, as `confirm_openers` and the turn end treat it in Rust.
 */
function opener(sessionId: string, turnId: string) {
  const row = {
    turnId: turnId as string | null,
    deliveredAt: null as number | null,
  };
  tauri.inApp = true;
  tauri.answer = (command, args) => {
    if (args.sessionId !== sessionId || args.turnId !== row.turnId) return;
    if (command === "mono_message_confirm_openers") row.deliveredAt ??= 1;
    // The turn end takes the slot of a row the provider never took, and
    // reconcile queues it again.
    if (command === "control_turn_finished" && row.deliveredAt === null)
      row.turnId = null;
  };
  return row;
}

function stub(
  id: "cursor" | "codex" | "claude" | "pi",
  extra: Partial<HarnessAdapter> = {},
): HarnessAdapter {
  return {
    id,
    live: true,
    async sendTurn(_input: SendTurnInput) {
      return "sent" as const;
    },
    async steerTurn(_input: SteerTurnInput) {},
    async cancelTurn() {},
    respondApproval() {},
    async stopSession() {},
    async forgetSession() {},
    bindSession() {},
    ...extra,
  };
}

describe("harness registry", () => {
  afterEach(() => {
    resetHarnessModelOverlays();
    resetHarnessIdlePark();
    vi.useRealTimers();
    tauri.inApp = false;
    tauri.calls = [];
    tauri.answer = () => {};
  });

  it("tracks live adapters", () => {
    registerHarness(stub("cursor"));
    registerHarness(stub("codex"));
    registerHarness(stub("claude"));
    expect(isLiveHarness("cursor")).toBe(true);
    expect(isLiveHarness("codex")).toBe(true);
    expect(isLiveHarness("claude")).toBe(true);
    expect(
      listHarnesses()
        .map((a) => a.id)
        .filter((id) => id === "claude" || id === "codex" || id === "cursor")
        .sort(),
    ).toEqual(["claude", "codex", "cursor"]);
  });

  it("announces readiness when the provider accepts, while preserving the caller's acceptance callback", async () => {
    let accepted!: () => void;
    let finish!: () => void;
    registerHarness(
      stub("codex", {
        async sendTurn(input) {
          accepted = () => input.onAccepted?.();
          await new Promise<void>((resolve) => {
            finish = resolve;
          });
          return "sent" as const;
        },
      }),
    );
    const onEvent = vi.fn();
    const onAccepted = vi.fn();
    const sending = sendHarnessTurn({
      harness: "codex",
      sessionId: "readiness",
      cwd: "/tmp",
      model: "codex:gpt-5.4",
      runtimeMode: "supervised",
      text: "Hello",
      turnId: "readiness-1",
      onEvent,
      onAccepted,
    });
    await vi.waitFor(() => expect(accepted).toBeDefined());
    expect(onEvent).not.toHaveBeenCalled();
    accepted();
    expect(onEvent).toHaveBeenCalledExactlyOnceWith({ type: "turn.ready" });
    expect(onAccepted).toHaveBeenCalledOnce();
    finish();
    await sending;
  });

  // Design 5.4 (3). Defect: an opener on a provider that never reports
  // turn.ready is never confirmed, or only after the turn end took its slot,
  // so the message goes back to the queue and is delivered again.
  it("confirms the openers of a turn whose send resolved, before the turn ends", async () => {
    const row = opener("s1", "t1");
    registerHarness(stub("cursor"));
    await sendHarnessTurn({
      harness: "cursor",
      sessionId: "s1",
      cwd: "/tmp",
      model: "cursor:composer-2.5",
      runtimeMode: "supervised",
      text: "hi",
      turnId: "t1",
      onEvent: () => undefined,
    });
    expect(row).toEqual({ turnId: "t1", deliveredAt: 1 });
    expect(tauri.calls[0]).toEqual([
      "control_authorize_turn",
      { sessionId: "s1", turnId: "t1", cwd: "/tmp", appAccess: false },
    ]);
  });

  // PR 5 review M2. Defect: a Stop while the provider's process spawns ends
  // the send normally, through the adapter's cancel guard, and the registry
  // confirms an opener that the provider never got.
  it("leaves the opener of a turn cancelled while its provider spawns unconfirmed", async () => {
    const row = opener("s1", "t1");
    let spawned!: () => void;
    const cancelled = new Set<string>();
    registerHarness(
      stub("cursor", {
        // The guard of cursor.ts, claude.ts and the other live adapters.
        async sendTurn(input) {
          await new Promise<void>((resolve) => {
            spawned = resolve;
          });
          if (cancelled.delete(input.sessionId)) return "cancelled";
          return "sent";
        },
        async cancelTurn(sessionId) {
          cancelled.add(sessionId);
        },
      }),
    );
    const sending = sendHarnessTurn({
      harness: "cursor",
      sessionId: "s1",
      cwd: "/tmp",
      model: "cursor:composer-2.5",
      runtimeMode: "supervised",
      text: "hi",
      turnId: "t1",
      onEvent: () => undefined,
    });
    await vi.waitFor(() => expect(spawned).toBeDefined());
    await cancelHarnessTurn("cursor", "s1");
    spawned();
    expect(await sending).toBe("cancelled");
    expect(row).toEqual({ turnId: null, deliveredAt: null });
  });

  // Design 5.4. Defect: a send that failed counts as the provider having the
  // opener, so the message is never queued again.
  it("leaves the openers of a send that failed unconfirmed", async () => {
    const row = opener("s1", "t1");
    registerHarness(
      stub("cursor", {
        async sendTurn() {
          throw new Error("cursor exited");
        },
      }),
    );
    await expect(
      sendHarnessTurn({
        harness: "cursor",
        sessionId: "s1",
        cwd: "/tmp",
        model: "cursor:composer-2.5",
        runtimeMode: "supervised",
        text: "hi",
        turnId: "t1",
        onEvent: () => undefined,
      }),
    ).rejects.toThrow("cursor exited");
    expect(row).toEqual({ turnId: null, deliveredAt: null });
  });

  // ADE-138. Defect: a run the harness starts by itself after a turn gets no
  // turn, or not that turn's session, cwd and app access, or the id of the
  // turn before; its end reaches Rust before its authorize, or at all for a
  // run Rust refused.
  it("authorizes a run the harness starts by itself as its own turn, and ends it", async () => {
    let wake: SendTurnInput["startHarnessTurn"];
    registerHarness(
      stub("pi", {
        async sendTurn(input) {
          wake = input.startHarnessTurn;
          return "sent" as const;
        },
      }),
    );
    tauri.inApp = true;
    for (const appAccess of [true, undefined]) {
      await sendHarnessTurn({
        harness: "pi",
        sessionId: "s1",
        cwd: "/tmp",
        model: "pi:default",
        runtimeMode: "supervised",
        text: "hi",
        turnId: "t1",
        appAccess,
        onEvent: () => undefined,
      });
      tauri.calls = [];
      wake?.()();
      await vi.waitFor(() => expect(tauri.calls).toHaveLength(2));
      const [authorize, finished] = tauri.calls;
      expect(authorize).toEqual([
        "control_authorize_harness_turn",
        {
          sessionId: "s1",
          turnId: expect.not.stringMatching(/^t1$/),
          cwd: "/tmp",
          appAccess: appAccess === true,
        },
      ]);
      expect(finished).toEqual([
        "control_turn_finished",
        { sessionId: "s1", turnId: authorize[1]?.turnId },
      ]);
    }

    // Rust refuses the first run; the second shows when the first is done.
    let refuse = true;
    tauri.answer = (command) => {
      if (command !== "control_authorize_harness_turn" || !refuse) return;
      refuse = false;
      throw new Error("This checkout is controlled by an orchestrator.");
    };
    tauri.calls = [];
    wake?.()();
    wake?.()();
    await vi.waitFor(() => expect(tauri.calls).toHaveLength(3));
    const [refused, allowed, finished] = tauri.calls;
    expect([refused[0], allowed[0]]).toEqual([
      "control_authorize_harness_turn",
      "control_authorize_harness_turn",
    ]);
    expect(finished[1]?.turnId).toBe(allowed[1]?.turnId);
  });

  it("advertises isolated text prompt support by harness", () => {
    registerBuiltinHarnesses();
    const ids: HarnessId[] = [
      "claude",
      "codex",
      "cursor",
      "grok",
      "opencode",
      "pi",
      "omp",
      "fx",
      "hermes",
      "antigravity",
    ];

    expect(
      Object.fromEntries(ids.map((id) => [id, canRunHarnessTextPrompt(id)])),
    ).toEqual({
      claude: true,
      codex: true,
      cursor: true,
      grok: true,
      opencode: true,
      pi: true,
      omp: true,
      fx: false,
      hermes: false,
      antigravity: false,
    });
  });

  it("exposes the native compaction support matrix", () => {
    registerBuiltinHarnesses();
    const ids: HarnessId[] = [
      "claude",
      "codex",
      "cursor",
      "grok",
      "opencode",
      "pi",
      "omp",
      "fx",
      "antigravity",
    ];

    expect(
      Object.fromEntries(ids.map((id) => [id, canCompactHarnessContext(id)])),
    ).toEqual({
      claude: true,
      codex: true,
      cursor: false,
      grok: true,
      opencode: true,
      pi: true,
      omp: true,
      fx: false,
      antigravity: false,
    });
  });
  it("advertises and dispatches compaction only when an adapter supports it", async () => {
    const compactContext = vi.fn(async () => undefined);
    registerHarness(stub("codex", { compactContext }));
    registerHarness(stub("claude"));

    expect(canCompactHarnessContext("codex")).toBe(true);
    expect(canCompactHarnessContext("claude")).toBe(false);

    await compactHarnessContext({
      harness: "codex",
      sessionId: "compact-1",
      cwd: "/tmp",
      model: "codex:gpt-5.4",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });

    expect(compactContext).toHaveBeenCalledOnce();
    await expect(
      compactHarnessContext({
        harness: "claude",
        sessionId: "compact-2",
        cwd: "/tmp",
        model: "claude:sonnet",
        runtimeMode: "supervised",
        onEvent: () => undefined,
      }),
    ).rejects.toThrow("does not support manual compaction");
  });
  it("cancels an isolated text prompt through the adapter lifecycle", async () => {
    const runTextPrompt = vi.fn(() => new Promise<string>(() => undefined));
    const stopTextPrompt = vi.fn(async () => undefined);
    registerHarness(stub("claude", { runTextPrompt, stopTextPrompt }));
    const controller = new AbortController();
    const request = runHarnessTextPrompt({
      harness: "claude",
      cwd: "/tmp",
      prompt: "read-only question",
      signal: controller.signal,
    });

    controller.abort();

    await expect(request).rejects.toThrow("By-the-way request cancelled");
    expect(runTextPrompt).toHaveBeenCalledOnce();
    expect(stopTextPrompt).not.toHaveBeenCalled();
  });

  it("does not stop the shared text backend while another prompt is active", async () => {
    const runTextPrompt = vi.fn(() => new Promise<string>(() => undefined));
    const stopTextPrompt = vi.fn(async () => undefined);
    registerHarness(stub("claude", { runTextPrompt, stopTextPrompt }));
    const first = new AbortController();
    const second = new AbortController();
    const firstRequest = runHarnessTextPrompt({
      harness: "claude",
      cwd: "/tmp",
      prompt: "first",
      signal: first.signal,
    });
    const secondRequest = runHarnessTextPrompt({
      harness: "claude",
      cwd: "/tmp",
      prompt: "second",
      signal: second.signal,
    });
    first.abort();
    await expect(firstRequest).rejects.toThrow("By-the-way request cancelled");
    expect(stopTextPrompt).not.toHaveBeenCalled();
    second.abort();
    await expect(secondRequest).rejects.toThrow("By-the-way request cancelled");
    expect(stopTextPrompt).not.toHaveBeenCalled();
  });

  it("exposes the edit-last-turn support matrix", () => {
    registerBuiltinHarnesses();
    const ids: HarnessId[] = [
      "claude",
      "codex",
      "cursor",
      "grok",
      "opencode",
      "pi",
      "omp",
      "fx",
    ];

    expect(
      Object.fromEntries(ids.map((id) => [id, canRewindHarnessLastTurn(id)])),
    ).toEqual({
      claude: false,
      codex: true,
      cursor: false,
      grok: false,
      opencode: true,
      pi: true,
      omp: true,
      fx: false,
    });
  });

  it("registers Antigravity as a live fx-tier harness", () => {
    registerBuiltinHarnesses();
    expect(isLiveHarness("antigravity")).toBe(true);
    const adapter = listHarnesses().find(
      (adapter) => adapter.id === "antigravity",
    )!;
    expect(adapter.canSteer).toBe(false);
    expect(adapter.bindSession).toBeTypeOf("function");
    expect(adapter.refreshCatalog).toBeTypeOf("function");
    expect(adapter.generateTitle).toBeUndefined();
    expect(adapter.generateCommitMessage).toBeUndefined();
  });

  it("refreshes only the requested catalogs", async () => {
    const pi = vi.fn(async () => undefined);
    const claude = vi.fn(async () => undefined);
    registerHarness(stub("pi", { refreshCatalog: pi }));
    registerHarness(stub("claude", { refreshCatalog: claude }));

    await refreshHarnessCatalogs(["claude"]);

    expect(claude).toHaveBeenCalledOnce();
    expect(pi).not.toHaveBeenCalled();
  });

  it("does not spawn a catalog probe twice after a live list lands", async () => {
    const pi = vi.fn(async () => {
      setHarnessModels("pi", [
        {
          id: "pi:opus",
          harness: "pi",
          name: "Opus",
          nativeId: "anthropic/opus",
        },
      ]);
    });
    registerHarness(stub("pi", { refreshCatalog: pi }));

    await refreshHarnessCatalogs(["pi"]);
    await refreshHarnessCatalogs(["pi"]);

    expect(pi).toHaveBeenCalledOnce();
  });

  it("forces a catalog probe past the live-list skip on request", async () => {
    const pi = vi.fn(async () => {
      setHarnessModels("pi", [
        {
          id: "pi:opus",
          harness: "pi",
          name: "Opus",
          nativeId: "anthropic/opus",
        },
      ]);
    });
    registerHarness(stub("pi", { refreshCatalog: pi }));

    await refreshHarnessCatalogs(["pi"]);
    await refreshHarnessCatalogs(["pi"], { force: true });

    expect(pi).toHaveBeenCalledTimes(2);
  });

  it("skips catalog refresh when no harness is in use", async () => {
    const pi = vi.fn(async () => undefined);
    registerHarness(stub("pi", { refreshCatalog: pi }));
    await refreshHarnessCatalogs([]);
    expect(pi).not.toHaveBeenCalled();
  });

  it("parks a live child a few minutes after the turn settles", async () => {
    vi.useFakeTimers();
    const stopSession = vi.fn(async () => undefined);
    registerHarness(stub("cursor", { stopSession }));

    await sendHarnessTurn({
      harness: "cursor",
      sessionId: "s1",
      cwd: "/tmp",
      model: "cursor:composer-2.5",
      text: "hi",
      turnId: "t1",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });

    expect(stopSession).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(HARNESS_IDLE_PARK_MS - 1);
    expect(stopSession).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(stopSession).toHaveBeenCalledWith("s1");
  });
  describe("idle limit", () => {
    const turn = (sessionId: string) =>
      sendHarnessTurn({
        harness: "cursor",
        sessionId,
        cwd: "/tmp",
        model: "cursor:composer-2.5",
        text: "hi",
        turnId: `turn-${sessionId}`,
        runtimeMode: "supervised",
        onEvent: () => undefined,
      });

    it("parks the longest-idle child once too many are warm", async () => {
      vi.useFakeTimers();
      const stopSession = vi.fn(async () => undefined);
      registerHarness(stub("cursor", { stopSession }));
      configureHarnessIdlePark({ limit: () => 2, keep: () => false });

      for (const id of ["s1", "s2", "s3"]) await turn(id);
      await vi.advanceTimersByTimeAsync(0);
      expect(stopSession.mock.calls).toEqual([["s1"]]);

      // The rest still park on the idle timer.
      await vi.advanceTimersByTimeAsync(HARNESS_IDLE_PARK_MS);
      expect(stopSession.mock.calls.flat().sort()).toEqual(["s1", "s2", "s3"]);
    });

    it("spares the conversation on screen and never counts a running turn", async () => {
      vi.useFakeTimers();
      const stopSession = vi.fn(async () => undefined);
      let finishRunning!: () => void;
      registerHarness(
        stub("cursor", {
          stopSession,
          async sendTurn(input) {
            if (input.sessionId !== "running") return "sent" as const;
            await new Promise<void>((resolve) => {
              finishRunning = resolve;
            });
            return "sent" as const;
          },
        }),
      );
      configureHarnessIdlePark({
        limit: () => 1,
        keep: (id) => id === "viewed",
      });

      const running = turn("running");
      await turn("viewed");
      await turn("other");
      await vi.advanceTimersByTimeAsync(0);
      expect(stopSession.mock.calls).toEqual([["other"]]);

      finishRunning();
      await running;
      await vi.advanceTimersByTimeAsync(0);
      expect(stopSession.mock.calls).toEqual([["other"], ["running"]]);
    });

    it("parks every idle child at a limit of zero, including the one on screen", async () => {
      vi.useFakeTimers();
      const stopSession = vi.fn(async () => undefined);
      registerHarness(stub("cursor", { stopSession }));
      configureHarnessIdlePark({ limit: () => 0, keep: () => true });

      await turn("viewed");
      await vi.advanceTimersByTimeAsync(0);
      expect(stopSession).toHaveBeenCalledWith("viewed");
    });

    it("applies a lowered limit straight away", async () => {
      vi.useFakeTimers();
      const stopSession = vi.fn(async () => undefined);
      registerHarness(stub("cursor", { stopSession }));
      let limit = 3;
      configureHarnessIdlePark({ limit: () => limit, keep: () => false });

      for (const id of ["s1", "s2", "s3"]) await turn(id);
      await vi.advanceTimersByTimeAsync(0);
      expect(stopSession).not.toHaveBeenCalled();

      limit = 1;
      enforceHarnessIdleLimit();
      expect(stopSession.mock.calls).toEqual([["s1"], ["s2"]]);
    });
  });

  it("serializes provider-state operations per session", async () => {
    const order: string[] = [];
    let releaseFirst!: () => void;
    let markFirstStarted!: () => void;
    const firstStarted = new Promise<void>((resolve) => {
      markFirstStarted = resolve;
    });
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const compactContext = vi.fn(async (input: { sessionId: string }) => {
      order.push(`start:${input.sessionId}`);
      if (input.sessionId === "s1") {
        markFirstStarted();
        await firstGate;
      }
      order.push(`end:${input.sessionId}`);
    });
    registerHarness(stub("codex", { compactContext }));

    const compact1 = compactHarnessContext({
      harness: "codex",
      sessionId: "s1",
      cwd: "/tmp",
      model: "codex:gpt-5.4",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });
    await firstStarted;
    const compact2 = compactHarnessContext({
      harness: "codex",
      sessionId: "s1",
      cwd: "/tmp",
      model: "codex:gpt-5.4",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });
    const independent = compactHarnessContext({
      harness: "codex",
      sessionId: "s2",
      cwd: "/tmp",
      model: "codex:gpt-5.4",
      runtimeMode: "supervised",
      onEvent: () => undefined,
    });

    await independent;
    expect(order).toEqual(["start:s1", "start:s2", "end:s2"]);
    releaseFirst();
    await Promise.all([compact1, compact2]);
    expect(order).toEqual([
      "start:s1",
      "start:s2",
      "end:s2",
      "end:s1",
      "start:s1",
      "end:s1",
    ]);
  });

  it("binds a restored session and forwards its task panels", () => {
    const bindSession = vi.fn();
    const restoreTaskLists = vi.fn();
    registerHarness(stub("claude", { bindSession, restoreTaskLists }));
    const taskList = {
      key: "claude-tasks",
      items: [{ id: "1", text: "Write tests", status: "pending" as const }],
    };

    bindHarnessSession("claude", "s1", "sess_1", "/repo", "work", [
      { id: "b1", role: "user", text: "go" },
      { id: "b2", role: "tasks", text: "Write tests", taskList },
    ]);
    bindHarnessSession("claude", "s2", "sess_2", "/repo");

    expect(bindSession).toHaveBeenCalledWith("s1", "sess_1", "/repo", "work");
    expect(bindSession).toHaveBeenCalledWith(
      "s2",
      "sess_2",
      "/repo",
      undefined,
    );
    expect(restoreTaskLists).toHaveBeenCalledTimes(1);
    expect(restoreTaskLists).toHaveBeenCalledWith("s1", [taskList]);
  });
});
