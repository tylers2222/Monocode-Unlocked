import { isHabitRun } from "../../monos/model/monoHabits";
import { sameProjectPath } from "../../projects/model/recents";
import type { SessionSummary } from "../../sessions/data/sessionStore";
import { sessionDraftBlock, type Session } from "../../sessions/model/session";
import type { AppSessionListing } from "./agentApp";

/**
 * The rows of `sessions.list`: the project's saved sessions, then the open ones
 * with their live busy state and draft. An open session keeps its saved branch,
 * which the app rewrites from git on every save; its in-memory branch is only
 * the one the app attached. `branch` is null, never absent, so a reader can tell
 * a session with no branch from an app that sends none.
 */
export function appSessionListings(
  cwd: string,
  stored: readonly SessionSummary[],
  open: readonly Session[],
): AppSessionListing[] {
  const byId = new Map<string, AppSessionListing>();
  for (const session of stored) {
    // A habit run belongs to its Mono until it ends.
    if (session.orchestrationLeadId || isHabitRun(session.id)) continue;
    byId.set(session.id, {
      id: session.id,
      title: session.title,
      harness: session.harness,
      model: session.model,
      busy: false,
      branch: session.branch ?? null,
      hasDraft: !!session.draft,
      archived: !!session.archived,
    });
  }
  for (const session of open) {
    // An Inbox Ask is no project session, as in `ownProject`; its
    // saved record lists as a hidden session (PR 7 review r2 n5).
    if (session.inboxAsk) {
      byId.delete(session.id);
      continue;
    }
    if (
      session.orchestrationLeadId ||
      isHabitRun(session.id) ||
      !sameProjectPath(session.cwd, cwd)
    )
      continue;
    const savedRow = byId.get(session.id);
    byId.set(session.id, {
      id: session.id,
      title: session.title,
      harness: session.harness,
      model: session.model,
      busy: !!session.busy,
      branch: savedRow?.branch ?? session.branch ?? null,
      hasDraft: !!sessionDraftBlock(session),
      archived: savedRow?.archived ?? false,
    });
  }
  return [...byId.values()];
}
