// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { newSession, type Session } from "../../sessions/model/session";
import { setHarnessModels } from "../../sessions/model/models";
import { rememberProject } from "../../projects/model/recents";
import { setHomeDir } from "../../../shared/lib/paths";
import {
  __monosTestReset,
  createMono,
  findMono,
  listMonos,
  saveMonoGiga,
  saveMonoSessionId,
} from "../../monos/model/mono";
import {
  createMonoWithChat,
  updateMonoSettings,
} from "../../monos/model/gigaMono";
import { handleAgentApp, type AgentAppHost } from "./agentApp";

vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "codex" || id === "claude",
}));

beforeEach(() => {
  __monosTestReset();
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
 * Boss, the caller's Mono, and Shroom, another Mono on Codex; both have a
 * chat. The caller is Boss's own chat, one of its habit runs, or a plain
 * project session; Boss is Giga unless `giga` is false.
 */
function fixture(
  kind: "mono" | "habit" | "session" = "mono",
  { giga = true, shroomBusy = false, preparingHandoff = false } = {},
) {
  const boss = createMono([], { name: "Boss" });
  if (giga) saveMonoGiga(boss.id, true);
  const shroom = createMono(["/code/app"], { name: "Shroom" });
  const shroomChat: Session = {
    ...newSession("codex", "/home", "codex:test", "auto"),
    busy: shroomBusy,
  };
  saveMonoSessionId(shroom.id, shroomChat.id);
  const bossChat = newSession("codex", "/home", "codex:test", "auto");
  saveMonoSessionId(boss.id, bossChat.id);
  const source = newSession("codex", "/code/app", "codex:test");
  const chats: Session[] = [shroomChat, bossChat];
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
      update: vi.fn((monoId, change) =>
        updateMonoSettings(monoId, change, {
          openChat: async (id) =>
            chats.find((chat) => chat.id === findMono(id)?.sessionId),
          preparingHandoff: () => preparingHandoff,
          switchModel: (current, choice) => ({
            ...current,
            harness: choice.harness,
            model: choice.model.id,
          }),
          replace,
        }),
      ),
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
  "refuses monos.create and monos.update from %s with giga_only",
  async (_, kind, giga) => {
    const { run, shroom, monos } = fixture(kind, { giga });
    const roster = JSON.stringify(listMonos());
    await expect(run("monos.create", create)).rejects.toMatchObject({
      code: "giga_only",
    });
    await expect(
      run("monos.update", { monoId: shroom.id, name: "Renamed" }),
    ).rejects.toMatchObject({ code: "giga_only" });
    expect(monos.create).not.toHaveBeenCalled();
    expect(monos.update).not.toHaveBeenCalled();
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

// Defect: a name blank under the shared trim makes a Mono with its default
// name, or one Rust refuses is created and then disappears.
it("refuses a monos.create name the database would refuse", async () => {
  const { run, monos } = fixture();
  for (const name of ["\u0085", "x".repeat(257)])
    await expect(
      run("monos.create", { ...create, name }),
    ).rejects.toMatchObject({ code: "invalid_input" });
  expect(monos.create).not.toHaveBeenCalled();
  expect(listMonos()).toHaveLength(2);
});

it("changes another Mono's model, keeping its harness when only a model is given", async () => {
  const { run, shroom, chats } = fixture();
  await expect(
    run("monos.update", { monoId: shroom.id, model: "claude:opus" }),
  ).rejects.toMatchObject({
    code: "invalid_input",
    hint: expect.stringContaining("Unknown model for codex"),
  });
  await expect(
    run("monos.update", { monoId: shroom.id, harness: "claude" }),
  ).rejects.toThrow("models.list");
  expect(chats[0]).toMatchObject({ harness: "codex", model: "codex:test" });

  expect(
    await run("monos.update", {
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
    run("monos.update", {
      monoId: shroom.id,
      model: "codex:last",
      name: "Renamed",
      projects: [],
    }),
  ).rejects.toMatchObject({ code: "mono_busy" });
  expect(JSON.stringify(listMonos())).toBe(before);
  expect(chats[0].model).toBe("codex:test");
  expect(replace).not.toHaveBeenCalled();
});

it("leaves a Mono that is preparing a handoff unchanged", async () => {
  const { run, shroom, chats, replace } = fixture("mono", {
    preparingHandoff: true,
  });
  const before = JSON.stringify(listMonos());
  await expect(
    run("monos.update", { monoId: shroom.id, model: "codex:last", name: "X" }),
  ).rejects.toMatchObject({ code: "mono_busy" });
  expect(JSON.stringify(listMonos())).toBe(before);
  expect(chats[0].model).toBe("codex:test");
  expect(replace).not.toHaveBeenCalled();
});

it("answers monos.update for an unknown Mono with unknown_mono", async () => {
  const { run } = fixture();
  const before = JSON.stringify(listMonos());
  await expect(
    run("monos.update", { monoId: "gone", name: "Renamed" }),
  ).rejects.toMatchObject({ code: "unknown_mono" });
  expect(JSON.stringify(listMonos())).toBe(before);
});

it("does not let a Giga Mono update itself", async () => {
  const { run, boss, chats } = fixture();
  const before = JSON.stringify(listMonos());
  await expect(
    run("monos.update", { monoId: boss.id, model: "codex:last", name: "Me" }),
  ).rejects.toMatchObject({ code: "self_update" });
  expect(JSON.stringify(listMonos())).toBe(before);
  expect(chats[1].model).toBe("codex:test");
});

// Defect: the rename reports success, the database refuses the name, and the
// change disappears on the next reload.
it("refuses a name the database would refuse, before changing anything", async () => {
  const { run, shroom, monos } = fixture();
  const before = JSON.stringify(listMonos());
  for (const name of ["x".repeat(257), "😀".repeat(257), "\u0085", 7])
    await expect(
      run("monos.update", { monoId: shroom.id, name }),
    ).rejects.toMatchObject({ code: "invalid_input" });
  expect(monos.update).not.toHaveBeenCalled();
  expect(JSON.stringify(listMonos())).toBe(before);
  // The limit counts characters, not UTF-16 units, as Rust does.
  const longest = "😀".repeat(256);
  expect(
    await run("monos.update", { monoId: shroom.id, name: ` ${longest}\u0085` }),
  ).toMatchObject({ name: longest });
  expect(findMono(shroom.id)!.name).toBe(longest);
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

it("refuses a folder named MonoCode other than the workspace as a Mono's project", async () => {
  setHomeDir("/home/u");
  rememberProject("/tmp/x/MonoCode");
  rememberProject("/home/u/MonoCode");
  const recents = localStorage.getItem("monocode.recentProjects");
  const { run, monos, shroom } = fixture();
  const reserved =
    "MonoCode is reserved for the Giga workspace. Rename the folder or choose another.";
  try {
    await expect(
      run("monos.create", { ...create, projects: ["/tmp/x/MonoCode"] }),
    ).rejects.toThrow(reserved);
    await expect(
      run("monos.update", {
        monoId: shroom.id,
        projects: ["/tmp/x/MonoCode"],
      }),
    ).rejects.toThrow(reserved);
    expect(monos.create).not.toHaveBeenCalled();
    expect(monos.update).not.toHaveBeenCalled();
    expect(listMonos()).toHaveLength(2);
    expect(findMono(shroom.id)!.projects).toEqual(["/code/app"]);
    expect(localStorage.getItem("monocode.recentProjects")).toBe(recents);

    await run("monos.update", {
      monoId: shroom.id,
      projects: ["/home/u/MonoCode"],
    });
    expect(findMono(shroom.id)!.projects).toEqual(["/home/u/MonoCode"]);
  } finally {
    setHomeDir(undefined);
  }
});
