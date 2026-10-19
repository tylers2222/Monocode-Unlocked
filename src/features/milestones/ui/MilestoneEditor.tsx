import type { KeyboardEvent, ReactNode } from "react";
import { Calendar, Trash2 } from "../../../shared/ui/icons";
import { checkDraft, draftOf, patchOf, type Draft } from "../model/draft";
import {
  MILESTONE_STATUSES,
  type Milestone,
  type MilestonePatch,
  type NewMilestone,
} from "../model/milestones";
import { ROW_DOT } from "./MilestoneTimeline";
import { STATUS_LABELS } from "./timeline";

/**
 * The open editor. The page holds it, not the card: the card remounts when
 * its row moves to another group or Done collapses, and the text must stay.
 */
export type EditorState = {
  /**
   * The row as it opened, so a save sends only what the user changed; null
   * for a new milestone.
   */
  opened: Milestone | null;
  /** This opening: a save that settles late acts only on the editor it came from. */
  session: symbol;
  draft: Draft;
  /** Errors show once a save was tried. */
  tried: boolean;
  busy: boolean;
  failure: string | null;
  /** Frame M·7: Delete asks once, inline, before it removes the row. */
  confirmingDelete: boolean;
};

export function openEditor(milestone: Milestone | null): EditorState {
  return {
    opened: milestone,
    session: Symbol("milestone editor"),
    draft: draftOf(milestone),
    tried: false,
    busy: false,
    failure: null,
    confirmingDelete: false,
  };
}

type Props = {
  state: EditorState;
  setState: (update: (current: EditorState) => EditorState) => void;
  onAdd: (milestone: NewMilestone) => Promise<void>;
  onUpdate: (id: string, patch: MilestonePatch) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onClose: () => void;
};

const ACTION =
  "inline-flex h-6.5 items-center gap-1.5 rounded-md px-3 text-[12px] disabled:opacity-40";
const SEGMENTS = "inline-flex rounded-md bg-content/5 p-0.5";
const SEGMENT =
  "inline-flex h-6 items-center gap-1.5 rounded px-2 text-[12px] text-content/60 hover:text-content aria-checked:bg-content/12 aria-checked:text-content";

/** The add card of frame M·5, the edit-in-place card of M·6 and its M·7 delete. */
export function MilestoneEditor({
  state,
  setState,
  onAdd,
  onUpdate,
  onRemove,
  onClose,
}: Props) {
  const {
    opened: milestone,
    draft,
    tried,
    busy,
    failure,
    confirmingDelete,
  } = state;
  const checked = checkDraft(draft);
  const errors = tried && !checked.ok ? checked.errors : {};
  const edit = (change: Partial<Draft>) =>
    setState((current) => ({
      ...current,
      draft: { ...current.draft, ...change },
    }));

  /** One write; the card closes only when it succeeds, else it shows why. */
  const run = async (write: () => Promise<void>) => {
    if (busy) return;
    setState((current) => ({ ...current, busy: true, failure: null }));
    try {
      await write();
      onClose();
    } catch (reason) {
      setState((current) => ({
        ...current,
        busy: false,
        failure: reason instanceof Error ? reason.message : String(reason),
      }));
    }
  };

  const save = () => {
    if (!checked.ok) {
      setState((current) => ({ ...current, tried: true }));
      return;
    }
    const { value } = checked;
    void run(() =>
      milestone
        ? onUpdate(milestone.id, patchOf(milestone, value))
        : onAdd(value),
    );
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      save();
    }
  };

  const failureLine = failure ? (
    <p role="alert" className="px-3.5 pb-2 text-[12px] text-red-400">
      {failure}
    </p>
  ) : null;
  const setConfirming = (confirmingDelete: boolean) =>
    setState((current) => ({ ...current, confirmingDelete, failure: null }));

  if (milestone && confirmingDelete)
    return (
      <DeleteConfirm
        milestone={milestone}
        busy={busy}
        failureLine={failureLine}
        onKeep={() => setConfirming(false)}
        onDelete={() => void run(() => onRemove(milestone.id))}
        onClose={onClose}
      />
    );

  return (
    <div
      role="group"
      aria-label={milestone ? `Edit ${milestone.title}` : "New milestone"}
      data-milestone-editor
      onKeyDown={onKeyDown}
      className="overflow-hidden rounded-[10px] border border-content/14 bg-content/3 shadow-sm"
    >
      <div className="px-3.5 pt-3 pb-2">
        <input
          autoFocus
          value={draft.title}
          maxLength={200}
          aria-label="Title"
          aria-invalid={errors.title ? true : undefined}
          placeholder="Milestone title"
          onChange={(event) => edit({ title: event.target.value })}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.metaKey || event.ctrlKey) return;
            event.preventDefault();
            save();
          }}
          className="w-full bg-transparent text-sm font-medium outline-none placeholder:text-content/35"
        />
        {errors.title ? <FieldError>{errors.title}</FieldError> : null}
        <textarea
          value={draft.note}
          maxLength={4000}
          rows={1}
          aria-label="Note"
          placeholder="Note (optional)"
          onChange={(event) => edit({ note: event.target.value })}
          className="mt-1.5 block w-full resize-none bg-transparent text-[13px] leading-5 text-content/70 outline-none field-sizing-content placeholder:text-content/35"
        />
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-content/8 px-3 py-2.5">
        <div role="radiogroup" aria-label="Status" className={SEGMENTS}>
          {MILESTONE_STATUSES.map((status) => (
            <button
              key={status}
              type="button"
              role="radio"
              aria-checked={draft.status === status}
              onClick={() => edit({ status })}
              className={SEGMENT}
            >
              <i
                className={`block size-[7px] rounded-full ${ROW_DOT[status]}`}
              />
              {STATUS_LABELS[status]}
            </button>
          ))}
        </div>
        <div role="radiogroup" aria-label="Target" className={SEGMENTS}>
          <button
            type="button"
            role="radio"
            aria-checked={draft.target.kind === "date"}
            onClick={() => {
              if (draft.target.kind === "later")
                edit({ target: { kind: "date", text: "" } });
            }}
            className={SEGMENT}
          >
            <Calendar className="size-3" />
            Date
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={draft.target.kind === "later"}
            onClick={() => edit({ target: { kind: "later" } })}
            className={SEGMENT}
          >
            Later
          </button>
        </div>
        {draft.target.kind === "date" ? (
          <input
            autoFocus={draft.target.text === ""}
            value={draft.target.text}
            aria-label="Target date"
            aria-invalid={errors.date ? true : undefined}
            aria-describedby={errors.date ? "milestone-date-error" : undefined}
            placeholder="Dec 12, 2026"
            spellCheck={false}
            onChange={(event) =>
              edit({ target: { kind: "date", text: event.target.value } })
            }
            className={`h-7 w-32 rounded-md border bg-transparent px-2 text-[12px] tabular-nums outline-none placeholder:text-content/35 ${
              errors.date
                ? "border-red-400/60"
                : "border-content/12 focus:border-content/30"
            }`}
          />
        ) : null}
        {errors.date ? (
          <FieldError id="milestone-date-error">{errors.date}</FieldError>
        ) : null}
      </div>
      {failureLine}
      <div className="flex items-center justify-end gap-1.5 px-3 pb-2.5">
        {milestone ? (
          <button
            type="button"
            onClick={() => setConfirming(true)}
            className={`${ACTION} text-red-400 hover:bg-red-400/10`}
          >
            <Trash2 className="size-3.5" />
            Delete
          </button>
        ) : (
          <kbd className="mr-1 font-sans text-[11px] text-content/35">⌘↵</kbd>
        )}
        <button
          type="button"
          onClick={onClose}
          className={`${ACTION} text-content/70 hover:bg-content/10 hover:text-content`}
        >
          Cancel
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={save}
          className={`${ACTION} bg-content font-medium text-background-base hover:bg-content/80`}
        >
          {milestone ? "Save" : "Add"}
        </button>
      </div>
    </div>
  );
}

/** Frame M·7: the row as stored, and one inline question. Escape keeps the row. */
function DeleteConfirm({
  milestone,
  busy,
  failureLine,
  onKeep,
  onDelete,
  onClose,
}: {
  milestone: Milestone;
  busy: boolean;
  failureLine: ReactNode;
  onKeep: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  return (
    <div
      role="group"
      aria-label={`Delete ${milestone.title}`}
      data-milestone-editor
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        onClose();
      }}
      className="overflow-hidden rounded-[10px] border border-red-400/30 bg-content/3 shadow-sm"
    >
      <div className="px-3.5 pt-3 pb-3">
        <div className="text-sm font-medium">{milestone.title}</div>
        {milestone.note ? (
          <p className="mt-1.5 text-xs leading-[18px] text-content/60">
            {milestone.note}
          </p>
        ) : null}
      </div>
      {failureLine}
      <div className="flex items-center gap-2 border-t border-red-400/20 bg-red-400/8 px-3 py-2 text-[12px] text-red-400">
        <Trash2 className="size-3.5 shrink-0" />
        <span className="min-w-0 flex-1">
          Delete “{milestone.title}”? This can’t be undone.
        </span>
        <button
          type="button"
          autoFocus
          onClick={onKeep}
          className={`${ACTION} text-content/70 hover:bg-content/10 hover:text-content`}
        >
          Keep
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={onDelete}
          className={`${ACTION} bg-red-500 font-medium text-white hover:bg-red-500/85`}
        >
          Delete
        </button>
      </div>
    </div>
  );
}

function FieldError({ id, children }: { id?: string; children: ReactNode }) {
  return (
    <p id={id} role="alert" className="mt-1 w-full text-[12px] text-red-400">
      {children}
    </p>
  );
}
