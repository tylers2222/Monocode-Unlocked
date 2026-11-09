import { invoke } from "@tauri-apps/api/core";

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

/** Archive and unarchive write only localStorage; this tells other windows. */
export function notifySpaceProjectsChanged(): void {
  // Only a courtesy to other windows: the write it reports has already landed.
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

/** The text of a typed Space refusal (`{ code, message }`), or of any failure. */
export function refusalMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    return String(error.message);
  }
  return String(error);
}

/** The Space `projectPath` belongs to, by the backend's one rule. */
export function projectSpace(projectPath: string): Promise<string> {
  return invoke<string>("spaces_project_get", { projectPath });
}
