import type { Mono, MonoState } from "../../monos/model/mono";
import { DEFAULT_PROVIDER_ACCOUNT_ID } from "../../providers/model/providerAccounts";
import {
  FIRST_SPACE,
  getSpaceSettings,
  listSpaceLogins,
  listSpaces,
  projectSpace,
  setSpaceSettings,
  type Space,
  type SpaceLogin,
  type SpaceLoginProvider,
} from "./spaces";

/**
 * The one model of the active Space: the Spaces, which one the window shows,
 * each Space's logins, and the Space of each project the rail asked about.
 * `space_settings.active_space_id` in monocode.db is where the choice is
 * saved; the window reads it back at launch.
 */
export type SpacesView = {
  /** False until the first read from monocode.db; the window then shows every row. */
  loaded: boolean;
  spaces: readonly Space[];
  activeSpaceId: string;
  logins: readonly SpaceLogin[];
  /** Project path -> its Space, by the backend's rule. */
  projectSpaces: ReadonlyMap<string, string>;
};

const UNREAD: SpacesView = {
  loaded: false,
  spaces: [],
  activeSpaceId: FIRST_SPACE,
  logins: [],
  projectSpaces: new Map(),
};
let view = UNREAD;
let loading: Promise<void> | undefined;
const asked = new Set<string>();
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
  asked.clear();
}

/** Subscribes, and reads the Spaces from monocode.db until a read succeeds. */
export function subscribeSpaces(listener: () => void): () => void {
  listeners.add(listener);
  if (!view.loaded) void loadSpaces();
  return () => {
    listeners.delete(listener);
  };
}

/** Reads the Spaces, the active one and every Space's logins. */
export function loadSpaces(): Promise<void> {
  loading ??= Promise.all([listSpaces(), getSpaceSettings(), listSpaceLogins()])
    .then(([spaces, settings, logins]) =>
      publish({
        ...view,
        loaded: true,
        spaces,
        activeSpaceId: settings.activeSpaceId,
        logins,
      }),
    )
    .catch((error) => {
      console.error("monocode: could not read Spaces", error);
    })
    .finally(() => {
      loading = undefined;
    });
  return loading;
}

/**
 * Shows Space `id`. It only saves which Space the window shows (and where
 * new rows go): nothing running in any Space is stopped or changed.
 */
export async function switchSpace(id: string): Promise<void> {
  if (id === view.activeSpaceId) return;
  const settings = await setSpaceSettings({ activeSpaceId: id });
  publish({ ...view, activeSpaceId: settings.activeSpaceId });
}

/**
 * Whether a row in Space `spaceId` shows in the window. A row the database
 * has not saved yet joins the active Space, and before the first read every
 * row shows.
 */
export function inActiveSpace(
  spaceId: string | undefined,
  spaces: Pick<SpacesView, "loaded" | "activeSpaceId"> = view,
): boolean {
  return !spaces.loaded || spaceId === undefined || spaceId === spaces.activeSpaceId;
}

/** Whether project `path` shows; asks the backend for its Space once. */
export function projectInActiveSpace(
  path: string,
  spaces: SpacesView = view,
): boolean {
  if (!asked.has(path)) {
    asked.add(path);
    void projectSpace(path)
      .then((space) => {
        const projectSpaces = new Map(view.projectSpaces).set(path, space);
        publish({ ...view, projectSpaces });
      })
      .catch(() => asked.delete(path));
  }
  return inActiveSpace(spaces.projectSpaces.get(path), spaces);
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

/** Each Space's activity, from the Monos' states and unseen reply counts. */
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
