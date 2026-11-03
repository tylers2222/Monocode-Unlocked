import { useMemo, useSyncExternalStore } from "react";
import { Layers, Pencil, Plus, Trash2 } from "../../../shared/ui/icons";
import { countBadge } from "../../../shared/lib/numbers";
import {
  listMonos,
  monosSnapshot,
  subscribeMonos,
  type MonoState,
} from "../../monos/model/mono";
import {
  spaceActivity,
  spacesView,
  subscribeSpaces,
} from "../model/activeSpace";
import type { Space, SpaceLogin } from "../model/spaces";
import { SpaceLogins } from "./SpaceLogins";

// New Space, rename and remove are drawn on frame 3 but what they open is not
// (frames 11-14 pending), so the buttons do nothing yet.

/** The "New Space" button beside the Spaces group title. */
export function NewSpaceButton() {
  return (
    <button
      type="button"
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
}: {
  /** What each Mono is doing, by Mono id, in every Space. */
  states: ReadonlyMap<string, MonoState>;
  /** Unseen replies by Mono id, in every Space. */
  unseenCounts?: ReadonlyMap<string, number>;
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
      />
    );
  });
}

function SpaceListRow({
  space,
  current,
  logins,
  unread,
}: {
  space: Space;
  current: boolean;
  logins: readonly SpaceLogin[];
  unread: number;
}) {
  return (
    <div
      data-space-id={space.id}
      className="flex items-center gap-4 border-b border-content/5 px-4 py-3.5 last:border-b-0"
    >
      <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-content/[0.05] ring-1 ring-inset ring-content/[0.06]">
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
      <div className="flex shrink-0 items-center gap-1">
        <button
          type="button"
          aria-label={`Rename ${space.name}`}
          title="Rename Space"
          className="grid size-7 place-items-center rounded-md text-content/40 transition-transform duration-150 hover:bg-content/10 hover:text-content active:scale-[0.96]"
        >
          <Pencil className="size-3.5" strokeWidth={1.75} />
        </button>
        {current ? null : (
          <button
            type="button"
            aria-label={`Remove ${space.name}`}
            title="Remove Space"
            className="grid size-7 place-items-center rounded-md text-content/35 transition-transform duration-150 hover:bg-red-400/10 hover:text-red-400 active:scale-[0.96]"
          >
            <Trash2 className="size-3.5" strokeWidth={1.75} />
          </button>
        )}
      </div>
    </div>
  );
}
