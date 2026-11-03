import { useMemo, useState, useSyncExternalStore, type FormEvent } from "react";
import { Layers, Loader, Pencil, Plus } from "../../../shared/ui/icons";
import { Modal } from "../../../shared/ui/Modal";
import { countBadge } from "../../../shared/lib/numbers";
import {
  listMonos,
  MONO_COLORS,
  monosSnapshot,
  subscribeMonos,
  type MonoState,
} from "../../monos/model/mono";
import { ColorPicker } from "../../monos/ui/monoPanelParts";
import {
  loadSpaces,
  spaceActivity,
  spacesView,
  subscribeSpaces,
} from "../model/activeSpace";
import { createSpace, type Space, type SpaceLogin } from "../model/spaces";
import { SpaceLogins } from "./SpaceLogins";

/** A Space's colour; Space 1 predates colours and reads as the first one. */
export function spaceColor(space: Pick<Space, "color">): string {
  return space.color ?? MONO_COLORS[0];
}

/** The "New Space" button beside the Spaces group title. */
export function NewSpaceButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex shrink-0 items-center gap-1.5 rounded-md border border-content/10 px-2.5 py-1 text-[12px] text-content/70 transition-transform duration-150 hover:bg-content/10 hover:text-content active:scale-[0.97]"
    >
      <Plus className="size-3.5" strokeWidth={1.75} aria-hidden />
      New Space
    </button>
  );
}

/** One row per Space with its Claude and Codex login. */
export function SpacesList({
  states,
  unseenCounts,
  onEdit,
}: {
  /** What each Mono is doing, by Mono id, in every Space. */
  states: ReadonlyMap<string, MonoState>;
  /** Unseen replies by Mono id, in every Space. */
  unseenCounts?: ReadonlyMap<string, number>;
  onEdit: (space: Space) => void;
}) {
  const view = useSyncExternalStore(subscribeSpaces, spacesView);
  const roster = useSyncExternalStore(subscribeMonos, monosSnapshot);
  const activity = useMemo(
    () => spaceActivity(listMonos(), states, unseenCounts),
    // The roster is read through its snapshot.
    [roster, states, unseenCounts],
  );
  return view.spaces.map((space) => {
    const current = space.id === view.activeSpaceId;
    return (
      <SpaceListRow
        key={space.id}
        space={space}
        current={current}
        logins={view.logins}
        // Like the switcher, only another Space's replies are news.
        unread={current ? 0 : (activity.get(space.id)?.unread ?? 0)}
        onEdit={() => onEdit(space)}
      />
    );
  });
}

function SpaceListRow({
  space,
  current,
  logins,
  unread,
  onEdit,
}: {
  space: Space;
  current: boolean;
  logins: readonly SpaceLogin[];
  unread: number;
  onEdit: () => void;
}) {
  const color = spaceColor(space);
  return (
    <div
      data-space-id={space.id}
      className="flex items-center gap-4 border-b border-content/5 px-4 py-3.5 last:border-b-0"
    >
      {/* Frame 13: the colour at 16% fill, a 32% ring, the icon in full. */}
      <span
        data-space-color={color}
        className="grid size-7 shrink-0 place-items-center rounded-lg"
        style={{
          color,
          background: `color-mix(in srgb, ${color} 16%, transparent)`,
          boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${color} 32%, transparent)`,
        }}
      >
        <Layers className="size-4" strokeWidth={1.75} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-1.5 text-[13px] font-medium text-content">
          <span className="truncate">{space.name}</span>
          {current ? (
            <span className="shrink-0 rounded bg-accent/15 px-1.5 text-[10px] font-medium leading-4 text-accent">
              This window
            </span>
          ) : null}
          {unread ? (
            <span
              data-space-unread={unread}
              className="grid h-4 min-w-4 shrink-0 place-items-center rounded-full bg-accent px-1 text-[10px] font-medium tabular-nums leading-none text-white"
            >
              {countBadge(unread)}
            </span>
          ) : null}
        </div>
        <div className="mt-[3px] flex min-w-0 gap-4">
          <SpaceLogins spaceId={space.id} logins={logins} withPlan />
        </div>
      </div>
      <button
        type="button"
        aria-label={`Edit ${space.name}`}
        title="Edit Space"
        onClick={onEdit}
        className="grid size-7 shrink-0 place-items-center rounded-md text-content/40 transition-transform duration-150 hover:bg-content/10 hover:text-content active:scale-[0.96]"
      >
        <Pencil className="size-3.5" strokeWidth={1.75} />
      </button>
    </div>
  );
}

/** Frame 11: name and colour; Create hands the new Space to Edit Space. */
export function NewSpaceDialog({
  onCreated,
  onCancel,
}: {
  onCreated: (space: Space) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  // The next palette colour, so a new Space looks unlike the last one.
  const [color, setColor] = useState<string>(
    () => MONO_COLORS[spacesView().spaces.length % MONO_COLORS.length],
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || !name.trim()) return;
    setBusy(true);
    setError(undefined);
    try {
      const space = await createSpace(name.trim(), color);
      await loadSpaces();
      onCreated(space);
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  };
  return (
    <Modal
      title="New Space"
      description="A Space has its own logins, Gigas, Monos, projects and sessions."
      size="sm"
      onClose={() => {
        if (!busy) onCancel();
      }}
    >
      <form className="flex flex-col gap-4 p-4" onSubmit={submit}>
        <SpaceNameField value={name} disabled={busy} onChange={setName} />
        <SpaceColorField value={color} onChange={setColor} />
        <p className="text-[11px] leading-relaxed text-content/45">
          Next you pick its Claude and Codex logins: Create opens Edit Space for
          the new Space.
        </p>
        {error ? (
          <p role="alert" className="text-[12px] text-red-400">
            {error}
          </p>
        ) : null}
        <DialogButtons
          busy={busy}
          submitLabel="Create Space"
          disabled={!name.trim()}
          onCancel={onCancel}
        />
      </form>
    </Modal>
  );
}

/** The Name field of New Space and Edit Space, as CreateWorktreeDialog's. */
export function SpaceNameField({
  value,
  disabled,
  onChange,
}: {
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1.5 text-[12px] text-content/70">
      <span>Name</span>
      <input
        autoFocus
        aria-label="Space name"
        className="h-9 rounded-md border border-content/10 bg-background-base px-2.5 text-[13px] text-content outline-none focus:border-content/25 disabled:opacity-50"
        value={value}
        disabled={disabled}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
    </label>
  );
}

/** The Colour field: monos' nine presets and the custom picker. */
export function SpaceColorField({
  value,
  onChange,
}: {
  value: string;
  onChange: (color: string) => void;
}) {
  return (
    <div className="flex flex-col gap-1.5 text-[12px] text-content/70">
      <span>Colour</span>
      <ColorPicker current={value} onPick={onChange} />
    </div>
  );
}

/**
 * Cancel and the submit button, as CreateWorktreeDialog draws them. With
 * `onSubmit` the button acts on click, for a dialog that holds a form of its
 * own (Edit Space's Add a login).
 */
export function DialogButtons({
  busy,
  submitLabel,
  disabled,
  onCancel,
  onSubmit,
}: {
  busy: boolean;
  submitLabel: string;
  disabled: boolean;
  onCancel: () => void;
  onSubmit?: () => void;
}) {
  return (
    <div className="flex justify-end gap-2">
      <button
        type="button"
        disabled={busy}
        onClick={onCancel}
        className="rounded-md px-3 py-1.5 text-[12px] hover:bg-content/8 active:scale-[0.97]"
      >
        Cancel
      </button>
      <button
        type={onSubmit ? "button" : "submit"}
        onClick={onSubmit}
        disabled={busy || disabled}
        className="inline-flex items-center gap-1.5 rounded-md bg-content px-3 py-1.5 text-[12px] font-medium text-background-base disabled:opacity-40 active:scale-[0.97]"
      >
        {busy ? <Loader className="size-3.5 animate-spin" /> : null}
        {submitLabel}
      </button>
    </div>
  );
}
