import { leafIds, newTab, type WorkspaceTab } from "../../workspace/model/layout";
import type { DockSide, ProjectTerminalDock } from "../../projects/model/projectTerminal";
import { sessionNeedsInput, type QueuedMessage, type Session } from "./session";
import { stopStreaming } from "../../../integrations/harness/core/apply";
import type { ProjectReturnMemory } from "../../projects/model/projectReturn";

export const INTERRUPT_MESSAGE =
  "Turn interrupted when MonoCode quit.";

export const CONTINUE_PROMPT = "Continue from where you left off.";

export type InFlightRef = {
  sessionId: string;
  cwd: string;
};

export type ResumedWorkspace = {
  sessions: Session[];
  tabs: WorkspaceTab[];
  activeTabId: string;
  projectCwd: string;
  projectTerminals?: ProjectTerminalDock[];
  projectReturnMemory?: ProjectReturnMemory;
  lastDockSide?: DockSide;
};

/** A turn or approval that would be lost if this webview died. */
export function isInFlightSession(session: Session): boolean {
  return (
    !session.worktreeRemoved && (!!session.busy || sessionNeedsInput(session))
  );
}

export function hasInFlightSessions(sessions: Session[]): boolean {
  return sessions.some(isInFlightSession);
}

/**
 * Busy chats in tab order, then parked ones still running after their tab closed.
 * Only persistable sessions can come back after a real quit.
 */
export function inFlightRefs(
  sessions: Session[],
  tabs: WorkspaceTab[],
): InFlightRef[] {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  const seen = new Set<string>();
  const refs: InFlightRef[] = [];

  const push = (session: Session | undefined) => {
    if (!session || session.inboxAsk || seen.has(session.id)) return;
    if (!isInFlightSession(session) || !canResumeAfterQuit(session)) return;
    seen.add(session.id);
    refs.push({ sessionId: session.id, cwd: session.cwd });
  };

  for (const tab of tabs) {
    for (const id of leafIds(tab.layout)) push(byId.get(id));
  }
  for (const session of sessions) push(session);
  return refs;
}

export function quitWhileBusyMessage(count: number): string {
  if (count === 1) {
    return "1 chat is still running. Quit anyway? It will resume when you reopen MonoCode.";
  }
  return `${count} chats are still running. Quit anyway? They will resume when you reopen MonoCode.`;
}

/**
 * Seal streams/tools and record that a real quit cut the turn short.
 * Idempotent for the current turn only — a later turn after Continue still
 * gets a fresh note. Matching any interrupt in the transcript would skip
 * resume on the second quit.
 */
export function markTurnInterrupted(session: Session): Session {
  const sealed = { ...sealOpenWork(stopStreaming(session)), busy: false };
  if (lastBlockIsInterrupt(sealed)) return sealed;
  const requeued = requeueUnansweredNotice(sealed);
  if (requeued) return requeued;
  if (noticeAlreadyRequeued(session)) return sealed;
  return {
    ...sealed,
    blocks: [
      ...sealed.blocks,
      {
        id: crypto.randomUUID(),
        role: "system",
        text: INTERRUPT_MESSAGE,
        notice: "interrupt",
      },
    ],
  };
}

/**
 * ADE-195: a Mono completion notice opened the turn the quit cut, and no
 * agent output follows it. The turn's commit took the notice out of the
 * saved queue before the agent had its prompt, and the Continue sent after a
 * relaunch does not carry it. So the agent may never have seen it. The
 * notice goes back to the head of the queue, and its opener leaves the
 * transcript. The Mono store marked the notice delivered when a save held
 * it (`mono_completions::settle`), so a requeued notice is not reported
 * again. If the agent read the prompt but did not answer before the quit,
 * it gets the notice a second time. That is better than never.
 */
function requeueUnansweredNotice(session: Session): Session | undefined {
  const { blocks } = session;
  let index = blocks.length - 1;
  while (index >= 0 && blocks[index].role === "system") index -= 1;
  const opener = blocks[index];
  if (
    opener?.role !== "user" ||
    !opener.monoSessionCompletion ||
    !opener.appRequestId
  )
    return undefined;
  const queued = session.queuedMessages ?? [];
  // A queue the user or an error paused stays paused. Any other queue is
  // paused by the quit, so the app sends it again when the Mono is idle.
  const heldByUser =
    session.queueStatus === "paused" &&
    queued.length > 0 &&
    !queued.some((message) => message.pausedBy === "quit");
  const notice: QueuedMessage = {
    id: opener.appRequestId,
    text: opener.text,
    attachments: [],
    monoSessionCompletion: opener.monoSessionCompletion,
    queuedAt: Date.now(),
    ...(heldByUser ? {} : { pausedBy: "quit" as const }),
  };
  return {
    ...session,
    // Only the opener leaves: a system note after it (a provider error, say)
    // stays where it was.
    blocks: [...blocks.slice(0, index), ...blocks.slice(index + 1)],
    queuedMessages: [notice, ...queued],
    queueStatus: "paused",
  };
}

/**
 * The second pass over a requeued notice. A graceful quit marks the session
 * when it saves it, and the relaunch marks the saved record again. That
 * record is idle, needs no input, and holds the notice at its queue head
 * instead of as an opener. No turn ran, so it gets no interrupt note and no
 * Continue: the queue sends the notice.
 */
function noticeAlreadyRequeued(session: Session): boolean {
  return (
    !session.busy &&
    !sessionNeedsInput(session) &&
    !!session.queuedMessages?.[0]?.monoSessionCompletion
  );
}

export function workspaceFromResumed(
  sessions: Session[],
): ResumedWorkspace | null {
  sessions = sessions.filter((session) => !session.inboxAsk);
  if (sessions.length === 0) return null;
  const tabs = sessions.map((session) => newTab(session.id));
  return {
    sessions,
    tabs,
    activeTabId: tabs[0].id,
    projectCwd: sessions[0].cwd,
  };
}

export function wasTurnInterrupted(session: Session): boolean {
  return lastBlockIsInterrupt(session);
}

/**
 * Provider thread exists and the quit note is still the last block.
 * A Continue (or any later user turn) appends after it, so this stays one-shot.
 */
export function canAutoContinue(session: Session): boolean {
  return (
    !session.worktreeRemoved &&
    !!session.providerSessionId &&
    !session.busy &&
    lastBlockIsInterrupt(session)
  );
}

function lastBlockIsInterrupt(session: Session): boolean {
  const last = session.blocks[session.blocks.length - 1];
  return last?.role === "system" && last.text === INTERRUPT_MESSAGE;
}

export function inFlightSnapshotKey(refs: InFlightRef[]): string {
  return refs.map((ref) => ref.sessionId).join("\n");
}

/**
 * Skip the first idle paint after boot so a restored snapshot is not wiped
 * before the turn is marked busy again. Once this process has seen a live
 * in-flight chat, an empty list is a real "turn finished" and should clear.
 */
export function shouldWriteInFlightSnapshot(
  key: string,
  refs: InFlightRef[],
  previousKey: string | null,
  sawInFlight: boolean,
): boolean {
  if (previousKey === key) return false;
  if (refs.length === 0 && !sawInFlight) return false;
  return true;
}

function canResumeAfterQuit(session: Session): boolean {
  return (
    !session.worktreeRemoved &&
    session.cwd !== "~" &&
    session.blocks.some((block) => block.role === "user")
  );
}

function sealOpenWork(session: Session): Session {
  return {
    ...session,
    busy: false,
    blocks: session.blocks.flatMap((block) => {
      if (block.role === "approval" && !block.approval?.decided) return [];
      if (!shouldCancelTool(block)) return [block];
      const { approval, ...rest } = block;
      return [
        {
          ...rest,
          streaming: false,
          tool: block.tool
            ? { ...block.tool, status: "cancelled" }
            : block.tool,
          ...(approval?.decided ? { approval } : {}),
        },
      ];
    }),
  };
}

function shouldCancelTool(block: Session["blocks"][number]): boolean {
  if (block.approval && !block.approval.decided) return true;
  if (!block.tool) return false;
  if (block.streaming) return true;
  const status = block.tool.status?.toLowerCase() ?? "";
  return (
    status === "in_progress" || status === "pending" || status === "running"
  );
}
