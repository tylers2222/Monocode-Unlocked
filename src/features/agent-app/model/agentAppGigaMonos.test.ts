// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import { setHarnessModels } from "../../sessions/model/models";
import { rememberProject } from "../../projects/model/recents";
import {
  createMono,
  findMono,
  listMonos,
  saveMonoGiga,
  saveMonoSessionId,
} from "../../monos/model/mono";
import { configureMono, createMonoWithChat } from "../../monos/model/gigaMono";
import { handleAgentApp, type AgentAppHost } from "./agentApp";

vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex" || id === "claude",
}));

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
  setHarnessModels("codex", [
    { id: "codex:test", harness: "codex", name: "Test" },
    { id: "codex:last", harness: "codex", name: "Last used" },
  ]);
  setHarnessModels("claude", [
    { id: "claude:opus", harness: "claude", name: "Opus" },
  ]);
});
afterEach(() => vi.unstubAllGlobals());

/**
 * Boss, the caller's Mono, and Shroom, another Mono on Codex. The caller is
 * Boss's own chat, one of its habit runs, or a plain project session; Boss is
 * Giga unless `giga` is false.
 */
function fixture(
  kind: "mono" | "habit" | "session" = "mono",
  { giga = true, shroomBusy = false } = {},
) {
  const boss = createMono([], { name: "Boss" });
  if (giga) saveMonoGiga(boss.id, true);
  const shroom = createMono(["/code/app"], { name: "Shroom" });
  const shroomChat: Session = {
    ...newSession("codex", "/home", "codex:test", "auto"),
    busy: shroomBusy,
  };
  saveMonoSessionId(shroom.id, shroomChat.id);
  const source = newSession("codex", "/code/app", "codex:test");
  const chats: Session[] = [shroomChat];
  const writeAgentFile = vi.fn(async () => "new-hash");
  const replace = vi.fn((next: Session) => {
    chats.splice(
      chats.findIndex((chat) => chat.id === next.id),
      1,
      next,
    );
  });
  const host = {
    isMono: (id: string) => kind === "mono" && id === source.id,
    isHabitRun: (id: string) => kind === "habit" && id === source.id,
    monoOf: (id: string) => {
      if (kind === "session" || id !== source.id) return undefined;
      const mono = findMono(boss.id)!;
      return {
        id: mono.id,
        name: mono.name,
        projects: mono.projects,
        ...(mono.giga ? { giga: true } : {}),
      };
    },
    monos: {
      list: vi.fn(),
      read: vi.fn(),
      send: vi.fn(),
      create: vi.fn((settings) =>
        createMonoWithChat(settings, {
          home: async () => "/home",
          add: (chat) => chats.push(chat),
        }),
      ),
      configure: vi.fn(async (monoId, change) => {
        const mono = findMono(monoId)!;
        const chat = chats.find((entry) => entry.id === mono.sessionId)!;
        return configureMono(mono, chat, change, {
          switchModel: (current, choice) => ({
            ...current,
            harness: choice.harness,
            model: choice.model.id,
          }),
          replace,
        });
      }),
    },
    agentFiles: vi.fn(async () => ({ soulHash: "seeded-hash" })),
    writeAgentFile,
    workspaceStatus: vi.fn(async (monoId: string) => ({
      root: "/home/MonoCode",
      state: "ready",
      gigaSlug: findMono(monoId)?.gigaSlug,
    })),
  } as unknown as AgentAppHost;
  const run = (action: string, input: Record<string, unknown>) =>
    handleAgentApp(source, "req-1", action, input, host);
  return {
    run,
    boss,
    shroom,
    chats,
    replace,
    writeAgentFile,
    monos: host.monos!,
  };
}

const create = {
  name: "Scout",
  harness: "codex",
  model: "codex:test",
  projects: [],
};

it.each([
  ["a regular Mono's chat", "mono", false],
  ["a regular Mono's habit run", "habit", false],
  ["a project session", "session", true],
] as const)(
  "refuses monos.create and monos.configure from %s",
  async (_, kind, giga) => {
    const { run, shroom, monos } = fixture(kind, { giga });
    const roster = JSON.stringify(listMonos());
    await expect(run("monos.create", create)).rejects.toThrow(
      "Only a Giga Mono can create or configure Monos",
    );
    await expect(
      run("monos.configure", { monoId: shroom.id, name: "Renamed" }),
    ).rejects.toThrow("Only a Giga Mono can create or configure Monos");
    expect(monos.create).not.toHaveBeenCalled();
    expect(monos.configure).not.toHaveBeenCalled();
    expect(JSON.stringify(listMonos())).toBe(roster);
  },
);

it("refuses monos.create without a model and points to models.list", async () => {
  const { run, monos } = fixture();
  await expect(
    run("monos.create", { name: "Scout", harness: "codex", projects: [] }),
  ).rejects.toThrow("models.list");
  expect(monos.create).not.toHaveBeenCalled();
  expect(listMonos()).toHaveLength(2);
});

it("creates a Mono whose chat runs on exactly the harness and model given", async () => {
  localStorage.setItem(
    "monocode.lastModel",
    JSON.stringify({ harness: "codex", model: "codex:last" }),
  );
  rememberProject("/code/app");
  const { run, chats, writeAgentFile } = fixture("habit");
  const result = (await run("monos.create", {
    ...create,
    projects: ["app"],
    soul: "# Soul\n\nReview PRs only.",
    giga: false,
  })) as { monoId: string };
  const mono = findMono(result.monoId)!;
  const chat = chats.find((entry) => entry.id === mono.sessionId)!;
  expect(chat).toMatchObject({ harness: "codex", model: "codex:test" });
  expect(mono).toMatchObject({ name: "Scout", projects: ["/code/app"] });
  expect(mono.giga).toBeUndefined();
  expect(result).toEqual({
    monoId: mono.id,
    name: "Scout",
    giga: false,
    projects: ["/code/app"],
    harness: "codex",
    model: "codex:test",
    created: true,
  });
  expect(writeAgentFile).toHaveBeenCalledExactlyOnceWith(
    mono.id,
    "SOUL.md",
    "# Soul\n\nReview PRs only.",
    "seeded-hash",
  );
});

it.each([
  [{ projects: ["/code/nowhere"] }, "Not a project in MonoCode"],
  [{ giga: true }, "giga can only be false"],
  [{ harness: "codex", model: "claude:opus" }, "Unknown model for codex"],
])("refuses an invalid monos.create (%#)", async (change, message) => {
  const { run, monos } = fixture();
  await expect(run("monos.create", { ...create, ...change })).rejects.toThrow(
    message,
  );
  expect(monos.create).not.toHaveBeenCalled();
});

it("changes another Mono's model, keeping its harness when only a model is given", async () => {
  const { run, shroom, chats } = fixture();
  await expect(
    run("monos.configure", { monoId: shroom.id, model: "claude:opus" }),
  ).rejects.toThrow("Unknown model for codex");
  await expect(
    run("monos.configure", { monoId: shroom.id, harness: "claude" }),
  ).rejects.toThrow("models.list");
  expect(chats[0]).toMatchObject({ harness: "codex", model: "codex:test" });

  expect(
    await run("monos.configure", {
      monoId: shroom.id,
      harness: "claude",
      model: "claude:opus",
      name: "Shroomzilla",
    }),
  ).toMatchObject({
    monoId: shroom.id,
    name: "Shroomzilla",
    harness: "claude",
    model: "claude:opus",
  });
  expect(chats[0]).toMatchObject({ harness: "claude", model: "claude:opus" });
});

it("leaves a busy Mono unchanged", async () => {
  const { run, shroom, chats, replace } = fixture("mono", { shroomBusy: true });
  const before = JSON.stringify(listMonos());
  await expect(
    run("monos.configure", {
      monoId: shroom.id,
      model: "codex:last",
      name: "Renamed",
      projects: [],
    }),
  ).rejects.toThrow("That Mono is busy");
  expect(JSON.stringify(listMonos())).toBe(before);
  expect(chats[0].model).toBe("codex:test");
  expect(replace).not.toHaveBeenCalled();
});

it("does not let a Giga Mono configure itself", async () => {
  const { run, boss, monos } = fixture();
  await expect(
    run("monos.configure", { monoId: boss.id, model: "codex:test" }),
  ).rejects.toThrow("A Mono cannot configure itself");
  expect(monos.configure).not.toHaveBeenCalled();
});

// Defect: a project session reads the workspace, or a habit run is refused.
it.each(["mono", "habit"] as const)(
  "workspace.status answers a Giga's %s with its slug",
  async (kind) => {
    const { run } = fixture(kind);
    await expect(run("workspace.status", {})).resolves.toEqual({
      root: "/home/MonoCode",
      state: "ready",
      gigaSlug: "boss",
    });
  },
);

it("workspace.status refuses a project session", async () => {
  const { run } = fixture("session");
  await expect(run("workspace.status", {})).rejects.toThrow(
    "Only a Mono or its habit runs can read the workspace",
  );
});
