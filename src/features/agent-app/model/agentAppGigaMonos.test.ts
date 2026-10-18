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
  loadMonoRoster,
  monoWritesSettled,
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

/**
 * `monocode.db` behind the roster commands once a test loads the roster, as
 * `mono_roster.rs` keeps it: an update merges its patch into the stored data
 * (RFC 7396), so a key the patch sets to null is gone. Arguments go through
 * JSON, as over IPC.
 */
const db = vi.hoisted(() => ({
  rows: new Map<
    string,
    {
      id: string;
      role: string;
      data: Record<string, unknown>;
      sortOrder: number;
    }
  >(),
  revision: 0,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (command: string, raw: Record<string, unknown> = {}) => {
    const args = JSON.parse(JSON.stringify(raw));
    if (command === "monos_create") db.rows.set(args.mono.id, args.mono);
    else if (command === "monos_update")
      for (const update of args.updates) {
        const row = db.rows.get(update.id)!;
        for (const [key, value] of Object.entries(update.patch ?? {}))
          if (value === null) delete row.data[key];
          else row.data[key] = value;
        if (update.role) row.role = update.role;
        if (update.sortOrder !== undefined) row.sortOrder = update.sortOrder;
      }
    else if (command !== "monos_list")
      throw new Error(`unexpected command ${command}`);
    if (command !== "monos_list") db.revision += 1;
    return {
      revision: db.revision,
      monos: [...db.rows.values()]
        .sort((a, b) => a.sortOrder - b.sortOrder)
        .map((row) => structuredClone(row)),
    };
  },
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

beforeEach(() => {
  __monosTestReset();
  db.rows.clear();
  db.revision = 0;
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
      get: vi.fn(async (monoId: string) => {
        const mono = findMono(monoId);
        return mono && { mono };
      }),
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
    const { run, boss, shroom, monos } = fixture(kind, { giga });
    const roster = JSON.stringify(listMonos());
    await expect(run("monos.create", create)).rejects.toMatchObject({
      code: "giga_only",
    });
    await expect(
      run("monos.update", { monoId: shroom.id, messageAllowlist: [boss.id] }),
    ).rejects.toMatchObject({ code: "giga_only" });
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
    messageAllowlist: [],
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

// Design 6.1 and review r1 m1, n4, n5. Defect: TS sets the list but drops it
// when it reads the roster back, so the next change cannot clear it and a
// reload hides it; monos.get hides it, so a Giga that adds one id drops the
// rest; a repeat is stored twice; a stored entry that is no Mono id is read;
// or a reader's copy shares the roster's array.
it("keeps, shows and clears the messageAllowlist that monos.update set, across reloads", async () => {
  const reload = async () => {
    await monoWritesSettled();
    __monosTestReset();
    await loadMonoRoster();
  };
  await loadMonoRoster();
  const { run, boss, shroom } = fixture();
  await expect(
    run("monos.update", {
      monoId: shroom.id,
      messageAllowlist: [boss.id, boss.id],
    }),
  ).resolves.toMatchObject({ monoId: shroom.id, messageAllowlist: [boss.id] });
  await reload();
  expect(findMono(shroom.id)!.messageAllowlist).toEqual([boss.id]);
  await expect(run("monos.get", { monoId: shroom.id })).resolves.toMatchObject({
    messageAllowlist: [boss.id],
  });
  findMono(shroom.id)!.messageAllowlist!.push("mono-x");
  expect(findMono(shroom.id)!.messageAllowlist).toEqual([boss.id]);
  db.rows.get(shroom.id)!.data.messageAllowlist = [boss.id, "not a mono id", 7];
  await reload();
  expect(findMono(shroom.id)!.messageAllowlist).toEqual([boss.id]);
  await expect(
    run("monos.update", { monoId: shroom.id, messageAllowlist: [] }),
  ).resolves.toMatchObject({ messageAllowlist: [] });
  await reload();
  expect(findMono(shroom.id)!.messageAllowlist).toBeUndefined();
  expect(db.rows.get(shroom.id)!.data.messageAllowlist).toBeUndefined();
});

// Defect: an allowlist that names a deleted Mono, a typo or the Mono itself
// reports success, so the Giga believes two Monos can now message each other.
it("refuses a messageAllowlist entry that is not another Mono, before changing anything", async () => {
  const { run, shroom, monos } = fixture();
  const before = JSON.stringify(listMonos());
  for (const messageAllowlist of [["gone"], [shroom.id], "gone", [7]])
    await expect(
      run("monos.update", { monoId: shroom.id, messageAllowlist }),
    ).rejects.toMatchObject({ code: "invalid_input" });
  expect(monos.update).not.toHaveBeenCalled();
  expect(JSON.stringify(listMonos())).toBe(before);
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
