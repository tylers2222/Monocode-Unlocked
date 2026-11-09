import { useState } from "react";
import {
  checkProjectArchive,
  checkProjectDelete,
} from "../../features/projects/model/projectDeleteCheck";
import { RemoveProjectDialog } from "../../features/projects/ui/RemoveProjectDialog";
import { refusalMessage } from "../../features/spaces/model/spaces";

type Point = { x: number; y: number };
type RemovalTarget = Point & { path: string; projectKey: string; name: string };

/**
 * A project menu's Archive and Delete. Each asks first, so a refusal shows in
 * the menu's footer and no dialog opens. A refusal after Delete is confirmed
 * reopens the menu with it, since the menu has closed by then.
 */
export function useProjectRemoval({
  closeMenu,
  reopenMenu,
  showRefusal,
  restoreFocus,
  onRemoveProject,
}: {
  /** Closes this project's menu, if it is still the one open. */
  closeMenu: (path: string) => void;
  reopenMenu: (menu: Point & { path: string; projectKey: string }, refusal: string) => void;
  /** Shows the refusal in this project's menu, if it is still the one open. */
  showRefusal: (path: string, refusal: string) => void;
  restoreFocus: () => void;
  onRemoveProject: (
    path: string,
    options: { purgeData: boolean },
  ) => void | Promise<string | null>;
}) {
  const [removing, setRemoving] = useState<RemovalTarget | null>(null);

  const whenAllowed = (
    check: (path: string) => Promise<string | null>,
    path: string,
    proceed: () => void,
  ) => {
    void check(path).then((refusal) => {
      if (refusal !== null) {
        showRefusal(path, refusal);
        return;
      }
      closeMenu(path);
      proceed();
    });
  };

  const confirmDelete = ({ x, y, path, projectKey }: RemovalTarget) => {
    const reopen = (refusal: string) =>
      reopenMenu({ x, y, path, projectKey }, refusal);
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
