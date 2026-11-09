import { useEffect, useRef, useState } from "react";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import { RemoveProjectDialog } from "../../features/projects/ui/RemoveProjectDialog";

type Point = { x: number; y: number };
type RemovalTarget = Point & { path: string; projectKey: string; name: string };

/**
 * A project menu's Archive and Delete. Each asks first, so a refusal shows in
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
}: {
  menuPath: string | undefined;
  closeMenu: () => void;
  reopenMenu: (menu: Point & { path: string; projectKey: string }, refusal: string) => void;
  showRefusal: (refusal: string) => void;
  restoreFocus: () => void;
  onRemoveProject: (
    path: string,
    options: { purgeData: boolean },
  ) => void | Promise<string | null>;
}) {
  const [removing, setRemoving] = useState<RemovalTarget | null>(null);
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

  const confirmDelete = ({ x, y, path, projectKey }: RemovalTarget) => {
    const purge = onRemoveProject(path, { purgeData: true });
    setRemoving(null);
    restoreFocus();
    void Promise.resolve(purge).then((refusal) => {
      if (refusal) reopenMenu({ x, y, path, projectKey }, refusal);
    });
  };

  return {
    archive: (path: string) =>
      whenAllowed(checkProjectArchive, path, () => {
        closeMenu();
        restoreFocus();
        onRemoveProject(path, { purgeData: false });
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
