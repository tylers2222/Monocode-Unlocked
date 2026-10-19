import { useState, type KeyboardEvent, type ReactNode } from "react";
import { Calendar } from "../../../shared/ui/icons";
import { checkDraft, draftOf, patchOf, type Draft } from "../model/draft";
import {
  MILESTONE_STATUSES,
  type Milestone,
  type MilestonePatch,
} from "../model/milestones";
import { ROW_DOT } from "./MilestoneTimeline";
import { STATUS_LABELS } from "./timeline";

type Props = {
  milestone: Milestone;
  onUpdate: (id: string, patch: MilestonePatch) => Promise<void>;
  onClose: () => void;
};

type EditorState = {
  /** The row as it opened, so a save sends only what the user changed. */
  opened: Milestone;
  draft: Draft;
  /** Errors show once a save was tried. */
  tried: boolean;
  busy: boolean;
  failure: string | null;
};

const ACTION =
  "inline-flex h-6.5 items-center gap-1.5 rounded-md px-3 text-[12px] disabled:opacity-40";
const SEGMENTS = "inline-flex rounded-md bg-content/5 p-0.5";
const SEGMENT =
  "inline-flex h-6 items-center gap-1.5 rounded px-2 text-[12px] text-content/60 hover:text-content aria-checked:bg-content/12 aria-checked:text-content";

/** The edit-in-place card of approved frame M·6. */
export function MilestoneEditor({ milestone, onUpdate, onClose }: Props) {
  const [state, setState] = useState<EditorState>(() => ({
    opened: milestone,
    draft: draftOf(milestone),
    tried: false,
    busy: false,
    failure: null,
  }));
  const { draft, tried, busy, failure } = state;
  const checked = checkDraft(draft);
  const errors = tried && !checked.ok ? checked.errors : {};
  const edit = (change: Partial<Draft>) =>
    setState((current) => ({
      ...current,
      draft: { ...current.draft, ...change },
    }));

  const save = async () => {
    if (busy) return;
    if (!checked.ok) {
      setState((current) => ({ ...current, tried: true }));
      return;
    }
    setState((current) => ({ ...current, busy: true, failure: null }));
    try {
      await onUpdate(milestone.id, patchOf(state.opened, checked.value));
      onClose();
    } catch (reason) {
      setState((current) => ({
        ...current,
        busy: false,
        failure: reason instanceof Error ? reason.message : String(reason),
      }));
    }
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void save();
    }
  };

  return (
    <div
      role="group"
      aria-label={`Edit ${milestone.title}`}
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
          onChange={(event) => edit({ title: event.target.value })}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.metaKey || event.ctrlKey) return;
            event.preventDefault();
            void save();
          }}
          className="w-full bg-transparent text-sm font-medium outline-none"
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
      {failure ? (
        <p role="alert" className="px-3.5 pb-2 text-[12px] text-red-400">
          {failure}
        </p>
      ) : null}
      <div className="flex items-center justify-end gap-1.5 px-3 pb-2.5">
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
          onClick={() => void save()}
          className={`${ACTION} bg-content font-medium text-background-base hover:bg-content/80`}
        >
          Save
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
