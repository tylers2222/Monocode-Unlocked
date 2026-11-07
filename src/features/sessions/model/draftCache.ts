/**
 * In-memory composer drafts, keyed by session id.
 *
 * SessionPane keeps the text you are typing in a React ref so retyping while
 * the pane stays mounted does not re-render on every keystroke. A plain ref
 * only lives as long as that one component instance though: closing a
 * session's pane (or moving it to another split) unmounts SessionPane, and
 * the ref - and whatever you had typed - is gone with no warning the moment
 * it remounts.
 *
 * This module-level map survives that, because it lives outside the React
 * tree for as long as the app process is running: the draft comes back when
 * the pane for that session opens again.
 *
 * It does not survive an app restart or a full reload - that needs the
 * draft written into the session's persisted record on disk, which is a
 * separate, larger change (touches the Rust-side session_upsert schema
 * too).
 */
import type { Attachment } from "./session";
import type { McpTag } from "./mcpPicker";

/** What a Composer holds besides its text, kept while its pane is hidden. */
export type ComposerStash = {
  attachments: Attachment[];
  /** Files lent by a recalled turn, whose previews the turn still owns. */
  borrowedAttachmentIds: string[];
  plan: boolean;
  operator: boolean;
  orchestration: boolean;
  draft: boolean;
};

const drafts = new Map<string, string>();
const mcpTags = new Map<string, McpTag[]>();
const stashes = new Map<string, ComposerStash>();

export function getComposerDraft(sessionId: string): string | undefined {
  return drafts.get(sessionId);
}

export function setComposerDraft(sessionId: string, text: string): void {
  if (text) {
    drafts.set(sessionId, text);
  } else {
    drafts.delete(sessionId);
    mcpTags.delete(sessionId);
  }
}

export function getComposerMcpTags(sessionId: string): McpTag[] {
  return mcpTags.get(sessionId) ?? [];
}

export function setComposerMcpTags(sessionId: string, tags: McpTag[]): void {
  if (tags.length > 0) {
    mcpTags.set(sessionId, tags);
  } else {
    mcpTags.delete(sessionId);
  }
}

export function getComposerStash(sessionId: string): ComposerStash | undefined {
  return stashes.get(sessionId);
}

/** A stash holding nothing unsent is dropped. */
export function setComposerStash(
  sessionId: string,
  stash: ComposerStash | null,
): void {
  if (
    stash &&
    (stash.attachments.length > 0 ||
      stash.plan ||
      stash.operator ||
      stash.orchestration ||
      stash.draft)
  ) {
    stashes.set(sessionId, stash);
  } else {
    stashes.delete(sessionId);
  }
}

const EMPTY_STASH: ComposerStash = {
  attachments: [],
  borrowedAttachmentIds: [],
  plan: false,
  operator: false,
  orchestration: false,
  draft: false,
};

/** Records what changed in a mounted Composer, so a remount finds it. */
export function updateComposerStash(
  sessionId: string,
  patch: Partial<ComposerStash>,
): void {
  setComposerStash(sessionId, {
    ...(stashes.get(sessionId) ?? EMPTY_STASH),
    ...patch,
  });
}

export function clearComposerDraft(sessionId: string): void {
  drafts.delete(sessionId);
  mcpTags.delete(sessionId);
  stashes.delete(sessionId);
}
