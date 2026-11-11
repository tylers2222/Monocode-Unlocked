import { spaceProjectKey } from "../../spaces/model/spaceProjects";
import {
  listSpaceProjects,
  removeProjectFromSpace,
  refusalMessage,
} from "../../spaces/model/spaces";
import { checkWindowRunning } from "./projectDeleteCheck";

/**
 * T5 or T6. The window refuses what only it can see, unless a Mono holds the
 * project: that refusal comes first, and it is the backend's. The backend then
 * decides in one transaction. `removed` false: it was not in this Space.
 */
export async function removeProjectFromActiveSpace(
  path: string,
): Promise<{ refusal: string } | { removed: boolean }> {
  try {
    const key = spaceProjectKey(path);
    const held = await listSpaceProjects().then(
      (list) => list.monoHeld.some((item) => spaceProjectKey(item) === key),
      () => false,
    );
    const refusal = held ? null : await checkWindowRunning(path);
    if (refusal) return { refusal };
    return { removed: await removeProjectFromSpace(path) };
  } catch (error) {
    return { refusal: refusalMessage(error) };
  }
}
