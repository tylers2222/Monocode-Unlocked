// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi, type Mock } from "vitest";
import type { Mono } from "./mono";

/**
 * Stands in for `monocode.db` behind the Tauri commands, across restarts. The
 * rules mirror `mono_roster.rs`, whose own tests pin them: import once into an
 * empty table, create never replaces, update never inserts and merges its
 * patch, a custom name wins over the window's name, a batch is all or nothing.
 */
type Row = {
  id: string;
  name: string;
  role: string;
  data: Record<string, unknown>;
  sortOrder: number;
};
type Update = {
  id: string;
  name?: string;
  role?: string;
  patch?: Record<string, unknown>;
  sortOrder?: number;
};
const db = {
  rows: new Map<string, Row>(),
  imported: false,
  revision: 0,
  broken: false,
  /** The next update fails, as a locked database would. */
  failUpdate: false,
  /** Each update waits for this before it runs. */
  updateGate: Promise.resolve(),
  /** Each list takes its snapshot, then waits for this before it answers. */
  listGate: Promise.resolve(),
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

function customName(row: Row) {
  return typeof row.data.name === "string" ? row.data.name.trim() : "";
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, raw: Record<string, unknown> = {}) => {
    if (db.broken) throw new Error("database is locked");
    const args = structuredClone(raw) as {
      monos?: Row[];
      mono?: Row;
      updates?: Update[];
      id?: string;
    };
    switch (command) {
      case "monos_list": {
        const listed = snapshot();
        await db.listGate;
        return listed;
      }
      case "monos_import_legacy":
        return write(() => {
          if (!db.imported && db.rows.size === 0)
            for (const row of args.monos!)
              db.rows.set(row.id, {
                ...row,
                name: customName(row) || row.name,
              });
          db.imported = true;
        });
      case "monos_create": {
        const row = args.mono!;
        if (db.rows.has(row.id)) throw new Error("UNIQUE constraint failed");
        return write(() =>
          db.rows.set(row.id, { ...row, name: customName(row) || row.name }),
        );
      }
      case "monos_update": {
        if (db.failUpdate) {
          db.failUpdate = false;
          throw new Error("database is locked");
        }
        await db.updateGate;
        const next = structuredClone(db.rows);
        for (const update of args.updates!) {
          const row = next.get(update.id);
          if (!row) throw new Error(`unknown_mono: ${update.id}`);
          for (const [key, value] of Object.entries(update.patch ?? {}))
            if (value === null) delete row.data[key];
            else row.data[key] = value;
          row.name = customName(row) || update.name || row.name;
          if (update.role) row.role = update.role;
          if (update.sortOrder !== undefined) row.sortOrder = update.sortOrder;
        }
        return write(() => (db.rows = next));
      }
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
      return () => {
        db.listeners = db.listeners.filter((entry) => entry !== listener);
      };
    },
  ),
}));

const ROSTER_KEY = "monocode:mono-roster";
let storage: Map<string, string>;
let setItem: Mock;

beforeEach(() => {
  db.rows = new Map();
  db.imported = false;
  db.revision = 0;
  db.broken = false;
  db.failUpdate = false;
  db.updateGate = Promise.resolve();
  db.listGate = Promise.resolve();
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

/** Another window's change: committed, and announced unless `silent`. */
function otherWindow(change: () => void, { silent = false } = {}) {
  if (!silent) return write(change);
  change();
  db.revision += 1;
}

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

/** A gate that stays shut until `open()`. */
function gate() {
  const shut = Promise.withResolvers<void>();
  return { wait: shut.promise, open: () => shut.resolve() };
}

it("keeps another window's rename when this window changes the same Mono from a stale copy", async () => {
  storeRoster();
  const here = await start();

  here.saveMonoSessionId("giga-1", "chat-a");
  // The rename lands while this window's first write is still queued.
  otherWindow(() => {
    const row = db.rows.get("mono-2")!;
    row.data.name = "Pinch";
    row.name = "Pinch";
  });
  here.saveMonoSessionId("mono-2", "chat-b");
  await here.monoWritesSettled();

  expect(db.rows.get("mono-2")).toMatchObject({
    name: "Pinch",
    data: { name: "Pinch", sessionId: "chat-b", mascot: "crab" },
  });
  expect(here.findMono("mono-2")).toMatchObject({
    name: "Pinch",
    sessionId: "chat-b",
  });
  expect(here.findMono("giga-1")?.sessionId).toBe("chat-a");
});

it("never brings back a Mono that another window deleted before this window heard of it", async () => {
  storeRoster();
  const here = await start();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  otherWindow(() => db.rows.delete("mono-3"), { silent: true });
  here.saveMonoSessionId("mono-3", "chat-9");
  await here.monoWritesSettled();

  expect(db.rows.has("mono-3")).toBe(false);
  expect(here.listMonos().map((entry) => entry.id)).toEqual([
    "giga-1",
    "mono-2",
  ]);
  error.mockRestore();
});

it("after a failed write, never goes back to a roster older than one already shown", async () => {
  storeRoster();
  const here = await start();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  const list = gate();
  db.failUpdate = true;
  db.listGate = list.wait;

  here.saveMonoSessionId("giga-1", "chat-x");
  // Let the update fail and the reload take its snapshot.
  await vi.waitFor(() => expect(error).toHaveBeenCalled());
  otherWindow(() => db.rows.delete("mono-3"));
  expect(here.listMonos().map((entry) => entry.id)).not.toContain("mono-3");

  list.open();
  await here.monoWritesSettled();
  expect(here.listMonos().map((entry) => entry.id)).toEqual([
    "giga-1",
    "mono-2",
  ]);
  // The failed change is dropped: the database decides.
  expect(here.findMono("giga-1")?.sessionId).toBe("chat-1");
  error.mockRestore();
});

it("keeps showing its own change while it is being saved, whatever other windows send", async () => {
  storeRoster();
  const here = await start();
  const update = gate();
  db.updateGate = update.wait;

  here.saveMonoName("mono-3", "Casper");
  otherWindow(() => {
    const row = db.rows.get("mono-2")!;
    row.data.name = "Pinch";
    row.name = "Pinch";
  });
  expect(here.findMono("mono-3")?.name).toBe("Casper");

  update.open();
  await here.monoWritesSettled();
  expect(here.listMonos().map((entry) => entry.name)).toEqual([
    "Boss",
    "Pinch",
    "Casper",
  ]);
});

it("in fallback, keeps the stored roster when another window saves to the database", async () => {
  storeRoster();
  db.broken = true;
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  const here = await start();
  db.broken = false;

  otherWindow(() =>
    db.rows.set("mono-9", {
      id: "mono-9",
      name: "Nine",
      role: "mono",
      data: { mascot: "crab", color: "#000", projects: [] },
      sortOrder: 0,
    }),
  );
  expect(here.listMonos()).toEqual(STORED);
  error.mockRestore();
});

it("keeps to the database's Mono id rule, so it never shows a Mono the database refuses", async () => {
  storeRoster([
    STORED[0],
    { ...STORED[1], id: "mono_2" },
    { ...STORED[2], id: "a".repeat(65) },
    { ...STORED[2], id: "a".repeat(64) },
  ]);
  const here = await start();
  expect(here.listMonos().map((entry) => entry.id)).toEqual([
    "giga-1",
    "a".repeat(64),
  ]);
  expect([...db.rows.keys()]).toEqual(["giga-1", "a".repeat(64)]);
});

it("drops a change the database refused and shows the saved roster again", async () => {
  storeRoster();
  const here = await start();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  db.failUpdate = true;

  here.saveMonoName("mono-3", "Casper");
  await here.monoWritesSettled();
  expect(here.findMono("mono-3")?.name).toBeUndefined();
  expect(here.listMonos()).toEqual(STORED);
  error.mockRestore();
});
