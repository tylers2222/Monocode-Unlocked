import { prettyCwd } from "../../../shared/lib/paths";
import { parseRemotePath } from "../../connections/model/remoteProjects";
import { spacesView } from "../../spaces/model/activeSpace";
import {
  previewProjectDelete,
  type ProjectDeletePreview,
} from "../../spaces/model/spaces";
import { activeSpaceName, remoteMachineName } from "./projectRowActions";
import { isRemoteProjectPath } from "./recents";

/** Frame 22's dialog, as text. */
export type ProjectDeleteText = {
  title: string;
  /** What goes, muted. */
  summary: string;
  /** What stays, brighter. */
  keeps: string[];
  count: string | null;
  location: string;
  confirm: string;
};

/**
 * `A`, `A and B`, `A, B and C`. A delete that is not on the last Space with no
 * Space named (an older backend) still has one: `another Space`.
 */
function joined(names: readonly string[]): string {
  return names.length > 1
    ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`
    : (names[0] ?? "another Space");
}

function savedCount(count: number, space: string): string | null {
  if (count <= 0) return null;
  const noun = count === 1 ? "conversation" : "conversations";
  return `${count} saved ${noun} in ${space} will be removed.`;
}

/** `Space 1 keeps` or `Space 1 and Space 3 keep`. */
function othersKeep(others: readonly string[]): string {
  return `${joined(others)} ${others.length > 1 ? "keep" : "keeps"}`;
}

/** T10, T11 and T16, from what the backend said the delete would do. */
export function projectDeleteText(input: {
  name: string;
  path: string;
  space: string;
  preview: ProjectDeletePreview;
  /** Other Spaces' names, for `preview.otherSpaces`. */
  others: readonly string[];
  /** Set on a remote project. */
  machine?: string;
}): ProjectDeleteText {
  const { name, path, space, preview, others, machine } = input;
  const location = machine
    ? `${prettyCwd(parseRemotePath(path)?.hostPath ?? path)} on ${machine}`
    : prettyCwd(path);
  const last = preview.lastSpace === true;
  const title = last
    ? `Delete “${name}”?`
    : `Delete “${name}” from ${space}?`;
  const confirm = last ? "Delete" : `Delete from ${space}`;
  if (machine) {
    return {
      title,
      summary: last
        ? `${space} is the last Space with this project. The project's milestones, logo and settings will be deleted. It leaves ${space}'s sidebar, and nothing on ${machine} changes.`
        : `It leaves ${space}'s sidebar, and nothing on ${machine} changes. ${othersKeep(others)} the project, and its milestones, logo and settings stay.`,
      keeps: [],
      count: null,
      location,
      confirm,
    };
  }
  if (last) {
    return {
      title,
      summary: `${space} is the last Space with this project. ${space}'s conversations and the project's milestones, logo and settings will be deleted. It also leaves the sidebar. The folder on disk stays put.`,
      keeps:
        preview.hiddenSessionCount > 0
          ? ["Conversations hidden in other Spaces stay, and come back if it is added there again."]
          : [],
      count: savedCount(preview.sessionCount, space),
      location,
      confirm,
    };
  }
  return {
    title,
    summary: `${space}'s conversations for this project will be deleted, and it leaves ${space}'s sidebar. The folder on disk stays put.`,
    keeps: [
      `${othersKeep(others)} ${others.length > 1 ? "their" : "its"} conversations, and the project's milestones, logo and settings stay.`,
    ],
    count: savedCount(preview.sessionCount, space),
    location,
    confirm,
  };
}

/**
 * Reads the facts behind the dialog. Null when the preview cannot be read:
 * the dialog then offers no Delete (decision 7), since it cannot say what goes.
 */
export async function readProjectDeleteText(
  name: string,
  path: string,
): Promise<ProjectDeleteText | null> {
  let preview: ProjectDeletePreview;
  try {
    preview = await previewProjectDelete(path);
  } catch {
    return null;
  }
  const machine = isRemoteProjectPath(path)
    ? await remoteMachineName(path)
    : undefined;
  const { spaces } = spacesView();
  const others = preview.otherSpaces.map(
    (id) => spaces.find((space) => space.id === id)?.name ?? id,
  );
  return projectDeleteText({
    name,
    path,
    space: activeSpaceName(),
    preview,
    others,
    machine,
  });
}
