import { expect, it, vi } from "vitest";
import type { MonoTools } from "./monoTools";
import {
  compactHarnessContext,
  registerHarness,
  rewindHarnessLastTurn,
  sendHarnessTurn,
} from "./registry";
import type { HarnessSessionInput } from "./types";

const registration: MonoTools = {
  exe: "/fake/monocode",
  tools: [],
  call: async () => ({ ok: true }),
};
const monoToolsFor = vi.hoisted(() =>
  vi.fn(async (_harness: string, sessionId: string, appAccess: boolean) =>
    sessionId === "mono-chat" || appAccess ? registration : undefined,
  ),
);
vi.mock("../../../features/monos/model/monoTools", () => ({ monoToolsFor }));


// Defect: a turn, a compaction or an edit spawns the session's harness
// without its Mono tools, so the process lacks them for its whole life; or
// (design 15 C7) a session with app access never gets them.
it("hands every spawning operation the session's Mono tools", async () => {
  const seen: Array<HarnessSessionInput["monoTools"]> = [];
  const record = async (input: HarnessSessionInput) => {
    seen.push(input.monoTools);
  };
  registerHarness({
    id: "claude",
    live: true,
    sendTurn: record,
    compactContext: record,
    rewindLastTurn: async (input) => {
      seen.push(input.monoTools);
      return { text: "" };
    },
    async steerTurn() {},
    async cancelTurn() {},
    respondApproval() {},
    async stopSession() {},
    async forgetSession() {},
    bindSession() {},
  });
  const base = {
    harness: "claude" as const,
    cwd: "/repo",
    model: "claude:default",
    runtimeMode: "supervised" as const,
    onEvent: () => {},
  };
  await sendHarnessTurn({ ...base, sessionId: "mono-chat", text: "hi" });
  await compactHarnessContext({ ...base, sessionId: "mono-chat" });
  await rewindHarnessLastTurn({ ...base, sessionId: "mono-chat" });
  const lead = { ...base, sessionId: "lead", appAccess: true };
  await sendHarnessTurn({ ...lead, text: "hi" });
  await compactHarnessContext(lead);
  await rewindHarnessLastTurn(lead);
  await sendHarnessTurn({ ...base, sessionId: "project", text: "hi" });
  expect(seen).toEqual([
    ...Array(6).fill(registration),
    undefined,
  ]);
  expect(monoToolsFor).toHaveBeenCalledWith("claude", "mono-chat", false);
});
