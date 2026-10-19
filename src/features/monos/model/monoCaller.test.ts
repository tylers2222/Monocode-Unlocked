// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import {
  __monosTestReset,
  createMono,
  loadMonoRoster,
  saveMonoSessionId,
} from "./mono";
import { levelGate, resolveCaller, type CallingSession } from "./monoCaller";
import { clearHabitRun, markHabitRun } from "./monoHabits";
import type { TurnId } from "./monoMessages";

const db = vi.hoisted(() => ({
  update: undefined as Promise<unknown> | undefined,
  monos: [] as unknown[],
  check: undefined as unknown,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) => {
    if (command === "monos_list") return { revision: 1, monos: db.monos };
    if (command === "monos_update") return db.update;
    if (command === "mono_app_action_check") return db.check;
    throw new Error(`unexpected ${command}`);
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

const giga = {
  id: "giga-1",
  role: "giga",
  data: { sessionId: "giga-chat", mascot: "crab", color: "red", projects: [] },
};
const turn = "turn-1" as TurnId;
/** A session as the window has it; with no /operator turn it has no app access. */
const session = (
  id: string,
  extra: Partial<CallingSession> = {},
): CallingSession => ({ id, blocks: [], cwd: "/code/app", ...extra });
/** A submitted /operator turn: the thread has app access from then on. */
const operator = [
  { id: "u1", role: "user" as const, text: "Fix CI", monocode: true },
];

beforeEach(() => {
  __monosTestReset();
  clearHabitRun("giga-run");
  db.monos = [giga];
  db.update = undefined;
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
  });
});

// Design 13.4: a Mono's chat is a Mono caller even after /operator.
it("resolves a Mono's chat to that Mono", async () => {
  await loadMonoRoster();
  expect(
    await resolveCaller(session("giga-chat", { blocks: operator }), turn),
  ).toEqual({
    ok: true,
    caller: {
      kind: "mono_chat",
      monoId: "giga-1",
      sessionId: "giga-chat",
      turnId: "turn-1",
    },
  });
});

// Design 13.4, PR7-6 review r1 nit 4: a habit run is a Mono caller even
// after /operator.
it("resolves a habit run to the Mono it works for", async () => {
  await loadMonoRoster();
  markHabitRun("giga-run", "giga-1");
  expect(
    await resolveCaller(session("giga-run", { blocks: operator }), turn),
  ).toMatchObject({
    ok: true,
    caller: { kind: "habit_run", monoId: "giga-1", sessionId: "giga-run" },
  });
});

// Design 13.4. Defect: a session the user never gave app access, which
// cannot call the CLI, still messages as a lead.
it("refuses a session with no app access with not_a_mono", async () => {
  await loadMonoRoster();
  expect(await resolveCaller(session("project-session"), turn)).toMatchObject({
    ok: false,
    code: "not_a_mono",
  });
});

// Design 13.4. Defect: a lead or worker with /operator access answers
// not_a_mono, so it cannot report, or Rust gets no level facts for it.
it("resolves a lead and a worker with app access to session callers", async () => {
  await loadMonoRoster();
  expect(
    await resolveCaller(session("lead", { blocks: operator }), turn),
  ).toEqual({
    ok: true,
    caller: {
      kind: "session",
      sessionId: "lead",
      turnId: "turn-1",
      access: "operator",
      leadSessionId: null,
      cwd: "/code/app",
    },
  });
  expect(
    await resolveCaller(
      session("worker", { blocks: operator, leadSessionId: "lead" }),
      turn,
    ),
  ).toEqual({
    ok: true,
    caller: {
      kind: "session",
      sessionId: "worker",
      turnId: "turn-1",
      access: "operator",
      leadSessionId: "lead",
      cwd: "/code/app",
    },
  });
});

// Design 2 step 4. Defect: a call that no running turn authorized resolves a
// caller anyway, so its reply is bound to some turn the window guesses.
it("refuses a call that carries no turn with turn_inactive", async () => {
  await loadMonoRoster();
  expect(await resolveCaller(session("giga-chat"), null)).toMatchObject({
    ok: false,
    code: "turn_inactive",
  });
});

it("refuses even a Mono's chat while the roster is not from the database", async () => {
  const mono = createMono();
  saveMonoSessionId(mono.id, "memory-chat");
  expect(await resolveCaller(session("memory-chat"), turn)).toMatchObject({
    ok: false,
    code: "roster_unavailable",
  });
});

it("waits for queued roster writes before it answers", async () => {
  await loadMonoRoster();
  let answer!: (snapshot: unknown) => void;
  // Not Promise.withResolvers: CI runs Node 20, which does not have it.
  db.update = new Promise((resolve) => (answer = resolve));
  saveMonoSessionId("giga-1", "new-chat");
  let settled = false;
  const resolving = resolveCaller(session("new-chat"), turn).then((result) => {
    settled = true;
    return result;
  });
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("monos_update", expect.anything()),
  );
  expect(settled).toBe(false);
  answer({
    revision: 2,
    monos: [{ ...giga, data: { ...giga.data, sessionId: "new-chat" } }],
  });
  expect(await resolving).toMatchObject({
    ok: true,
    caller: { kind: "mono_chat", sessionId: "new-chat" },
  });
});

// Design 6.6. Defect: the level check needs the persisted roster as
// messaging does, so a Mono cannot steer, stop or start sessions while
// roster_unavailable holds; or Rust misses the window's copy of an unsaved
// target's lead.
it("checks a Mono's app action without the roster", async () => {
  const mono = createMono();
  saveMonoSessionId(mono.id, "memory-chat");
  db.check = { outcome: "allowed", leadSessionId: null };
  const input = { sessionId: "worker", prompt: "Stop and rebase" };
  const gate = levelGate(session("memory-chat"), turn, (id) =>
    id === "worker" ? "lead" : undefined,
  );
  expect(
    await gate.check("r1", { action: "sessions.send", target: "worker", input }),
  ).toEqual({ leadSessionId: null });
  expect(invoke).toHaveBeenCalledWith("mono_app_action_check", {
    call: {
      caller: {
        kind: "mono_chat",
        monoId: mono.id,
        sessionId: "memory-chat",
        turnId: "turn-1",
      },
      action: "sessions.send",
      target: "worker",
      targetLeadSessionId: "lead",
      requestId: "r1",
      input,
    },
  });
});

// Design 2 step 4, #87 review r1 n3. Defect: a call with no turn reaches
// Rust as a caller bound to a made-up turn instead of turn_inactive.
it("refuses a check with no turn as turn_inactive and calls nothing", async () => {
  await loadMonoRoster();
  vi.mocked(invoke).mockClear();
  await expect(
    levelGate(session("giga-chat"), null, () => undefined).check("r1", {
      action: "sessions.list",
      target: null,
      input: {},
    }),
  ).rejects.toMatchObject({ code: "turn_inactive" });
  expect(invoke).not.toHaveBeenCalledWith(
    "mono_app_action_check",
    expect.anything(),
  );
});

// Design 6.6. Defect: a refused app action runs anyway, or its answer loses
// the code, hint or row id that the agent acts on.
it("throws the level check's refusal with its code, hint and row id", async () => {
  await loadMonoRoster();
  const hint =
    'A worker can message only its lead. Message your lead (lead): messages.send with send_to "lead".';
  db.check = {
    outcome: "failed",
    error: { code: "worker_scope", hint, message_id: "m_aaaaaaaaaaaa" },
  };
  const worker = session("worker", { blocks: operator, leadSessionId: "lead" });
  await expect(
    levelGate(worker, turn, () => undefined).check("r1", {
      action: "notes.list",
      target: null,
      input: {},
    }),
  ).rejects.toMatchObject({
    code: "worker_scope",
    hint,
    details: { message_id: "m_aaaaaaaaaaaa" },
  });
});
