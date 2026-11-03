import {
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type RefObject,
} from "react";
import { Check, ChevronsUpDown, Plus, Settings } from "../../../shared/ui/icons";
import { Popover } from "../../../shared/ui/Popover";
import { countBadge } from "../../../shared/lib/numbers";
import {
  listMonos,
  monosSnapshot,
  subscribeMonos,
  type MonoState,
} from "../../monos/model/mono";
import {
  otherSpaceHasNews,
  spaceActivity,
  spacesView,
  subscribeSpaces,
  switchSpace,
  type SpaceActivity,
  type SpacesView,
} from "../model/activeSpace";
import type { Space } from "../model/spaces";
import { SpaceLogins } from "./SpaceLogins";

/**
 * The Space the window shows, above the Giga Monos: its name and both
 * logins, and a dot when another Space has news. Opens a menu with one row
 * per Space; picking one only changes the view.
 */
export function SpaceSwitcher({
  states,
  unseenCounts,
  onOpenSettings,
}: {
  /** What each Mono is doing, by Mono id, in every Space. */
  states: ReadonlyMap<string, MonoState>;
  /** Unseen replies by Mono id, in every Space. */
  unseenCounts?: ReadonlyMap<string, number>;
  onOpenSettings?: () => void;
}) {
  const view = useSyncExternalStore(subscribeSpaces, spacesView);
  const roster = useSyncExternalStore(subscribeMonos, monosSnapshot);
  const activity = useMemo(
    () => spaceActivity(listMonos(), states, unseenCounts),
    // The roster is read through its snapshot.
    [roster, states, unseenCounts],
  );
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const active = view.spaces.find((space) => space.id === view.activeSpaceId);
  if (!active) return null;

  const news = otherSpaceHasNews(activity, active.id);
  return (
    <div className="shrink-0 px-2 pb-2" data-space-switcher>
      <button
        ref={trigger}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Space: ${active.name}`}
        onClick={() => setOpen((value) => !value)}
        className={`flex w-full items-center rounded-md border border-content/8 py-1.5 pl-2.5 pr-2 text-left shadow-sm ${
          open ? "bg-selection" : "hover:bg-content/10"
        }`}
      >
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex items-center gap-1.5">
            <span className="min-w-0 flex-1 truncate text-[13px] font-medium leading-tight text-content">
              {active.name}
            </span>
            {news ? (
              <span
                data-space-news
                title={newsTitle(view.spaces, activity, active.id)}
                className="size-1.5 shrink-0 rounded-full bg-accent"
              />
            ) : null}
            <ChevronsUpDown
              className="size-3.5 shrink-0 text-content/50"
              strokeWidth={1.75}
            />
          </span>
          <SpaceLogins spaceId={active.id} logins={view.logins} />
        </span>
      </button>
      {open ? (
        <SpacesMenu
          anchor={trigger}
          view={view}
          activity={activity}
          onClose={() => setOpen(false)}
          onOpenSettings={onOpenSettings}
        />
      ) : null}
    </div>
  );
}

/** The open switcher: one row per Space, then New Space and Manage Spaces. */
function SpacesMenu({
  anchor,
  view,
  activity,
  onClose,
  onOpenSettings,
}: {
  anchor: RefObject<HTMLButtonElement | null>;
  view: SpacesView;
  activity: ReadonlyMap<string, SpaceActivity>;
  onClose: () => void;
  onOpenSettings?: () => void;
}) {
  const settings = () => {
    onClose();
    onOpenSettings?.();
  };
  return (
    <Popover
      anchor={anchor}
      side="bottom"
      align="start"
      gap={4}
      width={296}
      onDismiss={onClose}
      role="menu"
      aria-label="Spaces"
      className="p-1"
    >
      <div className="px-2 pb-1 pt-1.5 text-[11px] font-semibold text-content/35">
        Spaces
      </div>
      {view.spaces.map((space) => (
        <SpaceRow
          key={space.id}
          space={space}
          current={space.id === view.activeSpaceId}
          activity={activity.get(space.id)}
          logins={view.logins}
          onPick={() => {
            onClose();
            void switchSpace(space.id).catch((error) =>
              console.error("monocode: could not switch Space", error),
            );
          }}
        />
      ))}
      <div className="my-1 h-px bg-content/10" />
      <MenuItem icon={Plus} label="New Space" onClick={settings} />
      <MenuItem icon={Settings} label="Manage Spaces" onClick={settings} />
    </Popover>
  );
}

function SpaceRow({
  space,
  current,
  activity,
  logins,
  onPick,
}: {
  space: Space;
  current: boolean;
  /** Undefined for a Space with no Mono. */
  activity: SpaceActivity | undefined;
  logins: SpacesView["logins"];
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={current}
      data-space-row={space.id}
      onClick={onPick}
      className={`flex w-full items-start gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] ${
        current ? "bg-selection" : "hover:bg-content/[0.04]"
      }`}
    >
      <span className="flex min-w-0 flex-1 flex-col gap-[3px]">
        <span className="truncate font-medium leading-4 text-content">
          {space.name}
        </span>
        <SpaceLogins spaceId={space.id} logins={logins} withPlan />
      </span>
      <span className="pt-px">
        {current ? (
          <Check className="size-3.5 text-content" strokeWidth={1.9} />
        ) : (
          <span className="flex items-center gap-1.5">
            {activity?.working ? (
              <span className="text-[10.5px] text-accent">
                {activity.working} working
              </span>
            ) : null}
            {activity?.unread ? (
              <span
                data-space-unread={activity.unread}
                className="grid h-4 min-w-4 place-items-center rounded-full bg-accent px-1 text-[10px] font-medium tabular-nums leading-none text-white"
              >
                {countBadge(activity.unread)}
              </span>
            ) : null}
          </span>
        )}
      </span>
    </button>
  );
}

function MenuItem({
  icon: Icon,
  label,
  onClick,
}: {
  icon: typeof Plus;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className="flex h-8 w-full items-center gap-2.5 rounded-lg px-2 text-left text-[13px] text-content/85 hover:bg-content/[0.07]"
    >
      <Icon className="size-3.5 text-content/55" strokeWidth={1.75} />
      {label}
    </button>
  );
}

/** "Space 2: 2 new replies" for each other Space with unread replies. */
function newsTitle(
  spaces: readonly Space[],
  activity: ReadonlyMap<string, SpaceActivity>,
  activeId: string,
): string {
  return spaces
    .filter((space) => space.id !== activeId)
    .flatMap((space) => {
      const unread = activity.get(space.id)?.unread ?? 0;
      if (!unread) return [];
      return [`${space.name}: ${unread === 1 ? "1 new reply" : `${unread} new replies`}`];
    })
    .join(", ");
}
