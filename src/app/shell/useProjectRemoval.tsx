import { useEffect, useRef, useState } from "react";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import {
  activeSpaceName,
  readProjectRowFacts,
  type ProjectRowFacts,
} from "../../features/projects/model/projectRowActions";
import { isRemoteProjectPath } from "../../features/projects/model/recents";
import { projectRemovalItems } from "../../features/projects/ui/projectRemovalItems";
import { RemoveProjectDialog } from "../../features/projects/ui/RemoveProjectDialog";
import { refusalMessage } from "../../features/spaces/model/spaces";

type Point = { x: number; y: number };
type RemovalTarget = Point & { path: string; projectKey: string; name: string };

/**
 * Which rows the menu for `menuPath` offers, read each time it opens. Only a
 * hint: the backend decides each event.
 */
function useProjectRowFacts(menuPath: string | undefined) {
  const [facts, setFacts] = useState<
    (ProjectRowFacts & { path: string }) | null
  >(null);
  useEffect(() => {
    if (!menuPath) return;
    let active = true;
    void readProjectRowFacts(menuPath).then((read) => {
      if (active) setFacts({ ...read, path: menuPath });
    });
    return () => {
      active = false;
    };
  }, [menuPath]);
  return facts;
}

type RemoveProject = (
  path: string,
  options: { purgeData: boolean; leaveSpace?: boolean },
) => void | Promise<string | null>;

type RemovalOptions = {
  /** The project whose menu is open. */
  menuPath?: string;
  /** Closes this project's menu, if it is still the one open. */
  closeMenu: (path: string) => void;
  reopenMenu: (menu: Point & { path: string; projectKey: string }, refusal: string) => void;
  /** Shows the refusal in this project's menu, if it is still the one open. */
  showRefusal: (path: string, refusal: string) => void;
  restoreFocus: () => void;
  /** A purge or a `leaveSpace` answers with why it was refused, or null. */
  onRemoveProject: RemoveProject;
};

/** T5 or T6: the removal is its own check, so a refusal stays in the menu. */
function leaveSpaceWith(onRemoveProject: RemoveProject) {
  return (path: string): Promise<string | null> =>
    Promise.resolve(
      onRemoveProject(path, { purgeData: false, leaveSpace: true }),
    ).then((refusal) => refusal ?? null, refusalMessage);
}

/** Frame 20's rows for `path`'s menu; facts read for another row do not count. */
function removalMenuItems(
  facts: (ProjectRowFacts & { path: string }) | null,
  path: string,
) {
  return projectRemovalItems(
    activeSpaceName(),
    facts?.path === path ? facts : null,
    isRemoteProjectPath(path),
  );
}

/**
 * A project menu's Remove, Archive and Delete. Each asks first, so a refusal shows in
 * the menu's footer and no dialog opens. A refusal after Delete is confirmed
 * reopens the menu with it, since the menu has closed by then.
 */
export function useProjectRemoval({
  menuPath,
  closeMenu,
  reopenMenu,
  showRefusal,
  restoreFocus,
  onRemoveProject,
}: RemovalOptions) {
  const [removing, setRemoving] = useState<RemovalTarget | null>(null);
  const rowFacts = useProjectRowFacts(menuPath);
  // The check still running. It is held in a ref, not state, so a close
  // handler from an older render still cancels the latest check.
  const running = useRef<AbortController | null>(null);
  const cancelCheck = () => running.current?.abort();

  const whenAllowed = (
    ask: (path: string) => Promise<string | null>,
    path: string,
    proceed: () => void,
  ) => {
    cancelCheck();
    const started = new AbortController();
    running.current = started;
    void ask(path).then((refusal) => {
      // The user closed or moved the menu while the check ran.
      if (started.signal.aborted) return;
      if (refusal !== null) {
        showRefusal(path, refusal);
        return;
      }
      closeMenu(path);
      proceed();
    });
  };

  const confirmDelete = ({ x, y, path, projectKey }: RemovalTarget) => {
    // Reopening moves the menu, so a check started meanwhile is dropped.
    const reopen = (refusal: string) => {
      cancelCheck();
      reopenMenu({ x, y, path, projectKey }, refusal);
    };
    const purge = onRemoveProject(path, { purgeData: true });
    setRemoving(null);
    restoreFocus();
    void Promise.resolve(purge).then(
      (refusal) => {
        if (typeof refusal === "string") reopen(refusal);
      },
      (error: unknown) => reopen(refusalMessage(error)),
    );
  };

  return {
    /** The menu closed or moved: an answer still on its way does nothing. */
    cancelCheck,
    menuItems: (path: string) => removalMenuItems(rowFacts, path),
    leaveSpace: (path: string) =>
      whenAllowed(leaveSpaceWith(onRemoveProject), path, restoreFocus),
    archive: (path: string) =>
      whenAllowed(checkProjectArchive, path, () => {
        restoreFocus();
        onRemoveProject(path, { purgeData: false });
      }),
    askToDelete: (target: RemovalTarget) =>
      whenAllowed(checkProjectDelete, target.path, () => setRemoving(target)),
    dismiss: () => setRemoving(null),
    isRemoving: removing != null,
    dialog: removing ? (
      <RemoveProjectDialog
        name={removing.name}
        path={removing.path}
        onConfirm={() => confirmDelete(removing)}
        onCancel={() => {
          setRemoving(null);
          restoreFocus();
        }}
      />
    ) : null,
  };
}
