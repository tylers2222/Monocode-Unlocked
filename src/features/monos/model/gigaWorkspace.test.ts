// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  gigaSlugFor,
  receiptLine,
  recordTurnCall,
  type TurnCalls,
} from "./gigaWorkspace";
import { createMono, findMono, saveMonoGiga, saveMonoName } from "./mono";

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

// Defect: a receipt with a missing, extra, renamed or reordered field, an
// `asked` that is not cut at 200 characters or splits a character in two, or
// a `files` list that is not the turn's own.
it("writes a receipt line with exactly the contract's fields", () => {
  const asked = `${"a".repeat(199)}😀tail`;
  const line = receiptLine({
    at: 1792000000000,
    monoId: "m1",
    slug: "monocode-general",
    sessionId: "s1",
    kind: "habit",
    asked,
    outcome: "cancelled",
    calls: {
      sent: [{ requestId: "app-s1-r1", toMonoId: "m2" }],
      started: [{ requestId: "app-s1-r2", sessionId: "app-s1-r2" }],
    },
    files: ["gigas/monocode-general/briefs/plan.md"],
  });
  expect(line).not.toContain("\n");
  expect(line).toBe(
    JSON.stringify({
      v: 1,
      at: 1792000000000,
      giga: { monoId: "m1", slug: "monocode-general" },
      sessionId: "s1",
      kind: "habit",
      asked: `${"a".repeat(199)}😀`,
      outcome: "cancelled",
      sent: [{ requestId: "app-s1-r1", toMonoId: "m2" }],
      started: [{ requestId: "app-s1-r2", sessionId: "app-s1-r2" }],
      files: ["gigas/monocode-general/briefs/plan.md"],
    }),
  );
});

// Defect: the receipt names calls that are not sends or starts, or names a
// retried call twice.
it("records each monos.send and sessions.start of a turn once", () => {
  let calls: TurnCalls = { sent: [], started: [] };
  calls = recordTurnCall(calls, "monos.send", "app-s-1", { monoId: "m2" });
  calls = recordTurnCall(calls, "monos.send", "app-s-1", { monoId: "m2" });
  calls = recordTurnCall(calls, "sessions.start", "app-s-2", {
    id: "app-s-2",
  });
  calls = recordTurnCall(calls, "notes.write", "app-s-3", { id: "n" });
  expect(calls).toEqual({
    sent: [{ requestId: "app-s-1", toMonoId: "m2" }],
    started: [{ requestId: "app-s-2", sessionId: "app-s-2" }],
  });
});

// Defect: a slug outside [a-z0-9-], longer than 48, or the same as another
// Mono's, which would put two Gigas in one folder.
it("makes a slug from the name, at most 48 characters, unique among Monos", () => {
  expect(gigaSlugFor("Monocode General", [])).toBe("monocode-general");
  expect(gigaSlugFor("  Café — Ops!! ", [])).toBe("cafe-ops");
  expect(gigaSlugFor("!!!", [])).toBe("giga");
  expect(gigaSlugFor("ops", ["ops", "ops-2"])).toBe("ops-3");
  const long = gigaSlugFor("x".repeat(60), ["x".repeat(48)]);
  expect(long).toBe(`${"x".repeat(46)}-2`);
});

// Defect: turning Giga on drops the Mono's projects, does not put the
// workspace first, or a rename moves the Giga to a new folder.
it("turning Giga on stores the slug once and puts the workspace first", () => {
  const other = createMono();
  saveMonoName(other.id, "Ops");
  saveMonoGiga(other.id, true, "/home/u/MonoCode");
  const mono = createMono(["/code/app", "/home/u/MonoCode"]);
  saveMonoName(mono.id, "Ops");
  saveMonoGiga(mono.id, true, "/home/u/MonoCode");
  expect(findMono(mono.id)).toMatchObject({
    giga: true,
    gigaSlug: "ops-2",
    projects: ["/home/u/MonoCode", "/code/app"],
    useSidebarFolders: true,
    showStartedSessionsInSidebar: true,
  });
  saveMonoName(mono.id, "Renamed");
  saveMonoGiga(mono.id, false);
  saveMonoGiga(mono.id, true, "/home/u/MonoCode");
  expect(findMono(mono.id)?.gigaSlug).toBe("ops-2");
});
