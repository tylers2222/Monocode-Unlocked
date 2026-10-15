// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  addMonoProject,
  createMono,
  findMono,
  listMonos,
  monoLook,
  monoProjectsPhrase,
  monoWorksOn,
  removeMono,
  removeMonoProject,
  reorderMonos,
  saveMonoGiga,
} from "./mono";
import { monoBackgroundKey, monoChatBackground } from "./monoBackground";
import { saveProjectChatBackgroundSettings } from "../../projects/model/projectChatBackground";

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

it("gives each new Mono a mascot and color the others are not using", () => {
  const first = createMono();
  const second = createMono();
  expect(second.mascot).not.toBe(first.mascot);
  expect(second.color).not.toBe(first.color);
  expect(monoLook(first).name).toMatch(/^Mono[A-Z]/);
  expect(first.projects).toEqual([]);
});

it("adds each project once and takes one away by its path", () => {
  const { id } = createMono(["/code/app"]);
  addMonoProject(id, "/code/site");
  addMonoProject(id, "/code/app/");
  expect(findMono(id)?.projects).toEqual(["/code/app", "/code/site"]);
  expect(monoWorksOn(findMono(id)!, "/code/site/")).toBe(true);
  removeMonoProject(id, "/code/app/");
  expect(findMono(id)?.projects).toEqual(["/code/site"]);
});

it("keeps the rail's order and forgets a removed Mono", () => {
  const a = createMono();
  const b = createMono();
  const c = createMono();
  reorderMonos([c.id, a.id, b.id]);
  expect(listMonos().map((mono) => mono.id)).toEqual([c.id, a.id, b.id]);
  removeMono(a.id);
  expect(listMonos().map((mono) => mono.id)).toEqual([c.id, b.id]);
});

it("names its projects the way a sentence would", () => {
  const project = (name: string) => ({ path: `/code/${name}`, name });
  expect(monoProjectsPhrase([])).toBe("");
  expect(monoProjectsPhrase([project("app")])).toBe("app");
  expect(
    monoProjectsPhrase([project("app"), project("site"), project("api")]),
  ).toBe("app, site and api");
});

it("forgets its background along with the Mono", () => {
  const mono = createMono();
  saveProjectChatBackgroundSettings(monoBackgroundKey(mono.id), {
    path: "/bg/mono.png",
    emptyOpacity: 0.24,
    sessionOpacity: 0.24,
    scope: "all",
    effect: "gradient-blur",
  });
  expect(monoChatBackground(mono.id)?.path).toBe("/bg/mono.png");
  removeMono(mono.id);
  expect(monoChatBackground(mono.id)).toBeNull();
});

it("keeps the Giga setting through a roster round trip, and reads none as not Giga", () => {
  const giga = createMono();
  const plain = createMono();
  saveMonoGiga(giga.id, true);
  const stored = JSON.parse(localStorage.getItem("monocode:mono-roster")!);
  expect(stored[0].giga).toBe(true);
  expect("giga" in stored[1]).toBe(false);
  expect(findMono(giga.id)?.giga).toBe(true);
  expect(findMono(plain.id)?.giga).toBeUndefined();
  expect(monoLook(findMono(giga.id)!).giga).toBe(true);

  // Only true makes a Giga Mono; anything else in storage reads as not Giga.
  localStorage.setItem(
    "monocode:mono-roster",
    JSON.stringify([
      { ...stored[0], giga: "yes" },
      { ...stored[1], giga: true },
    ]),
  );
  expect(listMonos().map((mono) => mono.giga)).toEqual([undefined, true]);

  saveMonoGiga(plain.id, false);
  expect(findMono(plain.id)?.giga).toBeUndefined();
});
