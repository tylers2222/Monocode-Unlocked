import {
  rememberProject,
  type RecentProject,
} from "../../features/projects/model/recents";
import type { QuickLaunch } from "../../features/quick-composer/model/quickComposer";
import { applyQuickWorkspace } from "../../features/quick-composer/model/quickWorkspace";
import { prepareAttachments } from "../../features/sessions/model/attachments";
import {
  mergeModelSettings,
  resolveModel,
} from "../../features/sessions/model/models";
import {
  newSession,
  sessionDraftBlock,
  type Session,
  type Attachment,
} from "../../features/sessions/model/session";
import {
  newTab,
  type SplitDir,
  type WorkspaceTab,
} from "../../features/workspace/model/layout";
import type { SubmissionAcceptance } from "./submissionAcceptance";

/**
 * Whether the app call that launches session `id` with `launch` already
 * did. Every app launch stamps `appRequestId` on its first user block, so a
 * repeat compares its text, its draft state and its caller (`startedBy`:
 * two callers' ids can join to one session id). A session with submitted
 * turns and no stamp was not launched by this call: the user sent its draft
 * or wrote it, so a start row would name the user's own turn (PR 7 review
 * r1 m5, n4). Throws on any conflict; false means launch it.
 */
export function appLaunchRepeat(
  existing: Session | null | undefined,
  launch: QuickLaunch,
  id: string,
): boolean {
  const previous = existing?.blocks.find((block) => block.appRequestId === id);
  if (previous) {
    if (
      previous.text !== launch.prompt ||
      previous.startedBy !== launch.startedBy ||
      (!launch.draft && !!previous.draft)
    )
      throw new Error("Request ID was already used for another session launch");
    return true;
  }
  if (existing?.blocks.some((block) => block.role === "user" && !block.draft))
    throw new Error("Request ID was already used for another session launch");
  if (existing && sessionDraftBlock(existing))
    throw new Error("Session ID already has a different draft");
  return false;
}

/** Complete the workspace handoff before the receiver acknowledges the launch. */
export async function acceptQuickLaunch(
  launch: QuickLaunch,
  deliveryId: string,
  workspace: {
    getSessions: () => Session[];
    updateSessions: (update: (sessions: Session[]) => Session[]) => void;
    appendTab: (tab: WorkspaceTab, cwd: string) => void;
    placeSession?: (
      sessionId: string,
      placement: { direction: SplitDir; besideSessionId: string },
      cwd: string,
    ) => string;
    setProjectCwd: (cwd: string) => void;
    setRecents: (recents: RecentProject[]) => void;
    revealTab: (id: string, cwd: string) => void;
    submit: (
      sessionId: string,
      text: string,
      attachments: Attachment[],
      options?: { intent?: "plan" | "orchestrate"; startedBy?: string },
    ) => SubmissionAcceptance;
    saveDraft?: (
      sessionId: string,
      text: string,
      attachments: Attachment[],
      requestId: string,
      startedBy?: string,
    ) => SubmissionAcceptance;
    /**
     * Design 13.2: saves the lead of the worker this launch makes, so the
     * worker's first save finds the lead's row. Throws when it cannot.
     */
    persistLead?: () => Promise<void>;
  },
  placement?: { direction: SplitDir; besideSessionId: string },
  /** Off for a session followed elsewhere, such as from the Mono that ran it. */
  openTab = true,
): Promise<void> {
  // Rehydrate image previews in this webview; the floating panel sends paths.
  const attachments = await prepareAttachments(launch.attachments ?? []);
  await workspace.persistLead?.();
  const existing = workspace
    .getSessions()
    .find((session) => session.id === deliveryId);
  const previous = existing?.blocks.find(
    (block) => block.appRequestId === deliveryId,
  );
  if (previous) {
    if (previous.text !== launch.prompt || (!launch.draft && !!previous.draft))
      throw new Error("Request ID was already used for another session launch");
    return;
  }
  if (
    existing?.quickLaunchAccepted ||
    existing?.blocks.some((block) => block.role === "user" && !block.draft)
  )
    return;
  const session =
    existing ??
    applyQuickWorkspace(
      newSession(launch.harness, launch.cwd, launch.model, launch.runtimeMode),
      launch,
    );
  session.id = deliveryId;
  if (!existing && launch.sidebarHidden) session.sidebarHidden = true;
  if (!existing && launch.leadSessionId)
    session.leadSessionId = launch.leadSessionId;
  if (launch.modelSettings) {
    session.modelSettings = mergeModelSettings(
      resolveModel(session.harness, session.model),
      launch.modelSettings,
    );
  }
  if (!existing) {
    const tab = newTab(session.id);
    // Resolve the target before adding the session, so a closed pane cannot
    // leave behind an orphaned launch if placement fails.
    const tabId = placement
      ? workspace.placeSession?.(session.id, placement, launch.cwd)
      : tab.id;
    if (!tabId) throw new Error("The target pane is unavailable");
    workspace.updateSessions((sessions) => [...sessions, session]);
    if (!placement && openTab) workspace.appendTab(tab, launch.cwd);
    if (launch.reveal && (placement || openTab)) {
      // The title bar filters tabs by this project. Select it before the tab.
      workspace.setProjectCwd(launch.cwd);
      workspace.setRecents(rememberProject(launch.cwd));
      workspace.revealTab(tabId, launch.cwd);
    }
  }
  if (launch.draft) {
    if (
      !workspace.saveDraft ||
      !(await workspace.saveDraft(
        session.id,
        launch.prompt,
        attachments,
        deliveryId,
        ...(launch.startedBy ? [launch.startedBy] : []),
      ))
    ) {
      throw new Error("The workspace could not save the session draft yet.");
    }
  } else if (
    !(await workspace.submit(session.id, launch.prompt, attachments, {
      ...(launch.intent ? { intent: launch.intent } : {}),
      ...(launch.startedBy ? { startedBy: launch.startedBy } : {}),
    }))
  ) {
    throw new Error("The workspace could not accept the queued session yet.");
  }
  workspace.updateSessions((sessions) =>
    sessions.map((item) =>
      item.id === deliveryId ? { ...item, quickLaunchAccepted: true } : item,
    ),
  );
}
