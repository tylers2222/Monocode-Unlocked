import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { LAYER } from "../../../shared/lib/layers";
import { prettyCwd } from "../../../shared/lib/paths";
import {
  readProjectDeletePrompt,
  type ProjectDeletePrompt,
} from "../model/projectDeleteCheck";
import { isRemoteProjectPath } from "../model/recents";

type Props = {
  name: string;
  path: string;
  onCancel: () => void;
  onConfirm: () => void;
};

/** Scoped unless this Space is the only one with chats or membership. */
function DeletePromptText({
  path,
  prompt,
}: {
  path: string;
  prompt: ProjectDeletePrompt | null;
}) {
  const space = prompt?.spaceName ?? "this Space";
  if (isRemoteProjectPath(path)) {
    return (
      <p className="text-[12px] leading-snug text-content/55">
        It leaves {space}’s sidebar, and nothing on the machine changes.
      </p>
    );
  }
  const sessions = prompt?.sessions ?? 0;
  return (
    <>
      <p className="text-[12px] leading-snug text-content/55">
        {prompt && !prompt.scoped
          ? "All conversations for this project will be deleted."
          : `All conversations for this project in ${space} will be deleted.`}{" "}
        It also leaves the sidebar. The folder on disk stays put, and opening it
        again brings the project back empty.
      </p>
      {sessions > 0 ? (
        <p className="text-[12px] leading-snug text-content/45">
          {sessions === 1
            ? "1 saved conversation will be removed."
            : `${sessions} saved conversations will be removed.`}
        </p>
      ) : null}
    </>
  );
}

/**
 * Delete drops the project from this Space and this Space's saved chats. The
 * folder on disk is left alone; opening it again brings the project back empty.
 */
export function RemoveProjectDialog({ name, path, onCancel, onConfirm }: Props) {
  const [prompt, setPrompt] = useState<ProjectDeletePrompt | null>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  useEffect(() => {
    let cancelled = false;
    void readProjectDeletePrompt(path).then((next) => {
      if (!cancelled) setPrompt(next);
    });
    return () => {
      cancelled = true;
    };
  }, [path]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      onCancel();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);

  return createPortal(
    <div className="fixed inset-0" style={{ zIndex: LAYER.dialog }}>
      <div
        className="dialog-scrim absolute inset-0 z-0 bg-black/30"
        onMouseDown={onCancel}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label={`Delete ${name}`}
        onMouseDown={(event) => event.stopPropagation()}
        className="modal-surface absolute z-[1] left-1/2 top-[22%] flex w-[min(420px,calc(100vw-24px))] -translate-x-1/2 flex-col gap-3 rounded-lg border border-content/10 bg-background-base dark:bg-content/5 p-4 shadow-xl backdrop-blur-xl"
      >
        <div className="flex flex-col gap-1">
          <h2 className="text-[13px] font-medium leading-tight text-content">
            Delete “{name}”?
          </h2>
          <DeletePromptText path={path} prompt={prompt} />
          <p className="truncate text-[11px] leading-tight text-content/40">
            {prettyCwd(path)}
          </p>
        </div>

        <div className="flex justify-end gap-2">
          <button
            ref={cancelRef}
            type="button"
            onClick={onCancel}
            className="rounded-md px-3 py-1.5 text-[12px] text-content/70 hover:bg-content/8 hover:text-content"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={onConfirm}
            className="rounded-md bg-red-500/20 px-3 py-1.5 text-[12px] font-medium text-red-300 hover:bg-red-500/30"
          >
            Delete
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
