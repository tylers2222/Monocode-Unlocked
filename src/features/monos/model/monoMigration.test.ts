// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi, type Mock } from "vitest";
import type { Mono } from "./mono";

/**
 * Stands in for `monocode.db` behind the Tauri commands, across restarts. The
 * import rule mirrors `mono_roster::import_legacy`, whose own tests pin it.
 */
type Row = {
  id: string;
  name: string;
  role: string;
  data: Record<string, unknown>;
  sortOrder: number;
};
const db = {
  rows: new Map<string, Row>(),
  imported: false,
  revision: 0,
  broken: false,
  listeners: [] as ((event: { payload: unknown }) => void)[],
};

function snapshot() {
  return {
    revision: db.revision,
    monos: [...db.rows.values()]
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .map((row) => structuredClone(row)),
  };
}

function write(change: () => void) {
  change();
  db.revision += 1;
  const after = snapshot();
  for (const listener of db.listeners) listener({ payload: after });
  return after;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, raw: Record<string, unknown> = {}) => {
    if (db.broken) throw new Error("database is locked");
    const args = structuredClone(raw) as {
      monos?: Row[];
      mono?: Row;
      id?: string;
    };
    const put = (rows: Row[]) =>
      rows.forEach((row) => db.rows.set(row.id, row));
    switch (command) {
      case "monos_list":
        return snapshot();
      case "monos_import_legacy":
        return write(() => {
          if (!db.imported && db.rows.size === 0) put(args.monos!);
          db.imported = true;
        });
      case "monos_upsert":
        return write(() => put([args.mono!]));
      case "monos_upsert_many":
        return write(() => put(args.monos!));
      case "monos_delete":
        return write(() => db.rows.delete(args.id!));
    }
    throw new Error(`unexpected command ${command}`);
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (_: string, listener: (event: { payload: unknown }) => void) => {
      db.listeners.push(listener);
      return () => {};
    },
  ),
}));

const ROSTER_KEY = "monocode:mono-roster";
let storage: Map<string, string>;
let setItem: Mock;

beforeEach(() => {
  db.rows.clear();
  db.imported = false;
  db.revision = 0;
  db.broken = false;
  db.listeners = [];
  storage = new Map();
  setItem = vi.fn((key: string, value: string) => storage.set(key, value));
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem,
    removeItem: (key: string) => storage.delete(key),
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

/**
 * A fresh app start: a new module instance that loads before first render.
 * Dynamic on purpose: each restart needs its own copy of the module state.
 */
async function start() {
  vi.resetModules();
  const mono = await import("./mono");
  await mono.loadMonoRoster();
  return mono;
}

const STORED: Mono[] = [
  {
    id: "giga-1",
    sessionId: "chat-1",
    name: "Boss",
    mascot: "skull",
    color: "#f00",
    projects: ["/MonoCode", "/code/app"],
    showStartedSessionsInSidebar: true,
    useSidebarFolders: true,
    giga: true,
    gigaSlug: "boss",
  },
  {
    id: "mono-2",
    mascot: "crab",
    color: "#0f0",
    projects: [],
    showStartedSessionsInSidebar: false,
    instructions: "Be brief.",
    legacyProject: "/code/old",
  },
  { id: "mono-3", mascot: "ghost", color: "#00f", projects: ["/code/site"] },
];

function storeRoster(entries: unknown[] = STORED) {
  storage.set(ROSTER_KEY, JSON.stringify(entries));
  return storage.get(ROSTER_KEY);
}

it("imports the stored roster with every field and its order, once", async () => {
  // Only `giga: true` ever made a Giga; anything else imports as a Mono.
  const stored = storeRoster([
    ...STORED,
    { ...STORED[2], id: "mono-4", giga: "yes" },
  ]);
  let mono = await start();

  expect(mono.listMonos()).toEqual([...STORED, { ...STORED[2], id: "mono-4" }]);
  expect(
    [...db.rows.values()].map((row) => [
      row.id,
      row.name,
      row.role,
      row.sortOrder,
    ]),
  ).toEqual([
    ["giga-1", "Boss", "giga", 0],
    ["mono-2", "MonoCrab", "mono", 1],
    ["mono-3", "MonoGhost", "mono", 2],
    ["mono-4", "MonoGhost", "mono", 3],
  ]);

  mono.saveMonoName("mono-3", "Casper");
  mono.reorderMonos(["mono-3", "giga-1", "mono-2", "mono-4"]);
  await mono.monoWritesSettled();
  mono = await start();

  // The second start reads the database; the stored roster changes nothing.
  expect(mono.listMonos().map((entry) => [entry.id, entry.name])).toEqual([
    ["mono-3", "Casper"],
    ["giga-1", "Boss"],
    ["mono-2", undefined],
    ["mono-4", undefined],
  ]);
  expect(storage.get(ROSTER_KEY)).toBe(stored);
  expect(setItem).not.toHaveBeenCalledWith(ROSTER_KEY, expect.anything());
});

it("keeps every Mono deleted after a restart, though the old key still holds them", async () => {
  const stored = storeRoster();
  let mono = await start();
  for (const { id } of mono.listMonos()) mono.removeMono(id);
  await mono.monoWritesSettled();

  mono = await start();
  expect(mono.listMonos()).toEqual([]);
  expect(db.rows.size).toBe(0);
  expect(storage.get(ROSTER_KEY)).toBe(stored);
});

it("keeps a change made through updateMono after a restart", async () => {
  let mono = await start();
  const created = mono.createMono(["/code/app"], { name: "Scout" });
  mono.updateMono(created.id, (entry) => ({
    ...entry,
    projects: [...entry.projects, "/code/site"],
    useSidebarFolders: true,
    giga: true,
    gigaSlug: "scout",
  }));
  const expected = mono.findMono(created.id);
  await mono.monoWritesSettled();

  mono = await start();
  expect(mono.findMono(created.id)).toEqual(expected);
  expect(expected).toMatchObject({
    name: "Scout",
    projects: ["/code/app", "/code/site"],
    giga: true,
  });
  expect(storage.has(ROSTER_KEY)).toBe(false);
});

it("shows the stored roster when the database cannot load, and never writes the key", async () => {
  const stored = storeRoster();
  db.broken = true;
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  const mono = await start();

  expect(mono.listMonos()).toEqual(STORED);
  const created = mono.createMono();
  mono.updateMono("mono-2", (entry) => ({ ...entry, name: "Pinch" }));
  mono.removeMono("mono-3");
  await mono.monoWritesSettled();

  // Changes show until the app quits, and reach neither the key nor the db.
  expect(mono.listMonos().map((entry) => entry.id)).toEqual([
    "giga-1",
    "mono-2",
    created.id,
  ]);
  expect(storage.get(ROSTER_KEY)).toBe(stored);
  expect(setItem).not.toHaveBeenCalledWith(ROSTER_KEY, expect.anything());
  expect(db.rows.size).toBe(0);
  error.mockRestore();
});

it("shows another window's change, and never lets a late older roster undo a newer one", async () => {
  storeRoster();
  const here = await start();
  const listener = db.listeners.at(-1)!;
  const changed = vi.fn();
  const unsubscribe = here.subscribeMonos(changed);
  const before = snapshot();

  // Another window renames one, then removes another.
  write(() => (db.rows.get("mono-2")!.data.name = "Pinch"));
  const renamed = snapshot();
  write(() => db.rows.delete("mono-3"));
  expect(changed).toHaveBeenCalled();
  expect(here.listMonos().map((entry) => [entry.id, entry.name])).toEqual([
    ["giga-1", "Boss"],
    ["mono-2", "Pinch"],
  ]);

  listener({ payload: renamed });
  listener({ payload: before });
  expect(here.listMonos().map((entry) => entry.id)).toEqual([
    "giga-1",
    "mono-2",
  ]);
  unsubscribe();
});
