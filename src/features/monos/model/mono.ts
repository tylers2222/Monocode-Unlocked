import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { projectKey, projectName } from "../../../shared/lib/paths";
import {
  loadTabGroupColors,
  loadTabGroupCustomColors,
  loadTabGroupLabels,
  loadTabGroupMascots,
  resolveTabGroupColor,
  resolveTabGroupLabel,
  resolveTabGroupMascot,
  TAB_GROUP_COLORS,
} from "../../workspace/model/tabGroups";
import {
  hasPendingApproval,
  type Block,
  type Session,
} from "../../sessions/model/session";
import {
  PROJECT_MASCOTS,
  projectMascot,
} from "../../projects/model/projectMascots";
import { gigaSlugFor } from "./gigaWorkspace";
import { removeMonoBackground } from "./monoBackground";

/** The project palette's colors, plus indigo for a Mono's ninth preset. */
export const MONO_COLORS = [
  ...TAB_GROUP_COLORS.slice(1),
  "hsl(245 75% 65%)",
] as const;

/**
 * A Mono is a long-lived agent of its own: a name, a mascot and a color, one
 * conversation, and the projects it works on. It is not tied to any one of
 * them; it lives on the rail beside the projects and can be given more.
 */
export type Mono = {
  id: string;
  /** Its conversation; absent until it is first opened. */
  sessionId?: string;
  /** Replaces the mascot's own name. */
  name?: string;
  mascot: string;
  color: string;
  /** Project folders it works on, in the order they were added. */
  projects: string[];
  /** New sessions stay out of the project sidebar unless turned on. */
  showStartedSessionsInSidebar?: boolean;
  /** Files the sessions it starts into a sidebar folder named after it. */
  useSidebarFolders?: boolean;
  /** Can create and configure other Monos through the app CLI. */
  giga?: true;
  /**
   * Its folder in the shared Giga workspace, `~/MonoCode/gigas/<slug>/`.
   * Set the first time Giga is turned on, and kept through renames.
   */
  gigaSlug?: string;
  /** Superseded by SOUL.md; only read once, to seed it. */
  instructions?: string;
  /**
   * From when each project had its own Mono: the project whose agent folder
   * it inherits. The folder moves under its id the first time it is read.
   */
  legacyProject?: string;
};

/** Where the roster lived before `monocode.db`; read, never written. */
const ROSTER_KEY = "monocode:mono-roster";
const MONOS_CHANGED = "monocode:monos-changed";
export const HANDED_KEY = "monocode:mono-handed";

/** Storage from when each project had one Mono, keyed by project. */
const LEGACY_AGENTS_KEY = "monocode:monos";
const LEGACY_PROFILES_KEY = "monocode:mono-profiles";
/** And from before Monos had their name. */
const LEGACY_KEYS: [string, string][] = [
  ["monocode:project-agents", LEGACY_AGENTS_KEY],
  ["monocode:project-agent-profiles", LEGACY_PROFILES_KEY],
  ["monocode:project-agent-handed", HANDED_KEY],
];

function readJson(key: string): unknown {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as unknown) : undefined;
  } catch {
    return undefined;
  }
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

let migrated = false;

/** Moves storage keys from before Monos had their name. */
export function migrateLegacyMonoStorage(): void {
  if (migrated) return;
  migrated = true;
  try {
    for (const [legacy, key] of LEGACY_KEYS) {
      const value = localStorage.getItem(legacy);
      if (value == null) continue;
      if (localStorage.getItem(key) == null) localStorage.setItem(key, value);
      localStorage.removeItem?.(legacy);
    }
  } catch {
    // Storage unavailable: there is nothing to move either.
  }
}

/**
 * The roster as WebKit storage last held it, or undefined when it holds none.
 * From before that, each project's Mono becomes one of its own that works on
 * that project, keeping its conversation, name and look; one never claimed is
 * dropped. Reads only.
 */
function legacyRoster(): Mono[] | undefined {
  try {
    const saved = readJson(ROSTER_KEY);
    if (Array.isArray(saved))
      return saved.flatMap((entry) => parseMono(entry) ?? []);
    const agents = record(readJson(LEGACY_AGENTS_KEY));
    const profiles = record(readJson(LEGACY_PROFILES_KEY));
    const projects = new Set([
      ...Object.keys(agents),
      ...Object.keys(profiles),
    ]);
    if (!projects.size) return undefined;
    const mascots = loadTabGroupMascots();
    const colors = loadTabGroupColors();
    const customColors = loadTabGroupCustomColors();
    const roster: Mono[] = [];
    for (const project of projects) {
      const sessionId = agents[project];
      const profile = record(profiles[project]);
      if (typeof sessionId !== "string" && profile.claim !== "claimed")
        continue;
      const seed = projectName(project);
      roster.push({
        id: newMonoId(),
        ...(typeof sessionId === "string" ? { sessionId } : {}),
        ...(typeof profile.name === "string" && profile.name.trim()
          ? { name: profile.name }
          : {}),
        mascot: projectMascot(seed, resolveTabGroupMascot(project, mascots))
          .name,
        color: resolveTabGroupColor(project, colors, customColors, seed),
        projects: [project],
        ...(typeof profile.instructions === "string" &&
        profile.instructions.trim()
          ? { instructions: profile.instructions }
          : {}),
        legacyProject: project,
      });
    }
    return roster;
  } catch {
    return undefined;
  }
}

function newMonoId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function parseMono(value: unknown): Mono | undefined {
  const entry = record(value);
  // The one Mono id rule, shared with `validate_mono_id` in `mono_roster.rs`.
  if (typeof entry.id !== "string" || !/^[A-Za-z0-9-]{1,64}$/.test(entry.id))
    return undefined;
  const text = (key: string) =>
    typeof entry[key] === "string" ? (entry[key] as string) : undefined;
  const sessionId = text("sessionId");
  const name = text("name");
  const instructions = text("instructions");
  const legacyProject = text("legacyProject");
  return {
    id: entry.id,
    ...(sessionId ? { sessionId } : {}),
    ...(name ? { name } : {}),
    mascot: text("mascot") ?? PROJECT_MASCOTS[0].name,
    color: text("color") ?? TAB_GROUP_COLORS[1],
    projects: Array.isArray(entry.projects)
      ? entry.projects.filter(
          (path): path is string => typeof path === "string" && !!path,
        )
      : [],
    ...(typeof entry.showStartedSessionsInSidebar === "boolean"
      ? { showStartedSessionsInSidebar: entry.showStartedSessionsInSidebar }
      : {}),
    ...(entry.useSidebarFolders === true ? { useSidebarFolders: true } : {}),
    ...(entry.giga === true ? { giga: true } : {}),
    ...(typeof entry.gigaSlug === "string" &&
    /^[a-z0-9][a-z0-9-]{0,47}$/.test(entry.gigaSlug)
      ? { gigaSlug: entry.gigaSlug }
      : {}),
    ...(instructions ? { instructions } : {}),
    ...(legacyProject ? { legacyProject } : {}),
  };
}

/** A Mono's record without the parts `monocode.db` keeps in columns. */
type MonoData = Omit<Mono, "id" | "giga">;

/**
 * A new Mono as `monocode.db` stores it; `data` is the rest of its record.
 * No name: the database derives `monos.name` from `data` alone.
 */
type MonoCreate = {
  id: string;
  role: "giga" | "mono";
  data: MonoData;
  sortOrder: number;
};

/**
 * What changed on one Mono. `patch` is a JSON merge patch of its data: the
 * changed keys, and null for each removed one.
 */
type MonoUpdate = {
  id: string;
  role?: "giga" | "mono";
  patch?: Record<string, unknown>;
  sortOrder?: number;
};

/** The most code points a custom name may have, as `NAME_MAX` in `mono_roster.rs`. */
export const MONO_NAME_MAX = 256;

/**
 * A Mono's own name without its outer whitespace, or undefined when it has
 * none. The set is JS `\s` plus U+0085, the same set `custom_name` in
 * `mono_roster.rs` trims; both are checked against `monoCustomNames.json`.
 */
export function customMonoName(name: string | undefined): string | undefined {
  return name?.replace(/^[\s\u0085]+|[\s\u0085]+$/g, "") || undefined;
}

/** The name a Mono goes by: the user's, or its mascot's default. */
function monoDisplayName(mono: Pick<Mono, "name" | "mascot">): string {
  return customMonoName(mono.name) ?? defaultMonoName(mono.mascot);
}

function monoCreate(mono: Mono, sortOrder: number): MonoCreate {
  const { id, giga, ...data } = mono;
  return { id, role: giga ? "giga" : "mono", data, sortOrder };
}

function dataPatch(
  before: Record<string, unknown>,
  after: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const patch: Record<string, unknown> = {};
  for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) {
    if (JSON.stringify(before[key]) !== JSON.stringify(after[key]))
      patch[key] = after[key] ?? null;
  }
  return Object.keys(patch).length ? patch : undefined;
}

/** Only what differs, so another window's change to other fields survives. */
function monoUpdate(
  before: Mono,
  beforeOrder: number,
  after: Mono,
  sortOrder: number,
): MonoUpdate | undefined {
  const { id: _, giga: wasGiga, ...was } = before;
  const { id, giga, ...data } = after;
  const patch = dataPatch(was, data);
  const update: MonoUpdate = {
    id,
    ...(patch ? { patch } : {}),
    ...(!wasGiga !== !giga ? { role: giga ? "giga" : "mono" } : {}),
    ...(sortOrder !== beforeOrder ? { sortOrder } : {}),
  };
  return Object.keys(update).length > 1 ? update : undefined;
}

/** The whole roster as the database left it after one write. */
type RosterSnapshot = { revision: number; monos: Mono[] };

/** A malformed answer from the database is an error, never an empty roster. */
function parseSnapshot(value: unknown): RosterSnapshot {
  const snapshot = record(value);
  if (typeof snapshot.revision !== "number" || !Array.isArray(snapshot.monos))
    throw new Error("Malformed Mono roster from monocode.db");
  return {
    revision: snapshot.revision,
    monos: snapshot.monos.flatMap((value) => {
      const row = record(value);
      return (
        parseMono({
          ...record(row.data),
          id: row.id,
          ...(row.role === "giga" ? { giga: true } : {}),
        }) ?? []
      );
    }),
  };
}

const copyMono = (mono: Mono): Mono => ({
  ...mono,
  projects: [...mono.projects],
});

/**
 * What every reader sees. `main.tsx` loads it from `monocode.db` before the
 * first render. Until then, or when that load failed, it lives only here.
 */
let roster: Mono[] = [];
let rosterVersion = 0;
/** Writes go to the database only once the roster came from it. */
let persisted = false;
/** The database revision `roster` shows, once no write of ours is in flight. */
let revision = -1;
let writing = 0;
/** The newest snapshot seen while our own writes were in flight. */
let newest: RosterSnapshot | undefined;
/** A write failed, so the database decides what the roster is. */
let resync = false;
let writes = Promise.resolve();

function showRoster(monos: readonly Mono[]): void {
  roster = monos.map(copyMono);
  rosterVersion += 1;
  window.dispatchEvent(new CustomEvent(MONOS_CHANGED));
}

/**
 * The highest revision wins, whatever order snapshots arrive in. While our own
 * writes are in flight the roster keeps showing them, and the newest snapshot
 * waits until they are done.
 */
function adopt(snapshot: RosterSnapshot): void {
  if (snapshot.revision <= revision) return;
  if (writing > 0) {
    if (!newest || snapshot.revision > newest.revision) newest = snapshot;
    return;
  }
  revision = snapshot.revision;
  showRoster(snapshot.monos);
}

async function settle(): Promise<void> {
  if (resync) {
    resync = false;
    try {
      const listed = parseSnapshot(await invoke("monos_list"));
      // A write sent since then settles again and lists again.
      if (writing > 0) {
        resync = true;
        return;
      }
      // The same revision drops the failed change; a lower one is older than
      // a roster already shown.
      if (listed.revision >= revision) {
        revision = listed.revision;
        showRoster(listed.monos);
      }
    } catch (error) {
      console.error("monocode: could not reload Monos", error);
    }
  }
  const snapshot = newest;
  newest = undefined;
  if (snapshot) adopt(snapshot);
}

/**
 * Sends the database what changed between two rosters, as operations: a
 * create never replaces, an update never inserts and carries only what
 * changed, a delete deletes. The updates go in one batch, so a reorder lands
 * whole or not at all: when one Mono in it is gone the database refuses the
 * batch, and the reload after the failure shows the database's order.
 */
function persist(before: readonly Mono[], after: readonly Mono[]): void {
  const was = new Map(before.map((mono, index) => [mono.id, { mono, index }]));
  const kept = new Set(after.map((mono) => mono.id));
  const deletes = before.flatMap((mono) =>
    kept.has(mono.id) ? [] : [mono.id],
  );
  const creates: MonoCreate[] = [];
  const updates: MonoUpdate[] = [];
  after.forEach((mono, index) => {
    const old = was.get(mono.id);
    if (!old) creates.push(monoCreate(mono, index));
    else {
      const update = monoUpdate(old.mono, old.index, mono, index);
      if (update) updates.push(update);
    }
  });
  if (!deletes.length && !creates.length && !updates.length) return;
  writing += 1;
  writes = writes.then(async () => {
    try {
      for (const id of deletes)
        adopt(parseSnapshot(await invoke("monos_delete", { id })));
      for (const mono of creates)
        adopt(parseSnapshot(await invoke("monos_create", { mono })));
      if (updates.length)
        adopt(parseSnapshot(await invoke("monos_update", { updates })));
    } catch (error) {
      console.error("monocode: could not save Monos", error);
      resync = true;
    } finally {
      writing -= 1;
    }
    if (writing === 0) await settle();
  });
}

function saveRoster(next: readonly Mono[]): void {
  const before = roster;
  showRoster(next);
  if (persisted) persist(before, next);
}

/**
 * Loads the roster from `monocode.db`, once, before the first render. The
 * first time, it imports the roster WebKit storage held. When the database
 * cannot be read, that stored roster shows instead, other windows' changes do
 * not replace it, and changes last until the app quits: the old key is never
 * written again.
 */
export async function loadMonoRoster(): Promise<void> {
  migrateLegacyMonoStorage();
  const legacy = legacyRoster();
  let unlisten: (() => void) | undefined;
  try {
    unlisten = await listen("monos_changed", (event) => {
      try {
        adopt(parseSnapshot(event.payload));
      } catch (error) {
        console.error("monocode: ignored a malformed Mono roster", error);
      }
    });
    const loaded = parseSnapshot(
      legacy
        ? await invoke("monos_import_legacy", {
            monos: legacy.map(monoCreate),
          })
        : await invoke("monos_list"),
    );
    persisted = true;
    adopt(loaded);
  } catch (error) {
    unlisten?.();
    console.error(
      "monocode: could not load Monos from monocode.db; showing the saved list",
      error,
    );
    showRoster(legacy ?? []);
  }
}

/** Resolves once every roster write sent so far has been answered. */
export function monoWritesSettled(): Promise<void> {
  return writes;
}

/**
 * For suites that import this module statically. `rosterVersion` keeps
 * counting, so a snapshot from before the reset never matches one after.
 */
export function __monosTestReset(): void {
  migrated = false;
  roster = [];
  persisted = false;
  revision = -1;
  writing = 0;
  newest = undefined;
  resync = false;
  writes = Promise.resolve();
}

/** Every Mono, in the order the rail shows them. */
export function listMonos(): Mono[] {
  return roster.map(copyMono);
}

export function findMono(id: string): Mono | undefined {
  return listMonos().find((mono) => mono.id === id);
}

/** The Mono whose conversation this is. */
export function monoForSession(sessionId: string): Mono | undefined {
  return listMonos().find((mono) => mono.sessionId === sessionId);
}

export function isMonoSession(sessionId: string): boolean {
  return !!monoForSession(sessionId);
}

/**
 * The mascot and color the next Mono gets: ones the others are not using yet,
 * where any are left.
 */
export function nextMonoLook(
  roster: readonly Pick<Mono, "mascot" | "color">[] = listMonos(),
): { mascot: string; color: string } {
  const mascots = new Set(roster.map((mono) => mono.mascot));
  const colors = new Set(roster.map((mono) => mono.color));
  const palette = MONO_COLORS;
  return {
    mascot: (
      PROJECT_MASCOTS.find((mascot) => !mascots.has(mascot.name)) ??
      PROJECT_MASCOTS[roster.length % PROJECT_MASCOTS.length]
    ).name,
    color:
      palette.find((color) => !colors.has(color)) ??
      palette[roster.length % palette.length],
  };
}

/**
 * A new Mono with the next look, starting with the projects given, if any,
 * and the name given.
 */
export function createMono(
  projects: readonly string[] = [],
  settings: { name?: string } = {},
): Mono {
  const roster = listMonos();
  const name = customMonoName(settings.name);
  const mono: Mono = {
    id: newMonoId(),
    ...(name ? { name } : {}),
    ...nextMonoLook(roster),
    projects: uniqueProjects(projects),
  };
  saveRoster([...roster, mono]);
  return mono;
}

const OPEN_MONO_KEY = "monocode:open-mono";

/** The Mono whose conversation was open, so the next launch reopens it. */
export function loadOpenMono(): string | undefined {
  try {
    return localStorage.getItem(OPEN_MONO_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

export function saveOpenMono(monoId: string | undefined): void {
  try {
    if (monoId) localStorage.setItem(OPEN_MONO_KEY, monoId);
    else localStorage.removeItem(OPEN_MONO_KEY);
  } catch {
    // The next launch opens the workspace instead; harmless.
  }
}

function uniqueProjects(paths: readonly string[]): string[] {
  const seen = new Set<string>();
  return paths.filter((path) => {
    const key = projectKey(path);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function updateMono(
  id: string,
  change: (mono: Mono) => Mono,
): Mono | undefined {
  const roster = listMonos();
  const index = roster.findIndex((mono) => mono.id === id);
  if (index < 0) return undefined;
  const next = change(roster[index]);
  if (!customMonoName(next.name)) delete next.name;
  if (!next.instructions?.trim()) delete next.instructions;
  next.projects = uniqueProjects(next.projects);
  roster[index] = next;
  saveRoster(roster);
  return next;
}

/** Forgets the Mono and its background. Its folder of files stays on disk. */
export function removeMono(id: string): void {
  removeMonoBackground(id);
  saveRoster(listMonos().filter((mono) => mono.id !== id));
}

export function reorderMonos(ids: readonly string[]): void {
  const roster = listMonos();
  const order = new Map(ids.map((id, index) => [id, index]));
  saveRoster(
    [...roster].sort(
      (a, b) =>
        (order.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
        (order.get(b.id) ?? Number.MAX_SAFE_INTEGER),
    ),
  );
}

export function saveMonoSessionId(monoId: string, sessionId: string): void {
  updateMono(monoId, (mono) => ({ ...mono, sessionId }));
}

export function saveMonoName(monoId: string, name: string): void {
  updateMono(monoId, (mono) => ({ ...mono, name }));
}

export function saveMonoMascot(monoId: string, mascot: string): void {
  updateMono(monoId, (mono) => ({ ...mono, mascot }));
}

/**
 * Turning Giga on gives the Mono its workspace slug (once) and the shared
 * workspace as its first project, and files the sessions it starts into its
 * own sidebar folder. Turning it off keeps the slug and the projects.
 */
export function saveMonoGiga(
  monoId: string,
  giga: boolean,
  workspaceRoot?: string,
): void {
  const taken = listMonos()
    .filter((mono) => mono.id !== monoId)
    .flatMap((mono) => (mono.gigaSlug ? [mono.gigaSlug] : []));
  updateMono(monoId, ({ giga: _, ...mono }) =>
    giga
      ? {
          ...mono,
          giga: true,
          gigaSlug: mono.gigaSlug ?? gigaSlugFor(monoLook(mono).name, taken),
          projects: workspaceRoot
            ? [workspaceRoot, ...mono.projects]
            : mono.projects,
          showStartedSessionsInSidebar: true,
          useSidebarFolders: true,
        }
      : mono,
  );
}

export function addMonoProject(monoId: string, path: string): void {
  updateMono(monoId, (mono) => ({
    ...mono,
    projects: [...mono.projects, path],
  }));
}

export function removeMonoProject(monoId: string, path: string): void {
  const key = projectKey(path);
  updateMono(monoId, (mono) => ({
    ...mono,
    projects: mono.projects.filter((project) => projectKey(project) !== key),
  }));
}

/** Whether the Mono has been given this project. */
export function monoWorksOn(mono: Pick<Mono, "projects">, path: string) {
  const key = projectKey(path);
  return mono.projects.some((project) => projectKey(project) === key);
}

export function subscribeMonos(onChange: () => void): () => void {
  window.addEventListener(MONOS_CHANGED, onChange);
  return () => window.removeEventListener(MONOS_CHANGED, onChange);
}

/**
 * Snapshot for `useSyncExternalStore`; stable while storage is unchanged.
 * Project labels are part of it, since a Mono's look names its projects.
 */
export function monosSnapshot(): string {
  try {
    return `${rosterVersion}|${JSON.stringify(loadTabGroupLabels())}`;
  } catch {
    return `${rosterVersion}`;
  }
}

/** Mascots whose Mono name is shorter than the mascot's. */
const MONO_NAME: Record<string, string> = { mushroom: "Shroom" };

/** The name a Mono goes by until the user names it: "MonoCrab", "MonoShroom". */
export function defaultMonoName(mascot: string): string {
  return `Mono${MONO_NAME[mascot] ?? mascot.charAt(0).toUpperCase() + mascot.slice(1)}`;
}

/** One project a Mono works on, as the rail labels it. */
export type MonoProject = { path: string; name: string };

export type MonoLook = {
  /** The user's name for it, or its default: "MonoInvader", "MonoGhost". */
  name: string;
  mascot: string;
  color: string;
  projects: MonoProject[];
  /** It can create and configure other Monos. */
  giga?: true;
  gigaSlug?: string;
};

/** How a Mono looks, and the projects it works on by their rail labels. */
export function monoLook(mono: Mono): MonoLook {
  const labels = loadTabGroupLabels();
  return {
    name: monoDisplayName(mono),
    mascot: mono.mascot,
    color: mono.color,
    projects: mono.projects.map((path) => ({
      path,
      name: resolveTabGroupLabel(projectKey(path), labels, projectName(path)),
    })),
    ...(mono.giga ? { giga: true } : {}),
    ...(mono.giga && mono.gigaSlug ? { gigaSlug: mono.gigaSlug } : {}),
  };
}

/** Its projects as a phrase: "app", "app and site", "app, site and api". */
export function monoProjectsPhrase(projects: readonly MonoProject[]): string {
  const names = projects.map((project) => project.name);
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** Plain one-line take on a step's title, for the Mono's status. */
function plainLine(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s*(?:#{1,6}|>)\s*/gm, "")
    .replace(/[`*_]/g, "")
    .replace(/^\s*[-\d.]+\s+/gm, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** What the agent is doing right now, as the Monos roster shows it. */
export type MonoStatus = "working" | "needs-you" | "idle";

export type MonoState = {
  status: MonoStatus;
  /** Short present-tense note while working or waiting. */
  activity?: string;
};

export const MONO_STATUS_LABEL: Record<MonoStatus, string> = {
  working: "Working",
  "needs-you": "Needs you",
  idle: "Idle",
};

/** The agent's state, with the step it is on or the call it is waiting for. */
export function monoState(
  session: Pick<
    Session,
    "blocks" | "busy" | "pendingQuestion" | "worktreeRemoved" | "usageLimit"
  >,
): MonoState {
  const { blocks, pendingQuestion: question } = session;
  if (!session.worktreeRemoved && question) {
    return {
      status: "needs-you",
      activity: question.title?.trim() || question.questions[0]?.prompt.trim(),
    };
  }
  if (!session.worktreeRemoved && hasPendingApproval(blocks)) {
    const approval = latest(
      blocks,
      (block) => !!block.approval && !block.approval.decided,
    );
    return {
      status: "needs-you",
      activity: approval?.tool?.title
        ? `Approve ${plainLine(approval.tool.title)}`
        : "Waiting for approval",
    };
  }
  if (!session.worktreeRemoved && session.usageLimit) {
    return { status: "needs-you", activity: "Usage limit reached" };
  }
  if (!session.busy) return { status: "idle" };
  const last = blocks[blocks.length - 1];
  if (last?.role === "assistant" && !last.tool && last.streaming) {
    return { status: "working", activity: "Writing a reply" };
  }
  // Only a step from the current turn says what it is doing now.
  for (let i = blocks.length - 1; i >= 0; i--) {
    const block = blocks[i];
    if (block.role === "user") break;
    if (block.tool?.title) {
      return { status: "working", activity: plainLine(block.tool.title) };
    }
  }
  return { status: "working", activity: "Thinking" };
}

function latest(
  blocks: readonly Block[],
  match: (block: Block) => boolean,
): Block | undefined {
  for (let i = blocks.length - 1; i >= 0; i--) {
    if (match(blocks[i])) return blocks[i];
  }
  return undefined;
}
