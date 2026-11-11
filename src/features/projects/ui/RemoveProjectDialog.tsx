import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { LAYER } from "../../../shared/lib/layers";
import {
  readProjectDeleteText,
  type ProjectDeleteText,
} from "../model/projectDeleteDialog";

type Props = {
  name: string;
  path: string;
  onCancel: () => void;
  onConfirm: () => void;
};

function DeleteDialogText({ text }: { text: ProjectDeleteText }) {
  return (
    <div className="flex flex-col gap-1">
      <h2 className="text-[13px] font-medium leading-tight text-content">
        {text.title}
      </h2>
      <p className="text-[12px] leading-snug text-content/55">{text.summary}</p>
      {text.keeps.map((line) => (
        <p key={line} className="text-[12px] leading-snug text-content/80">
          {line}
        </p>
      ))}
      {text.count ? (
        <p className="text-[12px] leading-snug text-content/45">{text.count}</p>
      ) : null}
      <p className="truncate text-[11px] leading-tight text-content/40">
        {text.location}
      </p>
    </div>
  );
}

/**
 * Delete drops the project from this Space and this Space's saved chats. It
 * opens once the backend answered what the delete would do, so it never shows
 * a sentence that turns out wrong. The folder on disk is left alone.
 */
export function RemoveProjectDialog({ name, path, onCancel, onConfirm }: Props) {
  const [text, setText] = useState<ProjectDeleteText | null>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
  }, [text]);

  useEffect(() => {
    let cancelled = false;
    void readProjectDeleteText(name, path).then((next) => {
      if (!cancelled) setText(next);
    });
    return () => {
      cancelled = true;
    };
  }, [name, path]);

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

  if (!text) return null;
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
        <DeleteDialogText text={text} />

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
            {text.confirm}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
