import { invoke } from "@tauri-apps/api/core";
import {
  savedMonoCompletions,
  type MonoCompletionStore,
  type SavedMonoCompletion,
} from "../../monos/model/monoSessionCompletion";

let holds: Promise<void> = Promise.resolve();

/**
 * mono_completions.rs, through the window's commands. Holds reach Rust in
 * call order, so an older set never replaces a newer one.
 */
export const monoCompletionStore: MonoCompletionStore = {
  hold: (saved) => {
    holds = holds
      .then(() => invoke<void>("mono_completions_hold", { saved }))
      .catch(console.error);
  },
};

/** Settles once every hold made so far has reached Rust. */
export function monoCompletionWrites(): Promise<void> {
  return holds;
}

/** This window's boot takes the results no live window holds. */
export async function claimMonoCompletions(): Promise<SavedMonoCompletion[]> {
  return savedMonoCompletions(await invoke<unknown>("mono_completions_claim"));
}
