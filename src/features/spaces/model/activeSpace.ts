import { listen } from "@tauri-apps/api/event";
import type { Mono, MonoState } from "../../monos/model/mono";
import {
  notifyArchivedProjectsChanged,
  notifyProjectPathsChanged,
} from "../../projects/model/recents";
import { DEFAULT_PROVIDER_ACCOUNT_ID } from "../../providers/model/providerAccounts";
import {
  FIRST_SPACE,
  getSpaceSettings,
  listSpaceLogins,
  listSpaceProjects,
  listSpaces,
  setSpaceSettings,
  SPACE_PROJECTS_CHANGED,
  type Space,
  type SpaceLogin,
  type SpaceLoginProvider,
} from "./spaces";
import type { SpaceProjectState } from "./spaceProjects";

/**
 * The one model of the active Space: the Spaces, which one the window shows,
 * each Space's logins, and the projects of the Space it shows.
 * `space_settings.active_space_id` in monocode.db is where the choice is
 * saved; the window reads it back at launch.
 */
export type SpacesView = {
  /** False until a read from monocode.db succeeds. */
  loaded: boolean;
  spaces: readonly Space[];
  activeSpaceId: string;
  logins: readonly SpaceLogin[];
  /** The shown Space's projects; it changes together with `activeSpaceId`. */
  projectList: SpaceProjectState;
};

const UNREAD: SpacesView = {
  loaded: false,
  spaces: [],
  activeSpaceId: FIRST_SPACE,
  logins: [],
  projectList: "loading",
};
let view = UNREAD;
let loading: Promise<void> | undefined;
/** Replaced on each saved switch; a read that began before it keeps it. */
let lastSwitch: object = {};
/** Replaced on each list read; only the latest may answer (T13). */
let listRequest: object = {};
/** The Space the latest list read is for, until it answers. */
let wantedSpace: string | undefined;
/** Set while the window listens for project changes; cleared when it cannot. */
let listening: Promise<unknown> | undefined;
const listeners = new Set<() => void>();

function publish(next: SpacesView): void {
  view = next;
  for (const listener of listeners) listener();
}

export function spacesView(): SpacesView {
  return view;
}

/** For suites that import this module statically. */
export function __spacesTestReset(): void {
  view = UNREAD;
  loading = undefined;
  listRequest = {};
  wantedSpace = undefined;
  listening = undefined;
}

/** Subscribes; while no read has succeeded, each subscriber starts one. */
export function subscribeSpaces(listener: () => void): () => void {
  listeners.add(listener);
  listenForProjectChanges();
  if (!view.loaded) void loadSpaces();
  else if (view.projectList === "failed") void refreshProjectList();
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Reads the Spaces, the active one, every Space's logins and the shown Space's
 * projects. A switch saved while it reads stays.
 */
export function loadSpaces(): Promise<void> {
  loading ??= readSpaces(lastSwitch);
  return loading;
}

function readSpaces(seen: object): Promise<void> {
  return Promise.all([listSpaces(), getSpaceSettings(), listSpaceLogins()])
    .then(([spaces, settings, logins]) => {
      const activeSpaceId =
        lastSwitch === seen
          ? settings.activeSpaceId
          : (wantedSpace ?? view.activeSpaceId);
      view = { ...view, loaded: true, spaces, logins };
      return readProjectList(activeSpaceId);
    })
    .catch((error) => {
      console.error("monocode: could not read Spaces", error);
      publish({ ...view, projectList: "failed" });
    })
    .finally(() => {
      loading = undefined;
    });
}

/**
 * Reads the list and shows it together with its Space, so a window never draws
 * one Space's projects under another. A reply older than the latest request, or
 * for a Space the window no longer wants, is dropped. A failed read shows every
 * project (T14).
 */
function readProjectList(spaceId: string): Promise<void> {
  const request = (listRequest = {});
  wantedSpace = spaceId;
  const show = (projectList: SpaceProjectState) => {
    if (request !== listRequest) return;
    wantedSpace = undefined;
    publish({ ...view, activeSpaceId: spaceId, projectList });
  };
  return listSpaceProjects()
    .then((list) => {
      if (list.spaceId === spaceId) show(list);
    })
    .catch((error: unknown) => {
      console.error("monocode: could not read the Space's projects", error);
      show("failed");
    });
}

/** Reads the list again for the Space the window shows or is switching to. */
export function refreshProjectList(): Promise<void> {
  return readProjectList(wantedSpace ?? view.activeSpaceId);
}

/** T13: recents and archive are the other half of what the window shows. */
function reloadProjectStores(): void {
  notifyProjectPathsChanged();
  notifyArchivedProjectsChanged();
}

function listenForProjectChanges(): void {
  listening ??= listen(SPACE_PROJECTS_CHANGED, () => {
    reloadProjectStores();
    if (view.loaded) void refreshProjectList();
  }).catch((error: unknown) => {
    listening = undefined;
    console.warn("monocode: no project change events", error);
  });
}

/**
 * Shows Space `id`. It only saves which Space the window shows (and where
 * new rows go): nothing running in any Space is stopped or changed. The Space
 * and its projects appear together once the list answers.
 */
export async function switchSpace(id: string): Promise<void> {
  if (id === (wantedSpace ?? view.activeSpaceId)) return;
  const settings = await setSpaceSettings({ activeSpaceId: id });
  lastSwitch = {};
  reloadProjectStores();
  await readProjectList(settings.activeSpaceId);
}

/**
 * Whether a row in Space `spaceId` shows in the window. A row the database
 * has not saved yet joins the active Space. The boot reads the Spaces before
 * the first render; if that read failed, every row shows, so no work is ever
 * hidden.
 */
export function inActiveSpace(
  spaceId: string | undefined,
  spaces: Pick<SpacesView, "loaded" | "activeSpaceId"> = view,
): boolean {
  return (
    !spaces.loaded || spaceId === undefined || spaceId === spaces.activeSpaceId
  );
}

/**
 * The login Space `spaceId` starts `provider` with, or undefined when it has
 * none. Space 1 with no row keeps the CLI's own login, as the spawn hook
 * does (`spaces::account::resolve`).
 */
export function spaceLoginId(
  logins: readonly SpaceLogin[],
  spaceId: string,
  provider: SpaceLoginProvider,
): string | undefined {
  const login = logins.find(
    (row) => row.spaceId === spaceId && row.provider === provider,
  );
  if (login) return login.accountId;
  return spaceId === FIRST_SPACE ? DEFAULT_PROVIDER_ACCOUNT_ID : undefined;
}

/** What a Space's badge counts: its Monos at work, waiting, and unread replies. */
export type SpaceActivity = { working: number; needsYou: number; unread: number };

const QUIET: SpaceActivity = { working: 0, needsYou: 0, unread: 0 };

/**
 * Each Space's activity, from the Monos' states and unseen reply counts. It
 * counts Monos only, as frame 2 does ("1 Mono working and 2 new replies"):
 * the window's open sessions carry no Space to count them by. A session's
 * reply in another Space reaches the Inbox instead.
 */
export function spaceActivity(
  monos: readonly Mono[],
  states: ReadonlyMap<string, MonoState>,
  unseenCounts: ReadonlyMap<string, number> | undefined,
): Map<string, SpaceActivity> {
  const bySpace = new Map<string, SpaceActivity>();
  for (const mono of monos) {
    if (mono.spaceId === undefined) continue;
    const was = bySpace.get(mono.spaceId) ?? QUIET;
    const status = states.get(mono.id)?.status;
    bySpace.set(mono.spaceId, {
      working: was.working + (status === "working" ? 1 : 0),
      needsYou: was.needsYou + (status === "needs-you" ? 1 : 0),
      unread: was.unread + (unseenCounts?.get(mono.id) ?? 0),
    });
  }
  return bySpace;
}

/** Whether a Space other than the active one has a reply or a Mono waiting. */
export function otherSpaceHasNews(
  activity: ReadonlyMap<string, SpaceActivity>,
  activeSpaceId: string,
): boolean {
  for (const [spaceId, counts] of activity) {
    if (spaceId !== activeSpaceId && (counts.unread > 0 || counts.needsYou > 0))
      return true;
  }
  return false;
}
