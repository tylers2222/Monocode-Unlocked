// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { writeFileSync } from "node:fs";
import { beforeEach, expect, it, vi } from "vitest";
import { __monosTestReset, loadMonoRoster } from "../../monos/model/mono";
import { markHabitRun } from "../../monos/model/monoHabits";
import { levelGate, type ActionChecked } from "../../monos/model/monoCaller";
import { setHarnessModels } from "../../sessions/model/models";
import { newSession, type Session } from "../../sessions/model/session";
import fixture from "./appActionCheck.fixtures.json";
import {
  FIELDS,
  handleAgentApp,
  type AgentAppHost,
  type StartAsk,
} from "./agentApp";
import { AppActionError } from "./appActionError";

// appActionCheck.fixtures.json is the contract with Rust, and it is
// generated, never edited by hand:
//   1. WRITE_APP_ACTION_FIXTURE=1 npx vitest run <this file> writes WORLD,
//      SCENARIOS and the `args` the real handleAgentApp and levelGate send
//      for each scenario (a new scenario gets an allowed `reply`).
//   2. WRITE_APP_ACTION_FIXTURE=1 cargo test --lib
//      mono_app_action_check_answers_every_call_the_web_app_sends writes
//      the `reply` the real mono_app_action_check answers to each `args`.
// Without the variable both tests compare instead: this file fails when the
// scenarios or the real calls differ from the file, Rust when its answers do.

const WRITE = process.env.WRITE_APP_ACTION_FIXTURE === "1";
const PROJECT = "/code/app";
const ALLOWED = { outcome: "allowed", leadSessionId: null, ask: null };

/** The sessions and Monos both sides set up: `saved` ones have a `sessions` row in Rust. */
const WORLD = vi.hoisted(() => ({
  sessions: [
    { id: "a-chat", cwd: "/Users/me", leadSessionId: null, saved: true },
    { id: "lead", cwd: "/code/app", leadSessionId: null, saved: true },
    { id: "worker", cwd: "/code/app", leadSessionId: "lead", saved: true },
    { id: "g-chat", cwd: "/Users/me", leadSessionId: null, saved: true },
    { id: "g-run", cwd: "/Users/me", leadSessionId: null, saved: true },
  ],
  monos: [
    { id: "mono-a", role: "mono", sessionId: "a-chat", projects: ["/code/app"] },
    { id: "giga-g", role: "giga", sessionId: "g-chat", projects: [] },
  ],
}));

type Scenario = {
  name: string;
  caller: string;
  action: string;
  input: Record<string, unknown>;
  requestId: string;
  /** The host method an allowed call reaches. */
  ran: "start" | "send" | "stop" | "approvalWithdraw" | null;
  /** The agent's next calls under the same request id, in order. */
  then?: { action: string; input: Record<string, unknown> }[];
};

const CHOICE = { harness: "codex", model: "codex:test", notifyOnComplete: false };
const SCENARIOS: Scenario[] = [
  { name: "A6: a lead's start makes a worker of it", caller: "lead", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" }, requestId: "a6-start", ran: "start" },
  { name: "A7: a Mono's start makes a lead", caller: "a-chat", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" }, requestId: "a7-start", ran: "start" },
  { name: "A7: a Mono's repeated start answers its first row", caller: "a-chat", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" }, requestId: "a7-again", ran: "start", then: [{ action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" } }] },
  { name: "R7: a Mono's start sets its ask's due_minutes", caller: "a-chat", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test", due_minutes: 30 }, requestId: "r7-due", ran: "start" },
  { name: "R7: a start's due_minutes out of range is refused before the launch", caller: "a-chat", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test", due_minutes: 5 }, requestId: "r7-due-5", ran: null },
  { name: "A1: a refused start again answers its first refusal", caller: "worker", action: "sessions.start", input: { ...CHOICE, prompt: "Spawn a helper" }, requestId: "a1-refused", ran: null, then: [{ action: "sessions.start", input: { ...CHOICE, prompt: "Spawn a helper" } }] },
  { name: "A2: a lead's start again with other input is refused before the launch", caller: "lead", action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" }, requestId: "a2-other", ran: "start", then: [{ action: "sessions.start", input: { ...CHOICE, prompt: "Fix the tests" } }] },
  { name: "A2: a Mono's start under the request id of its refused send is refused", caller: "a-chat", action: "sessions.send", input: { sessionId: "worker", prompt: "Stop" }, requestId: "a2-send", ran: null, then: [{ action: "sessions.start", input: { ...CHOICE, prompt: "Fix the flaky test" } }] },
  { name: "A5: a lead sends into its own worker", caller: "lead", action: "sessions.send", input: { sessionId: "worker", prompt: "Rebase first" }, requestId: "a5-own", ran: "send" },
  { name: "A5: a Mono may stop a worker", caller: "a-chat", action: "sessions.stop", input: { sessionId: "worker" }, requestId: "a5-stop", ran: "stop" },
  { name: "A5: a Mono's send into a worker, padded id parsed once", caller: "a-chat", action: "sessions.send", input: { sessionId: " worker\n", prompt: "Stop" }, requestId: "a5-send", ran: null },
  { name: "A5: a Mono's draft into a worker", caller: "a-chat", action: "sessions.draft", input: { sessionId: "worker", prompt: "Draft this" }, requestId: "a5-draft", ran: null },
  { name: "Giga row: a Giga's approvals.withdraw", caller: "g-chat", action: "approvals.withdraw", input: { id: "00000000-0000-4000-8000-000000000001" }, requestId: "g-withdraw", ran: "approvalWithdraw" },
  { name: "Giga row: a Giga habit run's approvals.withdraw", caller: "g-run", action: "approvals.withdraw", input: { id: "00000000-0000-4000-8000-000000000001" }, requestId: "g-run-withdraw", ran: "approvalWithdraw" },
  { name: "Giga row: a Mono's approvals.file", caller: "a-chat", action: "approvals.file", input: { project: PROJECT, heading: "Pick a store" }, requestId: "m-file", ran: null },
  { name: "Giga row: a lead's approvals.withdraw", caller: "lead", action: "approvals.withdraw", input: { id: "00000000-0000-4000-8000-000000000001" }, requestId: "l-withdraw", ran: null },
  // A3 and A4: a worker's call of every action the gate checks.
  ...[...FIELDS]
    .filter(([action]) => !action.startsWith("messages."))
    .map(([action, fields]): Scenario => ({
      name: `${action.startsWith("approvals.") ? "Giga row" : action === "sessions.start" ? "A3" : "A4"}: a worker's ${action}`,
      caller: "worker",
      action,
      input:
        fields.includes("sessionId") && action !== "chat.card"
          ? { sessionId: "lead" }
          : {},
      requestId: `w-${action.replaceAll(".", "-")}`,
      ran: null,
    })),
];

/** One IPC call a scenario made, and the reply Rust gave it. */
type Call = {
  command: "mono_app_action_check" | "mono_message_record_start";
  args: { call: { requestId: string } };
  reply: unknown;
};
type Case = Scenario & { calls: Call[] };
const cases = fixture.cases as Case[];
const ANSWERS = {
  mono_app_action_check: ALLOWED,
  mono_message_record_start: { outcome: "recorded", messageId: "m_aaaaaaaaaaaa" },
};

const command = vi.hoisted(() => ({
  answer: undefined as
    | ((name: string, args: { call: { requestId: string } }) => unknown)
    | undefined,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (name: string, args: { call: { requestId: string } }) => {
    if (name === "monos_list")
      return {
        revision: 1,
        monos: WORLD.monos.map((mono) => ({
          id: mono.id,
          role: mono.role,
          data: {
            sessionId: mono.sessionId,
            mascot: "crab",
            color: "red",
            projects: mono.projects,
          },
        })),
      };
    if (name === "mono_app_action_check" || name === "mono_message_record_start")
      return command.answer!(name, args);
    throw new Error(`unexpected ${name}`);
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex",
}));

beforeEach(async () => {
  vi.mocked(invoke).mockClear();
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
  });
  setHarnessModels("codex", [
    { id: "codex:test", harness: "codex", name: "Test model" },
  ]);
  __monosTestReset();
  markHabitRun("g-run", "giga-g");
  await loadMonoRoster();
  command.answer = (name) => ANSWERS[name as Call["command"]];
});

/** WORLD's sessions as this window has them; lead and worker have /operator access. */
function world(caller: string) {
  const sessions = new Map<string, Session>(
    WORLD.sessions.map((entry) => [
      entry.id,
      {
        ...newSession("codex", entry.cwd, "codex:test"),
        id: entry.id,
        ...(entry.leadSessionId ? { leadSessionId: entry.leadSessionId } : {}),
        blocks:
          entry.id === "a-chat"
            ? []
            : [{ id: "u1", role: "user", text: "Fix CI", monocode: true }],
      },
    ]),
  );
  const source = sessions.get(caller)!;
  const listing = (session: Session) => ({
    id: session.id,
    title: session.title,
    harness: session.harness,
    model: session.model,
    busy: false,
    hasDraft: false,
    archived: false,
  });
  const actions = {
    start: vi.fn(async () => {}),
    send: vi.fn(async () => ({ alreadySubmitted: false, delivery: "sent" })),
    draft: vi.fn(async () => ({ alreadySaved: false, draft: true })),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    postCard: vi.fn(),
    notes: vi.fn(async () => []),
    saveNote: vi.fn(async () => ({})),
    createWorktree: vi.fn(async () => ({})),
    messageSend: vi.fn(async () => ({ message_id: "m_bbbbbbbbbbbb" })),
    approvalFile: vi.fn(async () => ({})),
    approvalUpdate: vi.fn(async () => ({})),
    approvalWithdraw: vi.fn(async () => ({})),
  };
  const host = {
    ...actions,
    sessions: vi.fn(async (cwd: string) =>
      [...sessions.values()]
        .filter((session) => session.cwd === cwd)
        .map(listing),
    ),
    session: vi.fn(async (id: string) => sessions.get(id) ?? null),
    worktrees: vi.fn(async () => ({ worktrees: [], defaultRoot: "/tmp" })),
    isMono: (id: string) => id === "a-chat" || id === "g-chat",
    isHabitRun: (id: string) => id === "g-run",
    monoOf: (id: string) =>
      id === "a-chat"
        ? { id: "mono-a", projects: [PROJECT] }
        : id === "g-chat" || id === "g-run"
          ? { id: "giga-g", projects: [], giga: true }
          : undefined,
    messages: {
      getMonos: async () => ({ monos: [] }),
      send: actions.messageSend,
      reply: async () => ({}),
      get: async () => ({}),
    },
    approvals: {
      file: actions.approvalFile,
      update: actions.approvalUpdate,
      withdraw: actions.approvalWithdraw,
    },
    levels: levelGate(source, "turn-1"),
  } as unknown as AgentAppHost;
  const run = (action: string, input: Record<string, unknown>, requestId = "r1") =>
    handleAgentApp(source, requestId, action, input, host);
  return { run, host, actions, sessions };
}

const checks = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === "mono_app_action_check")
    .map(([, args]) => args);

/** Every level gate call so far, as `[command, args]`. */
const gateCalls = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name in ANSWERS)
    .map(([name, args]) => [name, args]);

/** JSON with sorted keys, as serde_json writes it, so both writers agree byte for byte. */
function sortedJson(value: unknown): string {
  const sort = (node: unknown): unknown =>
    Array.isArray(node)
      ? node.map(sort)
      : node && typeof node === "object"
        ? Object.fromEntries(
            Object.keys(node)
              .sort()
              .map((key) => [key, sort((node as Record<string, unknown>)[key])]),
          )
        : node;
  return `${JSON.stringify(sort(value), null, 2)}\n`;
}

/** Runs a scenario's calls in order, each with the level gate calls it made. */
async function runScenario(scenario: Scenario) {
  const { run, host, actions } = world(scenario.caller);
  const steps: { outcome: { value: unknown } | { error: unknown }; calls: number }[] = [];
  for (const { action, input } of [scenario, ...(scenario.then ?? [])]) {
    const before = gateCalls().length;
    const outcome = await run(action, input, scenario.requestId).then(
      (value) => ({ value }),
      (error: unknown) => ({ error }),
    );
    steps.push({ outcome, calls: gateCalls().length - before });
  }
  return { host, actions, steps };
}

it.runIf(WRITE)("writes the fixture from the real calls", async () => {
  const written: Case[] = [];
  for (const scenario of SCENARIOS) {
    vi.mocked(invoke).mockClear();
    // Each call gets the reply the file holds at its place, so a scenario
    // follows Rust's answers; a new call gets the default, which the Rust
    // write then replaces.
    const kept = cases.find((entry) => entry.name === scenario.name)?.calls;
    const answered: unknown[] = [];
    command.answer = (name) => {
      const at = kept?.[answered.length];
      const reply =
        at?.command === name ? at.reply : ANSWERS[name as Call["command"]];
      answered.push(reply);
      return reply;
    };
    await runScenario(scenario);
    const calls = gateCalls().map(([command, args], index): Call => ({
      command: command as Call["command"],
      args: args as Call["args"],
      reply: answered[index],
    }));
    written.push({ ...scenario, calls });
  }
  writeFileSync(
    `${process.cwd()}/src/features/agent-app/model/appActionCheck.fixtures.json`,
    sortedJson({ ...WORLD, cases: written }),
  );
});

// Drift guard. Defect: a scenario, a world session or a gated action (a new
// FIELDS entry) is missing from the fixture, so Rust never replays it.
it.skipIf(WRITE)("holds every scenario and the world in the fixture", () => {
  expect({ sessions: fixture.sessions, monos: fixture.monos }).toEqual(WORLD);
  expect(cases.map(({ calls: _calls, ...scenario }) => scenario)).toEqual(
    SCENARIOS,
  );
});

/** Fields `keys` of `value`, `undefined` where it has none. */
function fieldsOf(value: unknown, keys: string[]): unknown[] {
  const fields: Record<string, unknown> =
    value && typeof value === "object" ? Object.fromEntries(Object.entries(value)) : {};
  return keys.map((key) => fields[key]);
}

/** R6's ask tag around `prompt`, from WORLD's lead, else from its Mono. */
function askTag(ask: StartAsk, prompt: string, source: Session | null): string {
  const from =
    source?.id === "lead"
      ? `from="${source.title}" session_id="lead" level="lead"`
      : 'from="mono-a" mono_id="mono-a" role="mono"';
  const due = ask.dueInMs === null ? "" : ` due_in="${ask.dueInMs / 60_000}m"`;
  return `<mono_message message_id="${ask.askId}" ${from} wanting_reply="false" ask_id="${ask.askId}"${due}>\n${prompt}\n</mono_message>`;
}

// Design 6.6, 13.3 and 16.2, the Rust contract. Defect: an app action runs
// with no level check, or with a check that names another action, target,
// lead copy, launch id or input than Rust reads; Rust's refusal (A3-A5 and
// the Giga row), its replay (A1) or its request id refusal (A2) does not
// stop the action or loses its code, hint, row id or duplicate flag; the
// web app decides the new session's lead itself instead of taking Rust's
// answer; or a submitted start writes no start row, sends another key or
// input than the check, or answers another message_id than its row's; or
// (R7) the started session's agent does not read the check's ask as R6's
// tag, its transcript does not keep the prompt, or the answer's ask_id is
// not the check's.
it.skipIf(WRITE).each(cases.map((entry) => [entry.name, entry] as const))(
  "%s: sends the fixture's calls and acts on their replies",
  async (_name, entry) => {
    const replies = entry.calls.map((call) => call.reply);
    command.answer = () => replies.shift();
    const { host, actions, steps } = await runScenario(entry);
    expect(gateCalls()).toEqual(
      entry.calls.map(({ command, args }) => [command, args]),
    );
    let next = 0;
    let allowed = 0;
    for (const { outcome, calls } of steps) {
      const [check, record] = entry.calls.slice(next, next + calls);
      next += calls;
      const reply = check.reply as ActionChecked;
      if (reply.outcome === "failed") {
        const { code, hint, ...details } = reply.error;
        expect(outcome).toEqual({
          error: expect.objectContaining({ code, hint, details }),
        });
        continue;
      }
      allowed += 1;
      expect(fieldsOf("value" in outcome ? outcome.value : null, ["message_id", "ask_id"])).toEqual([
        fieldsOf(record?.reply, ["messageId"])[0],
        reply.ask?.askId,
      ]);
      if (entry.action === "sessions.start" && allowed === 1) {
        const launch = vi.mocked(host.start).mock.calls[0][0];
        expect(launch.leadSessionId).toBe(reply.leadSessionId ?? undefined);
        expect(launch.prompt).toBe(entry.input.prompt);
        const source = await host.session(entry.caller);
        expect(launch.firstTurn?.agentText).toBe(
          reply.ask ? askTag(reply.ask, launch.prompt, source) : undefined,
        );
      }
    }
    if (entry.ran) expect(actions[entry.ran]).toHaveBeenCalledTimes(allowed);
    if (!allowed)
      for (const action of Object.values(actions))
        expect(action).not.toHaveBeenCalled();
  },
);

// PR 7 review r1 B1. Defect: a blank id, an id with a space or an id over 256
// characters reaches the level check as no target or as another reading.
it.each([["  "], ["work er"], [` ${"w".repeat(257)}`], [42]])(
  "refuses sessionId %j as invalid_input before the level check",
  async (sessionId) => {
    const { run, actions } = world("a-chat");
    await expect(
      run("sessions.send", { sessionId, prompt: "Hi" }),
    ).rejects.toMatchObject({ code: "invalid_input" });
    expect(checks()).toEqual([]);
    expect(actions.send).not.toHaveBeenCalled();
  },
);

// PR 7 review r1 B1. Defect: the check reads the parsed id while the action
// reads the raw input, so a padded id whose trimmed form is 256 characters
// passes the check and the action refuses it or acts on another reading.
it("sends to a padded id whose trimmed form is 256 characters", async () => {
  const { run, actions, sessions } = world("a-chat");
  const id = "w".repeat(256);
  sessions.set(id, { ...newSession("codex", PROJECT, "codex:test"), id });
  expect(await run("sessions.send", { sessionId: ` ${id}`, prompt: "Hi" })).toMatchObject(
    { sessionId: id, submitted: true },
  );
  expect(checks()).toMatchObject([
    { call: { target: id, input: { sessionId: id } } },
  ]);
  expect(vi.mocked(actions.send).mock.calls[0]?.[0]).toBe(id);
});

// PR 7 review r2 m10. Defect: every chat.card must name a session, so a pr,
// choices or habit card is refused; or a session card's id is not the check's
// target.
it("posts a Mono's cards with and without a sessionId", async () => {
  const { run, actions } = world("a-chat");
  const cards = [
    { type: "pr", repo: "owner/repo", number: 123 },
    { type: "choices", options: ["Review", "Ship"] },
    {
      type: "habit",
      name: "Check CI",
      instructions: "Check CI",
      schedule: { kind: "daily", time: "09:00" },
    },
    { type: "session", sessionId: " worker", note: "See this" },
  ];
  for (const card of cards)
    expect(await run("chat.card", card), card.type).toMatchObject({
      posted: card.type,
    });
  expect(actions.postCard).toHaveBeenCalledTimes(cards.length);
  expect(
    checks().map((args) => (args as { call: { target: unknown } }).call.target),
  ).toEqual([
    null,
    null,
    null,
    "worker",
  ]);
});

// Design 13.5, #87 review r1 n2. Defect: a prompt with U+0000 reaches Rust,
// whose row of it (A3 keeps a start's prompt, A5 a send's or draft's) fails
// the length CHECK, so the refusal is an SQLite error.
it.each([
  ["sessions.start", "worker", { harness: "codex", model: "codex:test" }],
  ["sessions.send", "a-chat", { sessionId: "worker" }],
  ["sessions.draft", "a-chat", { sessionId: "worker" }],
] as const)(
  "refuses a %s prompt with U+0000 as invalid_input before the level check",
  async (action, caller, input) => {
    const { run, actions } = world(caller);
    await expect(
      run(action, { ...input, prompt: "Fix\u0000it" }),
    ).rejects.toMatchObject({ code: "invalid_input" });
    expect(checks()).toEqual([]);
    expect(actions.start).not.toHaveBeenCalled();
    expect(actions.send).not.toHaveBeenCalled();
    expect(actions.draft).not.toHaveBeenCalled();
  },
);

// #87 review r1 m2. Defect: a reply that is neither Rust's allowed nor its
// failed shape (a renamed tag, a bare error, a lead id that is no session id)
// reads as allowed, so the gate fails open.
it.each([
  [{ outcome: "Failed", error: { code: "worker_scope", hint: "No" } }],
  [{ error: { code: "worker_scope", hint: "No" } }],
  [{ outcome: "allowed", leadSessionId: "not a session" }],
  [{ outcome: "Allowed", leadSessionId: null }],
  [null],
])("runs nothing when the level check answers %j", async (reply) => {
  const { run, actions } = world("lead");
  command.answer = () => reply;
  await expect(
    run("sessions.start", {
      harness: "codex",
      model: "codex:test",
      prompt: "Fix the flaky test",
    }),
  ).rejects.toThrow("The level check answered in an unknown form");
  expect(actions.start).not.toHaveBeenCalled();
});

// Design 16.2. Defect: a draft start, which runs no work, writes a start row
// that a steer could name, or answers a message_id.
it("writes no start row for a draft start and answers no message_id", async () => {
  const { run, actions } = world("a-chat");
  const answer = await run("sessions.start", {
    ...CHOICE,
    prompt: "Fix the flaky test",
    draft: true,
  });
  expect(actions.start).toHaveBeenCalledOnce();
  expect(gateCalls().map(([name]) => name)).toEqual(["mono_app_action_check"]);
  expect(answer).not.toHaveProperty("message_id");
});

// Design 16.2. Defect: a start row that fails after the launch answers only
// its own hint ("use a new request id"), so the agent starts the work again,
// or loses the failure's code.
it("answers a coded record failure after the launch with its code and the started line", async () => {
  const { run, actions } = world("lead");
  command.answer = (name) =>
    name === "mono_message_record_start"
      ? {
          outcome: "failed",
          error: { code: "request_id_reused", hint: "Use a new request id." },
        }
      : { outcome: "allowed", leadSessionId: "lead", ask: null };
  await expect(
    run("sessions.start", { ...CHOICE, prompt: "Fix the flaky test" }, "r9"),
  ).rejects.toMatchObject({
    code: "request_id_reused",
    hint: "Use a new request id; session app-lead-r9 was started anyway, so do not start it again; read or steer it by its id.",
  });
  expect(actions.start).toHaveBeenCalledOnce();
});

// Design 9 (appLaunchId). Defect: a start whose new session id,
// app-<caller>-<request id>, is over 256 characters reaches the level check
// and launches, though no later call can name that session; or an id of
// exactly 256 characters is refused.
it("refuses a start whose new session id would be over 256 characters, before the check", async () => {
  const { host, actions, sessions } = world("lead");
  const caller = { ...sessions.get("lead")!, id: "s".repeat(200) };
  const start = (requestId: string) =>
    handleAgentApp(caller, requestId, "sessions.start", { ...CHOICE, prompt: "Fix it" }, host);
  const fits = "r".repeat(256 - "app-".length - 200 - "-".length);
  await expect(start(`${fits}r`)).rejects.toMatchObject({ code: "invalid_input" });
  expect(checks()).toEqual([]);
  expect(actions.start).not.toHaveBeenCalled();
  await start(fits).catch(() => {});
  expect(
    checks().map((args) => (args as { call: { startedSessionId: string } }).call.startedSessionId),
  ).toEqual([
    `app-${caller.id}-${fits}`,
  ]);
});

// #93 review r1 m2. Defect: a record that fails with no code (a Rust Err
// such as "database is locked", or a reply this window cannot read) drops
// the started line, so the agent starts the work again; or its text says
// the action did not run.
it.each([
  ["a Rust Err", "database is locked", "database is locked"],
  ["an unknown reply", { outcome: "stored" }, "The start row answered in an unknown form"],
])("answers %s from the record after the launch in one sentence with the started line", async (_case, reply, failure) => {
  const { run, actions } = world("lead");
  command.answer = (name) => {
    if (name !== "mono_message_record_start")
      return { outcome: "allowed", leadSessionId: "lead", ask: null };
    if (typeof reply === "string") throw reply;
    return reply;
  };
  const error = await run(
    "sessions.start",
    { ...CHOICE, prompt: "Fix the flaky test" },
    "r10",
  ).catch((caught: unknown) => caught);
  expect(error).toBeInstanceOf(Error);
  expect((error as Error).message).toBe(
    `${failure}; session app-lead-r10 was started anyway, so do not start it again; read or steer it by its id.`,
  );
  expect(actions.start).toHaveBeenCalledOnce();
});

// PR7-9a review r1 point 4. Defect: when mono_app_action_check fails (a
// retried refusal hits the request id's UNIQUE row until PR 11), the action
// runs anyway.
it("runs nothing when the level check itself fails", async () => {
  const { run, actions } = world("lead");
  command.answer = () => {
    throw "UNIQUE constraint failed: mono_messages.from_session_id";
  };
  await expect(
    run("sessions.start", {
      harness: "codex",
      model: "codex:test",
      prompt: "Fix the flaky test",
    }),
  ).rejects.toBe("UNIQUE constraint failed: mono_messages.from_session_id");
  expect(actions.start).not.toHaveBeenCalled();
});

// #81 review n5. Defect: messages.* go to the app-action gate too, so a
// worker's message to its lead gets worker_scope and an app_action row
// instead of its own gate in mono_message_submit.
it("sends messages.* to their own gate, never to the level check", async () => {
  const { run, actions } = world("worker");
  await run("messages.send", {
    message: "Done",
    send_to: "lead",
    wanting_reply: false,
    intent: "new",
  });
  expect(actions.messageSend).toHaveBeenCalledOnce();
  expect(checks()).toEqual([]);
});

// Design 13.4. Defect: a session with no app access reaches Rust as a caller.
it("refuses a session with no app access with turn_inactive and no check", async () => {
  const { run, sessions } = world("lead");
  sessions.get("lead")!.blocks = [];
  const refused = await run("models.list", {}).catch((error: unknown) => error);
  expect(refused).toBeInstanceOf(AppActionError);
  expect(refused).toMatchObject({ code: "turn_inactive" });
  expect(checks()).toEqual([]);
});

/** `world(caller)` with `caller` made by sessions.start in `runtimeMode`. */
function startedWorld(caller: string, runtimeMode: Session["runtimeMode"]) {
  const made = world(caller);
  Object.assign(made.sessions.get(caller)!, {
    runtimeMode,
    blocks: [{ id: "u1", role: "user", text: "Fix CI", startedBy: "a-chat" }],
  });
  return made;
}

// Design 6.6 runtime ceiling. Defect: a started lead in supervised starts a
// worker in full-access.
it("refuses a full-access worker from a started lead in supervised", async () => {
  const { run, actions } = startedWorld("lead", "supervised");
  await expect(
    run("sessions.start", {
      ...CHOICE,
      prompt: "Deploy",
      runtimeMode: "full-access",
    }),
  ).rejects.toThrow(
    "runtimeMode cannot be more permissive than this session's (supervised)",
  );
  expect(actions.start).not.toHaveBeenCalled();
});

// Design 6.6 runtime ceiling and 13.4. Defect: the ceiling refuses a mode
// stricter than the lead's own, or the launch does not name its caller, so
// the worker gets no app access.
it("starts a supervised worker from a started lead in auto, stamped with its caller", async () => {
  const { run, actions } = startedWorld("lead", "auto");
  await run("sessions.start", {
    ...CHOICE,
    prompt: "Review",
    runtimeMode: "supervised",
  });
  expect(actions.start).toHaveBeenCalledWith(
    expect.objectContaining({ runtimeMode: "supervised", startedBy: "lead" }),
    "app-lead-r1",
  );
});

// Design 6.6: the ceiling binds only started access. Defect: an /operator
// lead, whose access the user granted, cannot start a more permissive worker.
it("lets an /operator lead in supervised start a full-access worker", async () => {
  const { run, actions, sessions } = world("lead");
  sessions.get("lead")!.runtimeMode = "supervised";
  await run("sessions.start", {
    ...CHOICE,
    prompt: "Deploy",
    runtimeMode: "full-access",
  });
  expect(actions.start).toHaveBeenCalledWith(
    expect.objectContaining({ runtimeMode: "full-access" }),
    "app-lead-r1",
  );
});

// Design 6.6 runtime ceiling, its boundary. Defect: the ceiling refuses the
// lead's own mode, so every plain sessions.start of a started lead (no
// runtimeMode, which defaults to the lead's own) fails.
it("starts a worker in a started lead's own mode, given or omitted", async () => {
  const { run, actions } = startedWorld("lead", "auto");
  await run("sessions.start", { ...CHOICE, prompt: "Review", runtimeMode: "auto" });
  await run("sessions.start", { ...CHOICE, prompt: "Test" }, "r2");
  expect(
    actions.start.mock.calls.map(([launch]) => (launch as { runtimeMode: string }).runtimeMode),
  ).toEqual(["auto", "auto"]);
});
