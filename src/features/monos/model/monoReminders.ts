import { invoke } from "@tauri-apps/api/core";
import type { QueuedMessage } from "../../sessions/model/session";
import { steersQueuedNow } from "../../sessions/model/messageQueue";
import { ensureNotice, type MonoQueueHost } from "./monoRoute";

/** Rust `ask_scheduler::Claimed`: the prompt of a reminder this window took. */
type Claimed = { text: string; wake: boolean };

/**
 * R9: each reminder that waits for the chat, claimed with the chat's live
 * state. Rust picks its prompt and its delivery: a wake waits for the turn
 * end, a steer joins the running turn; a reminder for a busy turn that
 * cannot take a steer stays unclaimed until the turn end names the chat
 * again. A claimed one is a hidden app prompt whose id is its outbox key.
 */
export async function queueReminders(host: MonoQueueHost, sessionId: string) {
  const keys = await invoke<string[]>("mono_outbox_pending", { sessionId });
  for (const key of keys) {
    // A chat this window closed meanwhile gets its reminders at its next load.
    const session = host.session(sessionId);
    if (!session) return;
    const claimed = await invoke<Claimed | null>("mono_outbox_claim", {
      key,
      steerable: steersQueuedNow(session, host.isMonoChat(sessionId)),
    });
    if (!claimed) continue;
    host.update(sessionId, (session) =>
      ensureNotice(session, {
        id: key,
        text: claimed.text,
        monoReminder: true,
        waitForIdle: claimed.wake,
      }),
    );
  }
}

/**
 * R9: the queue pump's submit of item `item`, which is no Mono message. A
 * reminder stays queued, with its claim, while no provider is live (as a
 * Mono message does, design 5.5 rule 4); the provider taking it marks its
 * outbox row sent, once.
 */
export function submitQueued<T>(
  item: QueuedMessage,
  live: boolean,
  submit: (options: { providerTook?: (accepted: boolean) => void }) => T,
): T | false {
  if (!item.monoReminder) return submit({});
  if (!live) return false;
  return submit({
    providerTook: (accepted) => {
      if (accepted)
        void invoke("mono_outbox_sent", { key: item.id }).catch(console.error);
    },
  });
}
