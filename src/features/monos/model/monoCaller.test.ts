// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import {
  __monosTestReset,
  createMono,
  loadMonoRoster,
  saveMonoSessionId,
} from "./mono";
import { resolveCaller } from "./monoCaller";
import { clearHabitRun, markHabitRun } from "./monoHabits";
import type { SessionId, TurnId } from "./monoMessages";

const db = vi.hoisted(() => ({
  update: undefined as Promise<unknown> | undefined,
  monos: [] as unknown[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) => {
    if (command === "monos_list") return { revision: 1, monos: db.monos };
    if (command === "monos_update") return db.update;
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
const session = (id: string) => id as SessionId;

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

it("resolves a Mono's chat to that Mono", async () => {
  await loadMonoRoster();
  expect(await resolveCaller(session("giga-chat"), turn)).toEqual({
    ok: true,
    caller: {
      kind: "mono_chat",
      monoId: "giga-1",
      sessionId: "giga-chat",
      turnId: "turn-1",
    },
  });
});

it("resolves a habit run to the Mono it works for", async () => {
  await loadMonoRoster();
  markHabitRun("giga-run", "giga-1");
  expect(await resolveCaller(session("giga-run"), turn)).toMatchObject({
    ok: true,
    caller: { kind: "habit_run", monoId: "giga-1", sessionId: "giga-run" },
  });
});

it("refuses a session that is neither a Mono chat nor a habit run", async () => {
  await loadMonoRoster();
  expect(await resolveCaller(session("project-session"), turn)).toMatchObject({
    ok: false,
    code: "not_a_mono",
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
