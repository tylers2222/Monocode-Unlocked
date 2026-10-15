// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  __monosTestReset,
  addMonoProject,
  createMono,
  defaultMonoName,
  findMono,
  listMonos,
  monoLook,
  monoProjectsPhrase,
  monoWorksOn,
  removeMono,
  removeMonoProject,
  MONO_NAME_MAX,
  reorderMonos,
  saveMonoGiga,
} from "./mono";
import { monoBackgroundKey, monoChatBackground } from "./monoBackground";
import { saveProjectChatBackgroundSettings } from "../../projects/model/projectChatBackground";
import { PROJECT_MASCOTS } from "../../projects/model/projectMascots";
import DEFAULT_NAMES from "./monoDefaultNames.json";
import CUSTOM_NAMES from "./monoCustomNames.json";

beforeEach(() => {
  __monosTestReset();
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

it("turns Giga on for one Mono only, and off again", () => {
  const giga = createMono();
  const plain = createMono();
  saveMonoGiga(giga.id, true);
  expect(findMono(giga.id)?.giga).toBe(true);
  expect(findMono(plain.id)?.giga).toBeUndefined();
  expect(monoLook(findMono(giga.id)!).giga).toBe(true);

  saveMonoGiga(giga.id, false);
  expect(findMono(giga.id)?.giga).toBeUndefined();
});

it("names every mascot the way the database does", () => {
  // `monoDefaultNames.json` is also checked by `mono_roster.rs`, which derives
  // `monos.name` from it; a new mascot must be added there too.
  expect(
    Object.fromEntries(
      PROJECT_MASCOTS.map(({ name }) => [name, defaultMonoName(name)]),
    ),
  ).toEqual(DEFAULT_NAMES);
});

it("trims and limits a custom name the way the database does", () => {
  // `monoCustomNames.json` is also checked by `mono_roster.rs`. A name the two
  // trim differently shows one name in the app and another in `monos.name`.
  expect(MONO_NAME_MAX).toBe(CUSTOM_NAMES.maxChars);
  const shown = CUSTOM_NAMES.cases.map(([name]) => {
    const mono = { ...createMono(), mascot: "crab", name: name! };
    return monoLook(mono).name;
  });
  expect(shown).toEqual(
    CUSTOM_NAMES.cases.map(([, expected]) => expected ?? "MonoCrab"),
  );
});
