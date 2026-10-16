// @vitest-environment happy-dom
import { beforeEach, expect, it, vi } from "vitest";
import { __monosTestReset, loadMonoRoster } from "./mono";
import { clearHabitRun, markHabitRun } from "./monoHabits";
import { monoToolsFor } from "./monoTools";

const tools = [
  {
    name: "get_monos",
    action: "messages.get_monos",
    description: "List Monos",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
];

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke, isTauri: () => true }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

beforeEach(async () => {
  __monosTestReset();
  clearHabitRun("habit-run");
  invoke.mockReset();
  invoke.mockImplementation(async (command: string) => {
    if (command === "monos_list")
      return {
        revision: 1,
        monos: [
          {
            id: "mono-1",
            role: "mono",
            data: { sessionId: "mono-chat", mascot: "crab", color: "red", projects: [] },
          },
        ],
      };
    if (command === "app_cli_path") return "/Applications/MonoCode.app/monocode";
    if (command === "app_tool_schemas") return tools;
    if (command === "control_host_call") return { ok: true, monos: [] };
    throw new Error(`unexpected ${command}`);
  });
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
  });
  await loadMonoRoster();
});

// Defect: a project session gets the messaging tools, a Mono chat or habit
// run on a proven harness does not, or a harness whose smoke has not passed
// (or that scrubs the token) is registered anyway.
it("gives the tools to a Mono chat or habit run on claude, codex and omp only", async () => {
  markHabitRun("habit-run", "mono-1");
  for (const harness of ["claude", "codex", "omp"] as const) {
    expect(await monoToolsFor(harness, "mono-chat")).toMatchObject({
      exe: "/Applications/MonoCode.app/monocode",
      tools,
    });
    expect(await monoToolsFor(harness, "habit-run")).toBeDefined();
    expect(await monoToolsFor(harness, "project-session")).toBeUndefined();
  }
  for (const harness of [
    "cursor",
    "devin",
    "grok",
    "fx",
    "antigravity",
    "hermes",
    "pi",
    "opencode",
  ] as const) {
    expect(await monoToolsFor(harness, "mono-chat")).toBeUndefined();
  }
});

// Defect: an omp host tool call runs for another session than the one the
// tools were registered for.
it("runs a host tool call for the session it was registered for", async () => {
  const registered = await monoToolsFor("omp", "mono-chat");
  await registered!.call("messages.get_monos", {}, "req-1");
  expect(invoke).toHaveBeenCalledWith("control_host_call", {
    sessionId: "mono-chat",
    action: "messages.get_monos",
    input: {},
    requestId: "req-1",
  });
});
