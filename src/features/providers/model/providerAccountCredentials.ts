import { invoke } from "@tauri-apps/api/core";
import {
  removeSavedProviderAccount,
  type ProviderAccountProvider,
} from "./providerAccounts";

/**
 * Remove a named profile from `monocode.db`, then its native credentials,
 * before its UI metadata. A refused removal throws and keeps the folder.
 */
export async function removeProviderAccountCredentials(
  provider: ProviderAccountProvider,
  accountId: string,
): Promise<void> {
  await removeSavedProviderAccount(provider, accountId);
  await invoke("provider_account_remove", { provider, accountId });
}
