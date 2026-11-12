import { useRef, useState } from "react";
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
