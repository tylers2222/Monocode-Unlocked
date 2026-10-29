// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

import { createMono, findMono, HANDED_KEY, updateMono } from "./mono";
import {
  defaultSoul,
  loadMonoFiles,
  MEMORY_MAX_LINES,
  memoryWithinBudget,
  planAgentContext,
  monoContext,
  monoTurn,
  recordAgentContext,
  resetMonoDefaults,
} from "./monoFiles";

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
  });
  invoke.mockReset();
});
afterEach(() => vi.unstubAllGlobals());

const files = (soulHash = "s1", memoryHash = "m1") => ({
  id: "agent",
  dir: "/data/agents/agent",
  soul: "# Soul\n\nBe brief.",
  soulHash,
  memory: "- 2026-10-04 · releases are tag-triggered",
  memoryHash,
  memoryPath: "/data/agents/agent/MEMORY.md",
  topics: ["releases"],
});

it("hands a new native session everything, then nothing it already has", () => {
  expect(planAgentContext("chat", undefined, files())).toEqual({
    soul: true,
    memory: true,
  });
  // The first turn is recorded before its reply names the provider session.
  recordAgentContext("chat", undefined, files());
  expect(planAgentContext("chat", "native-1", files())).toEqual({
    soul: false,
    memory: false,
  });
  recordAgentContext("chat", "native-1", files());
  expect(planAgentContext("chat", "native-1", files())).toEqual({
    soul: false,
    memory: false,
  });
});

it("resends memory when it changed, and everything for a new session", () => {
  recordAgentContext("chat", "native-1", files());
  expect(planAgentContext("chat", "native-1", files("s1", "m2"))).toEqual({
    soul: false,
    memory: true,
  });
  expect(planAgentContext("chat", "native-1", files("s2", "m1"))).toEqual({
    soul: true,
    memory: true,
  });
  // A model switch or a rotated session starts a different native session.
  expect(planAgentContext("chat", "native-2", files())).toEqual({
    soul: true,
    memory: true,
  });
});

it("refreshes standing rules in an existing conversation after a context upgrade", () => {
  localStorage.setItem(
    HANDED_KEY,
    JSON.stringify({
      chat: { provider: "native-1", soulHash: "s1:v6", memoryHash: "m1" },
    }),
  );
  expect(planAgentContext("chat", "native-1", files())).toEqual({
    soul: true,
    memory: true,
  });
  recordAgentContext("chat", "native-1", files());
  expect(planAgentContext("chat", "native-1", files())).toEqual({
    soul: false,
    memory: false,
  });
});

it("loads only whole lines within the memory budget", () => {
  const memory = Array.from(
    { length: MEMORY_MAX_LINES + 5 },
    (_, i) => `- fact ${i}`,
  ).join("\n");
  const budget = memoryWithinBudget(memory);
  expect(budget.lines).toBe(MEMORY_MAX_LINES + 5);
  expect(budget.droppedLines).toBe(5);
  expect(budget.text.split("\n")).toHaveLength(MEMORY_MAX_LINES);
  expect(memoryWithinBudget("").lines).toBe(0);
});

it("says how to keep memory and update the soul only at the user's request", () => {
  const look = {
    name: "Skull",
    mascot: "skull",
    color: "#fff",
    projects: [
      { path: "/code/monocode", name: "monocode" },
      { path: "/code/site", name: "site" },
    ],
  };
  const full = monoContext(look, files(), { soul: true, memory: true });
  expect(full).toContain("You are Skull, a Mono in MonoCode");
  expect(full).toContain("these 2 projects (monocode and site)");
  expect(full).toContain("- site: /code/site");
  expect(full).toContain("Be brief.");
  expect(full).toContain(
    "Only when the user asks you to change your soul or standing instructions",
  );
  expect(full).toContain("app soul.read {}");
  expect(full).toContain(
    'app soul.update {"text":"<complete updated Markdown>","expectedHash":"<hash from soul.read>"}',
  );
  expect(full).toContain("preserving the other instructions");
  expect(full).toContain(
    "Never change your soul on your own or edit its files directly",
  );
  expect(full).not.toContain("never edit that file yourself");
  expect(full).toContain("app memory.add");
  expect(full).toContain('app artifacts.write {"kind":"document"');
  expect(full).toContain("Artifacts are separate from the user's Notes");
  expect(full).toContain("without repeating its contents");
  expect(full).not.toContain("app documents.write");
  expect(full).toContain('"notifyOnComplete":true');
  expect(full).toContain('"whenBusy":"steer"');
  expect(full).toContain('"whenBusy":"followUp"');
  const rules = (tag: string) =>
    full.match(new RegExp(`<${tag}>[\\s\\S]*?</${tag}>`))?.[0] ?? "";
  // A Mono learns every messaging tool and field it can use, the notice it
  // can get, the inference line that can start a message (design 16.10 I4
  // row), and not the removed tool. R14: one waiting question at a time, an
  // answer by the asker with the question's seq, one update per ask, asking
  // up only with an open ask of your own, and all eleven tool names.
  const messaging = rules("mono_messaging_rules");
  for (const tool of [
    "messages.get_monos",
    "messages.send",
    "messages.reply",
    'messages.get {"handle"',
    "get_message",
    "get_monos",
    "send_message",
    "wanting_reply",
    '"intent":"new"',
    '"intent":"steer"',
    '"reply_to"',
    '"scope":"turn"',
    '"to"',
    "<mono_message>",
    "<mono_notice>",
    '[steer → #1 · "',
    "[steer → whole turn]",
    "reply_to attribute",
    'messages.question {"ask_id"',
    "up to 3 times per ask and one at a time",
    'messages.answer {"ask_id":"<question_of>","seq"',
    "question_of and seq",
    'Once per ask, app messages.update {"ask_id"',
    "you have an open ask of your own, ask your own asker with messages.question",
    "otherwise answer from what you know, or ask the user",
    "your asker then gets it at once as one <mono_reminder> instead",
    "get_monos, send_message, reply, get_message, ask, ack, question, answer, update, extend and reason",
  ])
    expect(messaging).toContain(tool);
  expect(full).not.toContain("monos.send");
  expect(messaging).not.toContain('"action_type":');
  const delegation = rules("session_delegation_rules");
  expect(delegation).toMatch(/pass harness and model to sessions\.start/);
  expect(delegation).toContain("models.list");
  expect(full).toContain(
    "Submitted sessions notify you on completion by default",
  );
  expect(full).toContain('set "notifyOnComplete":false on sessions.start');
  expect(full).toContain("within 60 seconds after");
  expect(full).toContain("sends its own result when it stops");
  expect(full).toContain("give one consolidated report");
  expect(full).toContain("acknowledge the action in your current reply");
  expect(full).toContain("dismisses your pending report for that session");
  expect(full).toContain("do not generate a later completion notification");
  expect(full).toContain(
    "Sessions launched during later turns form separate groups",
  );
  expect(full).toContain(
    "Topic notes, read on demand with memory.read: releases",
  );
  expect(full).toContain("releases are tag-triggered");
  const memoryOnly = monoContext(look, files(), {
    soul: false,
    memory: true,
  });
  expect(memoryOnly).not.toContain("<soul>");
  expect(memoryOnly).toContain("<memory>");
  const alone = monoContext({ ...look, projects: [] }, files(), {
    soul: true,
    memory: false,
  });
  expect(alone).toContain("has not given you any projects yet");
});

it("seeds SOUL.md once, moving the old instructions into it", async () => {
  const { id } = createMono(["/code/app"]);
  updateMono(id, (mono) => ({ ...mono, instructions: "Ship on Fridays" }));
  invoke.mockImplementation(async (command: string) =>
    command === "mono_load" ? { ...files("empty"), soul: null } : "seeded",
  );
  const [first, second] = await Promise.all([
    loadMonoFiles(id),
    loadMonoFiles(id),
  ]);
  expect(first).toBe(second);
  expect(first.soul).toBe(defaultSoul("Ship on Fridays"));
  expect(first.soulHash).toBe("seeded");
  const saves = invoke.mock.calls.filter(
    ([command]) => command === "mono_save",
  );
  expect(saves).toHaveLength(1);
  expect(saves[0][1]).toMatchObject({
    mono: id,
    path: "SOUL.md",
    expectedHash: "empty",
  });
  expect(findMono(id)?.instructions).toBeUndefined();
});

it("resets a Mono to the default soul and name, over the soul it loaded", async () => {
  const { id } = createMono();
  updateMono(id, (mono) => ({ ...mono, name: "Broski" }));
  invoke.mockImplementation(async (command: string) =>
    command === "mono_load"
      ? { ...files("mine"), soul: "# Soul\n\nBe a pirate." }
      : "reset",
  );
  await resetMonoDefaults(id);
  const saves = invoke.mock.calls.filter(
    ([command]) => command === "mono_save",
  );
  expect(saves).toHaveLength(1);
  expect(saves[0][1]).toMatchObject({
    path: "SOUL.md",
    text: defaultSoul(),
    expectedHash: "mine",
  });
  expect(findMono(id)?.name).toBeUndefined();
});

it("reads a migrated Mono's old project folder once, then its own", async () => {
  const { id } = createMono(["/code/app"]);
  updateMono(id, (mono) => ({ ...mono, legacyProject: "/code/app" }));
  invoke.mockImplementation(async () => ({ ...files(), soul: "# Soul" }));
  await loadMonoFiles(id);
  expect(invoke.mock.calls[0]).toEqual([
    "mono_load",
    { mono: id, legacyProject: "/code/app" },
  ]);
  expect(findMono(id)?.legacyProject).toBeUndefined();
  await loadMonoFiles(id);
  expect(invoke.mock.calls[1]).toEqual([
    "mono_load",
    { mono: id, legacyProject: null },
  ]);
});

it("puts what the app adds ahead of the user's message, marked as its own", () => {
  expect(monoTurn("what is this", [])).toBe("what is this");
  const turn = monoTurn("what is this", ["<mono>\nYou are Cat.\n</mono>"]);
  expect(turn.startsWith("<monocode_context>")).toBe(true);
  expect(turn).toContain("they did not write it");
  expect(turn.indexOf("You are Cat.")).toBeLessThan(
    turn.indexOf("what is this"),
  );
  expect(turn.endsWith("</monocode_context>\n\nwhat is this")).toBe(true);
});

it("tells only a Giga Mono how to create and update Monos", () => {
  const look = { name: "Boss", mascot: "skull", color: "#fff", projects: [] };
  const soul = { soul: true, memory: false };
  expect(monoContext(look, files(), soul)).not.toContain("<giga_mono_rules>");
  const giga = monoContext({ ...look, giga: true }, files(), soul);
  expect(giga).toContain("<giga_mono_rules>");
  // Defect: a Giga is not told where it may write, or how to search first.
  expect(giga).not.toContain("<giga_workspace_rules>");
  const placed = monoContext(
    { ...look, giga: true, gigaSlug: "ops" },
    files(),
    soul,
  );
  expect(placed).toContain("Write only inside ~/MonoCode/gigas/ops/");
  expect(placed).toContain("sqlite3 ~/MonoCode/index/index.db");
  expect(
    monoContext({ ...look, gigaSlug: "ops" }, files(), soul),
  ).not.toContain("<giga_workspace_rules>");
  // Defect: a Giga is told an action the CLI no longer has.
  for (const action of [
    "monos.create",
    "monos.get",
    "monos.update",
    "models.list",
  ])
    expect(giga).toContain(`app ${action}`);
  expect(giga).not.toContain("monos.configure");
  // Defect: a Giga is not told how to file an approval card, or a plain Mono
  // is told of an action only a Giga may call.
  expect(giga).toContain("app approvals.file");
  expect(monoContext(look, files(), soul)).not.toContain("approvals.");
  // Rules go out with the soul, not on every turn.
  expect(
    monoContext({ ...look, giga: true }, files(), {
      soul: false,
      memory: true,
    }),
  ).not.toContain("<giga_mono_rules>");
});

// Design 6.2 and S8, S18, S19. Defect: a Mono is not told how it may reach
// a Giga, another Mono, a lead or a worker, or the limits, so it learns them
// only from refusals; a Giga is told the upward rule, which never binds it, or not
// the peer rule, which does; or a Giga is never told the monos.update
// field that sets an allowlist.
it("tells a Mono the upward and peer rules, every Mono the limits, and a Giga the allowlist field", () => {
  const look = { name: "Scout", mascot: "skull", color: "#fff", projects: [] };
  const soul = { soul: true, memory: false };
  const rules = (text: string, tag: string) =>
    text.match(new RegExp(`<${tag}>[\\s\\S]*?</${tag}>`))?.[0] ?? "";
  const mono = rules(monoContext(look, files(), soul), "mono_messaging_rules");
  const gigaContext = monoContext({ ...look, giga: true }, files(), soul);
  const giga = rules(gigaContext, "mono_messaging_rules");
  // 6 messages per recipient per 10 minutes, 4 deep, 8000 characters.
  for (const limit of ["6", "10", "4", "8000"]) {
    expect(mono).toMatch(new RegExp(`\\b${limit}\\b`));
    expect(giga).toMatch(new RegExp(`\\b${limit}\\b`));
  }
  expect(mono).toContain(
    "Message a Giga only while you hold an open ask from it",
  );
  expect(giga).not.toContain("Message a Giga only while");
  expect(mono).toContain("Another regular Mono must share a project");
  expect(giga).toContain("Another Giga must share a project");
  // Design 13.3 and W2 (review r1 m1): every Mono learns that only a
  // worker's lead reaches it, and a regular Mono that it may steer a lead's
  // whole turn.
  expect(mono).toContain("Message a lead, never a worker");
  expect(giga).toContain("Message a lead, never a worker");
  expect(mono).toContain("a regular Mono steering a lead session");
  expect(rules(gigaContext, "giga_mono_rules")).toContain('"messageAllowlist"');
});
