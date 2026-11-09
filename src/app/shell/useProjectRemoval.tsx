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

type RemovalOptions = {
  menuPath: string | undefined;
  closeMenu: () => void;
  reopenMenu: (menu: Point & { path: string; projectKey: string }, refusal: string) => void;
  showRefusal: (refusal: string) => void;
  restoreFocus: () => void;
  onRemoveProject?: (
    path: string,
    options: { purgeData: boolean; leaveSpace?: boolean },
  ) => void | Promise<string | null>;
};

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
  const openMenuPath = useRef(menuPath);
  useEffect(() => {
    openMenuPath.current = menuPath;
  }, [menuPath]);

  const whenAllowed = (
    check: (path: string) => Promise<string | null>,
    path: string,
    proceed: () => void,
  ) => {
    void check(path).then((refusal) => {
      // The user closed or moved the menu while the check ran.
      if (openMenuPath.current !== path) return;
      if (refusal) showRefusal(refusal);
      else proceed();
    });
  };

  const leaveSpace = async (path: string) =>
    (await onRemoveProject?.(path, { purgeData: false, leaveSpace: true })) ??
    null;

  const confirmDelete = ({ x, y, path, projectKey }: RemovalTarget) => {
    const purge = onRemoveProject?.(path, { purgeData: true });
    setRemoving(null);
    restoreFocus();
    void Promise.resolve(purge).then((refusal) => {
      if (refusal) reopenMenu({ x, y, path, projectKey }, refusal);
    });
  };

  return {
    menuItems: (path: string) =>
      projectRemovalItems(
        activeSpaceName(),
        rowFacts?.path === path ? rowFacts : null,
        isRemoteProjectPath(path),
      ),
    leaveSpace: (path: string) =>
      whenAllowed(leaveSpace, path, () => {
        closeMenu();
        restoreFocus();
      }),
    archive: (path: string) =>
      whenAllowed(checkProjectArchive, path, () => {
        closeMenu();
        restoreFocus();
        onRemoveProject?.(path, { purgeData: false });
      }),
    askToDelete: (target: RemovalTarget) =>
      whenAllowed(checkProjectDelete, target.path, () => {
        closeMenu();
        setRemoving(target);
      }),
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
