import { invoke } from "@tauri-apps/api/core";
import {
  savedMonoCompletions,
  type MonoCompletionStore,
  type SavedMonoCompletion,
} from "../../monos/model/monoSessionCompletion";

let writes: Promise<void> = Promise.resolve();

/** Writes reach Rust in call order, so an older set never replaces a newer one. */
function write(command: string, args: Record<string, unknown>) {
  writes = writes.then(() => invoke<void>(command, args)).catch(console.error);
}

/** mono_completions.rs, through the window's commands. */
export const monoCompletionStore: MonoCompletionStore = {
  hold: (saved) => write("mono_completions_hold", { saved }),
  dismiss: (monoId, sessionId) =>
    write("mono_completions_dismiss", { monoId, sessionId }),
};

/**
 * Settles once every write made so far has reached Rust. A Mono chat save
 * waits for it, so a notice's rows carry that notice before a save can hold it.
 */
export function monoCompletionWrites(): Promise<void> {
  return writes;
}

/** This window's boot takes the results no live window holds. */
export async function claimMonoCompletions(): Promise<SavedMonoCompletion[]> {
  return savedMonoCompletions(await invoke<unknown>("mono_completions_claim"));
}
