import { invoke } from "@tauri-apps/api/core";
import { writeFileSync } from "node:fs";
import { beforeEach, expect, it, vi } from "vitest";
import * as approvals from "../../approvals/model/approvals";
import { rememberProject } from "../../projects/model/recents";
import { newSession } from "../../sessions/model/session";
import fixture from "./approvalsAgentCalls.fixtures.json";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import type { AppErrorCode } from "./appActionError";

// approvalsAgentCalls.fixtures.json is generated, never edited by hand. With
// WRITE_APPROVALS_FIXTURE=1, run this file (it writes the steps and args),
// then approvals_commands_answer_every_call_the_web_app_sends (the replies).

const WRITE = process.env.WRITE_APPROVALS_FIXTURE === "1";
const PROJECT = "/code/app";
const CARD_ID = "00000000-0000-4000-8000-000000000001";

type Step = {
  name: string;
  caller: "giga-a" | "giga-b";
  action: string;
  input: Record<string, unknown>;
  expect: "ok" | AppErrorCode;
};

const CARD = {
  heading: "Pick a store",
  summary: "Where approval cards live.",
  tradeoffs: { pros: ["Local and fast"], cons: ["One machine"] },
  options: [
    { label: "SQLite", description: "One file next to the app.", recommended: true },
    { label: "Server" },
  ],
  urgency: "blocking",
  links: [{ kind: "pr", title: "PR #1", url: "https://github.com/tylers2222/monocode/pull/1" }],
  origin: "MonoCode Fork Orchestrator",
};
const FILE = { project: PROJECT, ...CARD };
const FORGED = {
  requested_by: "giga-b",
  requested_by_name: "Giga B",
  id: CARD_ID,
  number: 7,
  created_at: 1,
  status: "approved",
};

const STEPS: Step[] = [
  { name: "1: giga-a files a card with two options and a link", caller: "giga-a", action: "approvals.file", input: FILE, expect: "ok" },
  { name: "2: giga-a files the same heading with padding and a new summary", caller: "giga-a", action: "approvals.file", input: { ...FILE, heading: "  Pick a store ", summary: "Now with costs." }, expect: "ok" },
  { name: "3: giga-a updates the card with a new heading", caller: "giga-a", action: "approvals.update", input: { id: CARD_ID, ...CARD, heading: "Pick a card store" }, expect: "ok" },
  { name: "4: giga-b updates giga-a's card", caller: "giga-b", action: "approvals.update", input: { id: CARD_ID, ...CARD }, expect: "not_your_approval" },
  { name: "5: giga-b withdraws giga-a's card", caller: "giga-b", action: "approvals.withdraw", input: { id: CARD_ID }, expect: "not_your_approval" },
  { name: "6: giga-b files giga-a's heading", caller: "giga-b", action: "approvals.file", input: { ...FILE, heading: "Pick a card store" }, expect: "heading_taken" },
  ...Object.entries(FORGED).map(([field, value], index): Step => ({
    name: `${7 + index}: giga-a files with a forged ${field}`,
    caller: "giga-a",
    action: "approvals.file",
    input: { ...FILE, [field]: value },
    expect: "invalid_input",
  })),
  { name: "13: giga-a files in a project that is not on the rail", caller: "giga-a", action: "approvals.file", input: { ...FILE, project: "/code/elsewhere" }, expect: "invalid_input" },
  { name: "14: giga-a files an option with an unknown key", caller: "giga-a", action: "approvals.file", input: { ...FILE, options: [{ label: "SQLite", icon: "db" }] }, expect: "invalid_input" },
  { name: "15: giga-a files a blank heading", caller: "giga-a", action: "approvals.file", input: { ...FILE, heading: "  " }, expect: "invalid_input" },
  { name: "16: giga-a withdraws its card", caller: "giga-a", action: "approvals.withdraw", input: { id: CARD_ID }, expect: "ok" },
  { name: "17: giga-a updates its withdrawn card", caller: "giga-a", action: "approvals.update", input: { id: CARD_ID, ...CARD }, expect: "approval_not_open" },
  { name: "18: giga-a withdraws an unknown id", caller: "giga-a", action: "approvals.withdraw", input: { id: "ffffffff-ffff-4fff-8fff-ffffffffffff" }, expect: "unknown_approval" },
];

type Call = {
  command: string;
  args: Record<string, unknown>;
  reply: { Ok: unknown } | { Err: string };
};
type Case = Step & { calls: Call[] };
const cases = fixture.steps as Case[];

const rust = vi.hoisted(() => ({
  reply: undefined as ((command: string) => unknown) | undefined,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) => {
    const reply = rust.reply!(command) as Call["reply"];
    if ("Err" in reply) throw reply.Err;
    return reply.Ok;
  }),
}));

beforeEach(() => {
  vi.mocked(invoke).mockClear();
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
  rememberProject(PROJECT);
});

function run(step: Step) {
  const source = {
    ...newSession("codex", "/Users/me", "codex:test"),
    id: `${step.caller}-chat`,
  };
  const host = {
    levels: {
      check: async () => ({ leadSessionId: null }),
      recordStart: async () => "m_aaaaaaaaaaaa",
    },
    isMono: (id: string) => id === source.id,
    monoOf: (id: string) =>
      id === source.id ? { id: step.caller, projects: [], giga: true } : undefined,
    approvals,
  } as unknown as AgentAppHost;
  return handleAgentApp(source, "req-1", step.action, step.input, host).then(
    (value) => ({ value }),
    (error: unknown) => ({ error }),
  );
}

function replay(entry: Case) {
  const replies = entry.calls.map((call) => call.reply);
  rust.reply = () => replies.shift();
  return run(entry);
}

const ipcCalls = () =>
  vi.mocked(invoke).mock.calls.map(([command, args]) => [command, args]);

/** JSON with sorted keys, as serde_json writes it, so both writers agree byte for byte. */
function sortedJson(value: unknown): string {
  const sorted = JSON.stringify(
    value,
    (_key, node: unknown) =>
      node && typeof node === "object" && !Array.isArray(node)
        ? Object.fromEntries(
            Object.entries(node).sort(([a], [b]) => (a < b ? -1 : 1)),
          )
        : node,
    2,
  );
  return `${sorted}\n`;
}

it.runIf(WRITE)("writes the fixture from the real calls", async () => {
  const written: Case[] = [];
  for (const step of STEPS) {
    vi.mocked(invoke).mockClear();
    const kept = cases.find((entry) => entry.name === step.name)?.calls ?? [];
    const answered: Call["reply"][] = [];
    rust.reply = (command) => {
      const at = kept[answered.length];
      answered.push(at?.command === command ? at.reply : { Ok: null });
      return answered[answered.length - 1];
    };
    await run(step);
    const calls = ipcCalls().map(([command, args], index) => ({
      command: command as string,
      args: args as Call["args"],
      reply: answered[index],
    }));
    written.push({ ...step, calls });
  }
  writeFileSync(
    `${process.cwd()}/src/features/agent-app/model/approvalsAgentCalls.fixtures.json`,
    sortedJson({ steps: written }),
  );
});

// Defect: a step or its hand-written outcome is missing from the fixture,
// so Rust never replays it.
it.skipIf(WRITE)("holds every step in the fixture", () => {
  expect(cases).toEqual(
    STEPS.map((step) => ({ ...step, calls: expect.any(Array) })),
  );
});

// Defect: the handler sends another command, argument name, card shape,
// project path or filer than approvals.rs reads; a forged app-set field, a
// project that is not on the rail or an unknown nested key reaches Rust; or
// the answer loses Rust's card or refusal code.
it.skipIf(WRITE).each(cases.map((entry) => [entry.name, entry] as const))(
  "%s",
  async (_name, entry) => {
    const outcome = await replay(entry);
    expect(ipcCalls()).toEqual(
      entry.calls.map(({ command, args }) => [command, args]),
    );
    if (entry.expect === "ok")
      expect(outcome).toEqual({
        value: (entry.calls[0].reply as { Ok: unknown }).Ok,
      });
    else
      expect(outcome).toEqual({
        error: expect.objectContaining({ code: entry.expect }),
      });
  },
);

// Defect: filing an open heading again makes a second card, or a withdraw
// answers a card that still reads as open.
it.skipIf(WRITE)("refiles the first card and withdraws it", async () => {
  const card = async (step: number) => {
    const entry = cases.find(({ name }) => name.startsWith(`${step}: `))!;
    const outcome = await replay(entry);
    if ("error" in outcome) throw outcome.error;
    return outcome.value as approvals.Approval;
  };
  expect((await card(2)).id).toBe((await card(1)).id);
  expect((await card(16)).status).toEqual({ state: "withdrawn", withdrawnAt: 16 });
});
