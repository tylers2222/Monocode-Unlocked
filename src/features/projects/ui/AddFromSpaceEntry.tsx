import { useRef, useState } from "react";
import { LAYER } from "../../../shared/lib/layers";
import { ChevronRight, Layers } from "../../../shared/ui/icons";
import { spacesView } from "../../spaces/model/activeSpace";
import { useAddableProjects } from "../../spaces/model/addableProjects";
import { addProjectToSpace, refusalMessage } from "../../spaces/model/spaces";
import {
  ADD_FROM_SPACE_FLYOUT,
  ProjectPickerPopover,
} from "./SearchableProjectPicker";

export { ADD_FROM_SPACE_FLYOUT };

/**
 * "Add from another Space…" with its flyout: T2 for the project picked. Hidden
 * when no other Space has anything to add. A refused add keeps the list open
 * with the backend's reason.
 */
export function AddFromSpaceEntry({
  className,
  iconClassName,
  menuItem = false,
  onAdded,
}: {
  className: string;
  iconClassName: string;
  /** A row of a menu, with a flyout arrow, not of the picker's footer. */
  menuItem?: boolean;
  onAdded: () => void;
}) {
  const projects = useAddableProjects();
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  if (projects.length === 0) return null;
  const { spaces, activeSpaceId } = spacesView();
  const spaceName = (id: string) =>
    spaces.find((space) => space.id === id)?.name ?? id;
  const add = (path: string) => {
    setRefusal(null);
    // Every path here is another Space's or a recent, so it is never new (T2).
    void addProjectToSpace(path, true).then(onAdded, (error: unknown) =>
      setRefusal(refusalMessage(error)),
    );
  };
  return (
    <>
      <button
        ref={anchor}
        type="button"
        role={menuItem ? "menuitem" : undefined}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className={className}
      >
        <Layers className={iconClassName} strokeWidth={1.75} />
        <span className="min-w-0 flex-1 truncate">Add from another Space…</span>
        {menuItem ? (
          <ChevronRight className="size-3.5 shrink-0" strokeWidth={1.75} />
        ) : null}
      </button>
      {open ? (
        <ProjectPickerPopover
          anchor={anchor}
          flyout={{
            layer: LAYER.submenu,
            attributes: { "data-add-from-space": true },
          }}
          label="Add from another Space"
          projects={projects.map(({ path }) => ({ path, openedAt: 0 }))}
          projectTag={(path) => {
            const entry = projects.find((item) => item.path === path);
            return entry && spaceName(entry.spaceId);
          }}
          note={
            <div className="shrink-0 border-t border-stroke px-3 py-2 text-[11px] leading-snug">
              <p className="text-content/45">
                Adds it to {spaceName(activeSpaceId)}’s list.
              </p>
              <p className="text-content/45">Nothing is copied.</p>
              {refusal ? (
                <p role="alert" className="mt-1 text-red-400">
                  {refusal}
                </p>
              ) : null}
            </div>
          }
          onDismiss={() => undefined}
          onSelectProject={add}
        />
      ) : null}
    </>
  );
}
