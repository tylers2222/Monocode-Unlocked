import { invoke } from "@tauri-apps/api/core";
import {
  savedMonoCompletions,
  type MonoCompletionStore,
  type SavedMonoCompletion,
} from "../../monos/model/monoSessionCompletion";

let storeCalls: Promise<void> = Promise.resolve();

/** In call order, so a delivery never reaches Rust before the hold it follows. */
function write(command: string, args: Record<string, unknown>) {
  storeCalls = storeCalls
    .then(() => invoke<void>(command, args))
    .catch(console.error);
}

/** mono_completions.rs, through the window's commands. */
export const monoCompletionStore: MonoCompletionStore = {
  hold: (saved) => write("mono_completions_hold", { saved }),
  deliver: (groupId, requestIds) =>
    write("mono_completions_deliver", { groupId, requestIds }),
};

/** This window's boot takes the results no live window holds. */
export async function claimMonoCompletions(): Promise<SavedMonoCompletion[]> {
  return savedMonoCompletions(await invoke<unknown>("mono_completions_claim"));
}
