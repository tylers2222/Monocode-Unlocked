import { Archive, Minus, Trash2 } from "../../../shared/ui/icons";
import type { TabGroupMenuExtraItem } from "../../workspace/ui/TabGroupMenu";
import type { ProjectRowFacts } from "../model/projectRowActions";

export const REMOVE_FROM_SPACE = "remove-from-space";

/**
 * Frame 20's rows. A row only in this Space has today's two, with no lines;
 * a remote Delete always says the machine is untouched (T16).
 */
export function projectRemovalItems(
  spaceName: string,
  facts: ProjectRowFacts | null,
  remote: boolean,
): TabGroupMenuExtraItem[] {
  const shared = facts?.offersRemove === true;
  const items: TabGroupMenuExtraItem[] = [
    {
      id: "archive",
      label: "Archive",
      icon: Archive,
      description: shared ? "Hides it in every Space." : undefined,
    },
    {
      id: "delete",
      label: "Delete",
      icon: Trash2,
      danger: true,
      description: remote
        ? "Takes it off this Space. Nothing on the machine changes."
        : shared
          ? `${spaceName}'s conversations only. You confirm next.`
          : undefined,
    },
  ];
  if (shared) {
    items.unshift({
      id: REMOVE_FROM_SPACE,
      label: `Remove from ${spaceName}`,
      icon: Minus,
      description: remote
        ? `Nothing on ${facts?.machine ?? "the machine"} changes.`
        : "Its conversations stay. They come back if you add it again.",
    });
  }
  items[0] = { ...items[0], sepBefore: true };
  return items;
}
