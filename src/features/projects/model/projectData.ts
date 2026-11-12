import { projectKey } from "../../../shared/lib/paths";
import { clearProjectLogo } from "./projectLogos";
import { clearProjectChatBackground } from "./chatBackground";
import {
  clearProjectChatBackgroundSetting,
  rebaseProjectChatBackgroundSetting,
} from "./projectChatBackground";
import { normalizeProjectPath } from "./recents";
import {
  clearTabGroupSettings,
  rebaseProjectTabGroupSettings,
} from "../../workspace/model/tabGroups";
import {
  rebaseProjectGroupAssignment,
  removeProjectGroupAssignment,
} from "./projectGroups";
import { rebaseSessionFolderSettings } from "../../sessions/model/sessionFolders";
import { clearProjectProviders, rebaseProjectProviders } from "../../sessions/model/projectProviders";
import {
  clearProjectSidebarTab,
  rebaseProjectSidebarTab,
} from "../../settings/model/projectSidebarTab";

/**
 * What this window stores for a project once its last Space deleted it (T11):
 * its rail appearance and settings. The backend deleted the chats and milestones
 * in its own transaction.
 */
export async function clearProjectSettings(path: string): Promise<void> {
  const normalized = normalizeProjectPath(path);
  const key = projectKey(normalized);
  // Drops the copied image from app data; the localStorage entry goes with it.
  await clearProjectLogo(key).catch(() => undefined);
  await clearProjectChatBackground(key).catch(() => undefined);
  clearProjectChatBackgroundSetting(key);
  clearTabGroupSettings(key);
  removeProjectGroupAssignment(normalized);
  clearProjectProviders(key);
  clearProjectSidebarTab(normalized);
}

/** Move local project settings after the filesystem resolver finds a rename. */
export function rebaseProjectData(from: string, to: string): void {
  const oldKey = projectKey(normalizeProjectPath(from));
  const newKey = projectKey(normalizeProjectPath(to));
  rebaseProjectTabGroupSettings(from, to);
  rebaseProjectGroupAssignment(from, to);
  rebaseProjectChatBackgroundSetting(oldKey, newKey);
  rebaseSessionFolderSettings(from, to);
  rebaseProjectProviders(oldKey, newKey);
  rebaseProjectSidebarTab(from, to);
}
