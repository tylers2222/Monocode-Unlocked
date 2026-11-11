import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** The Space every row predates Spaces in, and a project with no row is in. */
export const FIRST_SPACE = "space-1";

/** The CLIs a Space keeps one login for, in the order a Space row lists them. */
export const SPACE_LOGIN_PROVIDERS = ["claude", "codex"] as const;
export type SpaceLoginProvider = (typeof SPACE_LOGIN_PROVIDERS)[number];

export type Space = {
  id: string;
  name: string;
  color: string | null;
  createdAt: number;
};

export type SpaceSettings = {
  activeSpaceId: string;
  routeGrantTtlMs: number;
};

/** A Space's login for one CLI (`space_accounts`). */
export type SpaceLogin = {
  spaceId: string;
  provider: string;
  accountId: string;
};

export function listSpaces(): Promise<Space[]> {
  return invoke<Space[]>("spaces_list");
}

export function createSpace(name: string, color?: string): Promise<Space> {
  return invoke<Space>("spaces_create", { name, color: color ?? null });
}

export function updateSpace(
  id: string,
  name: string,
  color: string | null,
): Promise<void> {
  return invoke("spaces_update", { id, name, color });
}

export function listSpaceLogins(): Promise<SpaceLogin[]> {
  return invoke<SpaceLogin[]>("spaces_logins_list");
}

export function setSpaceLogin(login: SpaceLogin): Promise<void> {
  return invoke("spaces_login_set", { login });
}

export function getSpaceSettings(): Promise<SpaceSettings> {
  return invoke<SpaceSettings>("spaces_settings_get");
}

export function setSpaceSettings(
  patch: Partial<SpaceSettings>,
): Promise<SpaceSettings> {
  return invoke<SpaceSettings>("spaces_settings_set", { patch });
}

/** A project with a stored fact in the window's Space. */
export type SpaceProject = {
  path: string;
  assigned: boolean;
  linked: boolean;
  monos: string[];
};

/** `spaces_project_list`: the calling window's Space, in one read. */
export type SpaceProjectList = {
  spaceId: string;
  projects: SpaceProject[];
  monoHeld: string[];
  noRowIsHere: boolean;
  assignedElsewhere: string[];
};

export const SPACE_PROJECTS_CHANGED = "spaces:projects-changed";

export function listSpaceProjects(): Promise<SpaceProjectList> {
  return invoke<SpaceProjectList>("spaces_project_list");
}

/** T1 and T2: lists a project in the calling window's Space. */
export function addProjectToSpace(
  projectPath: string,
  { knownToWindow }: { knownToWindow: boolean },
): Promise<"assigned" | "linked"> {
  return invoke("spaces_project_add", { projectPath, knownToWindow });
}

/** Archive and unarchive write only localStorage; this tells other windows. */
export function notifySpaceProjectsChanged(): void {
  // Only a courtesy to other windows. Open folder sends it before its adds
  // answer; each add sends its own event, so other windows read again anyway.
  void invoke("spaces_project_notify_changed").catch(() => undefined);
}

/** What deleting a project's data from the calling window's Space would do. */
export type ProjectDeletePreview = {
  lastSpace: boolean;
  sessionCount: number;
  otherSpaces: string[];
  /** Other Spaces' chats that stay, hidden, when this delete runs. */
  hiddenSessionCount: number;
};

export function previewProjectDelete(
  projectPath: string,
): Promise<ProjectDeletePreview> {
  return invoke<ProjectDeletePreview>("spaces_project_delete_preview", {
    projectPath,
  });
}

/** Resolves when Archive may go ahead; rejects with T9's refusal. */
export function checkProjectArchive(projectPath: string): Promise<null> {
  return invoke<null>("spaces_project_archive_check", { projectPath });
}

/** A project some Space has, with those Spaces. */
export type KnownProject = { path: string; spaces: string[] };

/** Every project any Space has; no root or under-workspace path. */
export function listKnownProjects(): Promise<KnownProject[]> {
  return invoke<KnownProject[]>("spaces_project_known");
}

/** T5 or T6. False: it was not in the Space, and nothing changed. */
export function removeProjectFromSpace(projectPath: string): Promise<boolean> {
  return invoke<boolean>("spaces_project_remove", { projectPath });
}

/** Calls `onChange` after every write to any Space's projects, in any window. */
export function subscribeSpaceProjects(onChange: () => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void listen(SPACE_PROJECTS_CHANGED, onChange)
    .then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    })
    .catch(() => undefined);
  return () => {
    disposed = true;
    unlisten?.();
  };
}

/** The text of a typed Space refusal (`{ code, message }`), or of any failure. */
export function refusalMessage(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    return String(error.message);
  }
  return String(error);
}

/** The backend's workspace refusal reads "so it stays", which is Remove's. */
const ADD_WORKSPACE_REFUSAL =
  "This folder is part of the MonoCode workspace, so it can't be added to a Space.";

/**
 * A refused add in Add wording ("Add refusal text" in the decisions file):
 * "<reason>, so it can't be added to <Space>.". The workspace refusal goes by
 * its code; every other reason is the backend's message.
 */
export function addRefusalMessage(error: unknown, spaceName: string): string {
  if ((error as { code?: unknown } | null | undefined)?.code === "workspace") {
    return ADD_WORKSPACE_REFUSAL;
  }
  const reason = refusalMessage(error).trim().replace(/\.+$/, "");
  return `${reason}, so it can't be added to ${spaceName}.`;
}
