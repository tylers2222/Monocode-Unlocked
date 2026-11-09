import type { Space } from "../../spaces/model/spaces";
import { invoke } from "@tauri-apps/api/core";
import { appendReadyHandoff, buildDeterministicHandoff } from "../../sessions/model/handoff";
import {
  invokeWorkspace,
  notifyGitChanged,
  resolveProjectLocation,
} from "../../../platform/tauri/fs";
import { isFilesystemTab, type FilePaneTab } from "../../workspace/model/layout";
import { isEqualOrInside, parentPath, pathKey } from "../../../shared/lib/paths";
import { isBlankSession } from "../../projects/model/projectReturn";
import { newSession, sessionWorkCwd, type Session } from "../../sessions/model/session";

export type Worktree = {
  path: string;
  branch: string | null;
  head: string;
  isMain: boolean;
  locked: boolean;
  prunable: boolean;
  missing: boolean;
  dirty: boolean | null;
  unpushed: number | null;
  /** The calling window's Space's sessions on this worktree. */
  sessionIds: string[];
  /** Every other Space's: they refuse the delete (spec T18). */
  otherSpaceSessions?: { sessionId: string; spaceId: string }[];
};
export type Worktrees = { worktrees: Worktree[]; defaultRoot: string };

export const listWorktrees = (cwd: string) =>
  invokeWorkspace<Worktrees>("git_worktrees", { cwd });

export async function createWorktree(
  cwd: string,
  branch: string,
  base: string,
  existing: boolean,
) {
  const tree = await invoke<Worktree>("git_worktree_create", {
    cwd,
    branch,
    base,
    existing,
  });
  notifyGitChanged();
  return tree;
}

export async function createOrchestrationWorktree(
  cwd: string,
  branch: string,
) {
  const tree = await invoke<Worktree>("git_orchestration_worktree_create", {
    cwd,
    branch,
  });
  notifyGitChanged();
  return tree;
}

export async function renameWorktreeBranch(
  cwd: string,
  path: string,
  branch: string,
) {
  const tree = await invoke<Worktree>("git_worktree_rename_branch", {
    cwd,
    path,
    branch,
  });
  notifyGitChanged();
  return tree;
}

export function temporaryWorktreeBranchName(
  id: string = crypto.randomUUID(),
): string {
  const token = id
    .replace(/[^a-zA-Z0-9]/g, "")
    .slice(0, 8)
    .toLowerCase();
  return `mc/${token || Date.now().toString(36)}`;
}

export function orchestrationWorktreeBranchName(id: string): string {
  const token = id.replace(/[^a-zA-Z0-9]/g, "").slice(0, 12).toLowerCase();
  return `mc/orch-${token || Date.now().toString(36)}`;
}

export function namedWorktreeBranch(fragment: string): string | null {
  const clean = fragment
    .trim()
    .replace(/^(?:mc|monocode)\/+/, "")
    .replace(/^\/+|\/+$/g, "");
  return clean ? `mc/${clean}` : null;
}

export async function removeWorktree(
  cwd: string,
  path: string,
  force = false,
  keepSessions = false,
) {
  const result = await invoke<{ sessionIds: string[]; projectCwd: string }>(
    "git_worktree_remove",
    { cwd, path, force, keepSessions },
  );
  notifyGitChanged();
  return result;
}

export async function removeOrchestrationWorktree(
  cwd: string,
  path: string,
) {
  const result = await invoke<{ sessionIds: string[]; projectCwd: string }>(
    "git_orchestration_worktree_remove",
    { cwd, path },
  );
  notifyGitChanged();
  return result;
}

export async function removeOrchestrationBranch(
  cwd: string,
  branch: string,
) {
  await invoke<void>("git_orchestration_branch_remove", { cwd, branch });
  notifyGitChanged();
}

/** Read-only preflight; final removal must still recheck for new blockers. */
export const checkWorktreeRemoval: RemoveWorktree = (cwd, path, force) =>
  invoke("git_worktree_check_remove", { cwd, path, force });

/** Removed outside the app. A missing parent is more likely an unmounted volume. */
export async function worktreeDeletedExternally(path: string) {
  const [tree, parent] = await Promise.all([
    resolveProjectLocation(path),
    resolveProjectLocation(parentPath(path)),
  ]);
  return !tree && !!parent;
}

export function assertWorktreeFilesClosed(
  path: string,
  files: readonly FilePaneTab[],
) {
  if (
    files.some(
      (file) =>
        isEqualOrInside(file.cwd, path) ||
        (isFilesystemTab(file) && isEqualOrInside(file.path, path)),
    )
  ) {
    throw new Error(
      "Close the files and terminals open in this worktree first.",
    );
  }
}

export function worktreeSessionIds(
  tree: Worktree,
  sessions: readonly Pick<
    Session,
    "id" | "cwd" | "worktreeCwd" | "worktreeRemoved"
  >[],
) {
  const ids = new Set(tree.sessionIds);
  const others = new Set(tree.otherSpaceSessions?.map((row) => row.sessionId));
  for (const session of sessions) {
    // Live sessions override their last saved context.
    ids.delete(session.id);
    if (
      !others.has(session.id) &&
      !session.worktreeRemoved &&
      isEqualOrInside(session.worktreeCwd || session.cwd, tree.path)
    )
      ids.add(session.id);
  }
  return [...ids];
}

/** Why this worktree may not be deleted from this Space (T18), or undefined. */
export function worktreeRefusal(
  tree: Worktree,
  spaces: readonly Pick<Space, "id" | "name">[],
): string | undefined {
  const other = tree.otherSpaceSessions?.[0];
  if (!other) return undefined;
  const name =
    spaces.find((space) => space.id === other.spaceId)?.name ?? other.spaceId;
  return `A session in ${name} uses this worktree.`;
}

export const NO_BRANCH_LABEL = "No branch selected";

/** Keep the transcript and project identity while requiring a new working copy. */
export function detachSessionWorktree<T extends { cwd: string; worktreeCwd?: string }>(
  session: T,
  projectCwd: string,
  path: string,
) {
  return {
    ...session,
    cwd: isEqualOrInside(session.cwd, path) ? projectCwd : session.cwd,
    worktreeCwd: session.worktreeCwd || session.cwd,
    worktreeRemoved: true,
    branch: undefined,
    providerSessionId: undefined,
    context: undefined,
    pendingSwitch: undefined,
    pendingQuestion: undefined,
    busy: false,
    queueStatus: "paused" as const,
    queuePause: undefined,
  };
}

/** Existing working copies stay bound; removed ones can be replaced in place. */
export function sessionInWorktree(session: Session, tree: Worktree): Session {
  if (
    !session.worktreeRemoved &&
    pathKey(sessionWorkCwd(session)) === pathKey(tree.path)
  )
    return session;
  const target =
    session.worktreeRemoved && !isBlankSession(session)
      ? appendReadyHandoff(
          session,
          session.harness,
          session.harness,
          `The previous working copy was deleted. Continue this conversation in ${tree.path}. Recheck the files before making changes.\n\n${buildDeterministicHandoff(session)}`,
        )
      : isBlankSession(session)
        ? session
        : {
            ...newSession(
              session.harness,
              session.cwd,
              session.model,
              session.runtimeMode,
              session.modelSettings,
            ),
            providerAccountId: session.providerAccountId,
          };
  return {
    ...target,
    worktreeRemoved: undefined,
    worktreeCwd:
      pathKey(tree.path) === pathKey(session.cwd) ? undefined : tree.path,
    branch: tree.branch ?? undefined,
    providerSessionId: undefined,
    context: undefined,
    pendingSwitch: undefined,
  };
}

export type RemoveWorktree = (
  cwd: string,
  path: string,
  force: boolean,
  keepSessions?: boolean,
) => Promise<void | { sessionIds: string[]; projectCwd: string }>;
