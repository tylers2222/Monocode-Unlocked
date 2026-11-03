import { useEffect, useState } from "react";
import { Layers, Pencil, Plus, Trash2 } from "../../../shared/ui/icons";
import {
  getSpaceSettings,
  listSpaceLogins,
  listSpaces,
  SPACE_LOGIN_PROVIDERS,
  type Space,
  type SpaceLogin,
} from "../model/spaces";
// PLACEHOLDER (lead, 2026-11-03): ADE-179 owns the one both-logins line; its
// path and props replace this import when it lands.
import { SpaceLogins } from "./SpaceLogins";

// New Space, rename and remove are drawn on frame 3 but what they open is not
// (frames 11-14 pending), so the buttons do nothing yet.

type SpaceLoginProvider = (typeof SPACE_LOGIN_PROVIDERS)[number];

type SpaceRow = {
  space: Space;
  /** The Space this window shows. */
  current: boolean;
  /** Login id per provider; null when the Space has none. */
  logins: Record<SpaceLoginProvider, string | null>;
};

/** One row per Space, in the order Rust lists them. */
export function spaceRows(
  spaces: Space[],
  logins: SpaceLogin[],
  activeSpaceId: string,
): SpaceRow[] {
  const loginOf = (spaceId: string, provider: SpaceLoginProvider) =>
    logins.find(
      (login) => login.spaceId === spaceId && login.provider === provider,
    )?.accountId ?? null;
  return spaces.map((space) => ({
    space,
    current: space.id === activeSpaceId,
    logins: {
      claude: loginOf(space.id, "claude"),
      codex: loginOf(space.id, "codex"),
    },
  }));
}

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
export function SpacesList() {
  const [rows, setRows] = useState<SpaceRow[] | null>(null);

  useEffect(() => {
    Promise.all([listSpaces(), listSpaceLogins(), getSpaceSettings()])
      .then(([spaces, logins, settings]) =>
        setRows(spaceRows(spaces, logins, settings.activeSpaceId)),
      )
      // No frame draws a failed load; the card stays empty.
      .catch((error: unknown) => console.warn("Could not load Spaces", error));
  }, []);

  return rows?.map((row) => <SpaceListRow key={row.space.id} row={row} />);
}

function SpaceListRow({ row }: { row: SpaceRow }) {
  const { space } = row;
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
          {row.current ? (
            <span className="shrink-0 rounded bg-accent/15 px-1.5 text-[10px] font-medium leading-4 text-accent">
              This window
            </span>
          ) : null}
        </div>
        <SpaceLogins logins={row.logins} className="mt-0.5" />
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
        {row.current ? null : (
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
