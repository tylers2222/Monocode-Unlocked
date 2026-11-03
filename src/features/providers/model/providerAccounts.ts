import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { pathKey } from "../../../shared/lib/paths";
import type { HarnessId } from "../../sessions/model/session";

const ACCOUNTS_KEY = "monocode.providerAccounts.v1";
const SELECTIONS_KEY = "monocode.providerAccountSelections.v1";
/** The project-choice import's entries the database took. */
const SELECTIONS_IMPORT_KEY = "monocode.providerAccountSelections.v1.import";
/** The login import's entries the database took, in the order they landed. */
const ACCOUNTS_IMPORT_KEY = "monocode.providerAccounts.v1.import";
const CHANGE_EVENT = "monocode-provider-accounts-changed";

export const DEFAULT_PROVIDER_ACCOUNT_ID = "default";
const DEFAULT_PROVIDER_ACCOUNT_LABEL = "Default account";

/** Legacy sessions predate persisted account ids and belong to the default profile. */
export function sameProviderAccountId(
  left: string | undefined,
  right: string | undefined,
): boolean {
  return (
    (left ?? DEFAULT_PROVIDER_ACCOUNT_ID) ===
    (right ?? DEFAULT_PROVIDER_ACCOUNT_ID)
  );
}

/** Providers listed under Accounts, each with at least its CLI's own login. */
export const PROVIDER_ACCOUNT_PROVIDERS = [
  "claude",
  "codex",
  "devin",
] as const satisfies readonly HarnessId[];

export type ProviderAccountProvider =
  (typeof PROVIDER_ACCOUNT_PROVIDERS)[number];

export function supportsProviderAccounts(
  provider: HarnessId,
): provider is ProviderAccountProvider {
  return PROVIDER_ACCOUNT_PROVIDERS.some((candidate) => candidate === provider);
}

/**
 * Providers whose CLIs can keep isolated, locally named profiles. Devin has
 * no config-dir override on Windows, so it offers only its default login.
 */
const PROFILE_PROVIDERS = [
  "claude",
  "codex",
] as const satisfies readonly ProviderAccountProvider[];

export type ProfileAccountProvider = (typeof PROFILE_PROVIDERS)[number];

export function supportsAccountProfiles(
  provider: HarnessId,
): provider is ProfileAccountProvider {
  return PROFILE_PROVIDERS.some((candidate) => candidate === provider);
}

export type ProviderAccount = {
  id: string;
  provider: ProviderAccountProvider;
  label: string;
  isDefault?: boolean;
};

type StoredAccounts = Partial<
  Record<ProviderAccountProvider, ProviderAccount[]>
>;
type StoredSelections = Record<
  string,
  Partial<Record<ProviderAccountProvider, string>>
>;
type ProjectLogin = {
  projectPath: string;
  provider: ProviderAccountProvider;
  accountId: string;
};

/** Browser storage predates Spaces: what it holds is Space 1's (`FIRST_SPACE`). */
const BROWSER_SPACE = "space-1";

/**
 * The records `monocode.db` holds, in the shape browser storage held them.
 * A record it does not hold, and every record until it loads or when it
 * cannot load, comes from browser storage.
 */
let saved: Map<string, string> | undefined;
/** The active Space: the snapshot holds its project choices. */
let space = BROWSER_SPACE;
let writes: Promise<unknown> = Promise.resolve();
/** Writes sent and not yet answered. */
let queued = 0;
/** Whether the snapshot shows a write the database has not taken. */
let stale = false;
/** Replaced by a fresh token each time the database refuses a write. */
let refusal: object = {};
/** After a failed boot: the boot, for the next write to start again. */
let restart: (() => Promise<void>) | undefined;
/** Other windows of this app, once loaded; each reloads its snapshot when one writes. */
let windows: BroadcastChannel | undefined;

/**
 * Loads the logins from `monocode.db` before the first render, and once
 * imports the logins browser storage holds, giving Space 1 each CLI's
 * `default`. The old key stays for the Space 2 merge (ADE-178). A failure
 * keeps browser storage, and the next write starts the boot again.
 */
export async function loadProviderAccounts(): Promise<void> {
  try {
    windows = new BroadcastChannel("monocode-provider-accounts");
    windows.onmessage = () => {
      writes = writes.then(loadSaved).catch(console.error);
    };
    await start();
    await writes;
  } catch (error) {
    bootFailed(error);
  }
}

/** A boot that could not load: the next write starts it again. */
function bootFailed(error: unknown): void {
  restart = start;
  console.error(error);
}

/**
 * Loads the saved logins and the active Space's project choices, then imports
 * browser storage's logins and, with Space 1 active and once those have run,
 * its project choices.
 */
async function start(): Promise<void> {
  const accounts = PROVIDER_ACCOUNT_PROVIDERS.flatMap(providerAccounts);
  const choices = Object.entries(readRecord<StoredSelections>(SELECTIONS_KEY));
  const before = queued;
  ({ activeSpaceId: space } = await invoke<{ activeSpaceId: string }>(
    "spaces_settings_get",
  ));
  await loadSaved();
  // The import reads every earlier write; only one made while this loaded is lost.
  stale = queued > before;
  await importApart(
    ACCOUNTS_IMPORT_KEY,
    // `default` cannot be saved, only renamed, so every login is both.
    ...accounts.map((account): ImportEntry => [
      `${account.provider}/${account.id}`,
      () => {
        saveProviderAccount(account);
        renameProviderAccount(account.provider, account.id, account.label);
      },
    ]),
    [
      BROWSER_SPACE,
      () => {
        for (const provider of PROVIDER_ACCOUNT_PROVIDERS) {
          persist("spaces_login_set", {
            login: {
              spaceId: BROWSER_SPACE,
              provider,
              accountId: DEFAULT_PROVIDER_ACCOUNT_ID,
            },
          });
        }
      },
    ],
  );
  if (space !== BROWSER_SPACE) return;
  // After the login import has run, so a choice waits for its login to land.
  const logins = readJson<string[]>(ACCOUNTS_IMPORT_KEY, Array.of());
  await importApart(
    SELECTIONS_IMPORT_KEY,
    ...choices.flatMap(([project, selection]) =>
      PROVIDER_ACCOUNT_PROVIDERS.flatMap((provider): ImportEntry[] => {
        const accountId = selection?.[provider];
        if (!logins.includes(`${provider}/${accountId}`)) return [];
        // A bare drive (`c:`) is not an absolute folder to the database.
        const send = /^[a-z]:$/.test(project)
          ? null
          : () => selectProviderAccount(provider, project, accountId);
        return [[`${project}/${provider}`, send]];
      }),
    ),
  );
}

/**
 * Runs an import on its own chain and waits for its writes, so a boot running
 * as a write-queue item imports before the writes queued behind it.
 */
async function importApart(
  marker: string,
  ...entries: ImportEntry[]
): Promise<void> {
  const queue = writes;
  writes = Promise.resolve();
  importOnce(marker, ...entries);
  const imported = writes;
  writes = queue;
  await imported;
}

/**
 * One entry of an import: its key in the marker, and how to send it, or
 * `null` to skip it for good (logged, and recorded as done).
 */
type ImportEntry = [key: string, send: (() => void) | null];

/**
 * Imports browser storage once per entry. Each entry the marker does not
 * hold is sent in turn on the write queue; once its writes have run, the
 * marker records it if the database took them all. An entry in the marker is
 * never sent again, so a later boot only retries what was refused. The
 * marker lists entries in the order they landed: browser storage's order,
 * with a retried one last.
 */
function importOnce(marker: string, ...entries: ImportEntry[]): void {
  const landed = readJson<string[]>(marker, Array.of());
  let seen = refusal;
  for (const [key, send] of entries) {
    if (landed.includes(key)) continue;
    if (send) send();
    else console.warn(`monocode: ${marker} skips ${key}`);
    // Once its writes have run: kept only if none was refused.
    writes = writes.then(() => {
      if (refusal === seen) {
        landed.push(key);
        writeJson(marker, landed);
      }
      seen = refusal;
    });
  }
}

async function loadSaved(): Promise<void> {
  const order = readJson<string[]>(ACCOUNTS_IMPORT_KEY, Array.of());
  // As unsigned, an id the import did not see (-1) ranks after every one it did.
  const rank = (account: ProviderAccount) =>
    order.indexOf(`${account.provider}/${account.id}`) >>> 0;
  const stored: StoredAccounts = {};
  for (const account of (
    await invoke<ProviderAccount[]>("spaces_accounts_list")
  ).sort((a, b) => rank(a) - rank(b))) {
    (stored[account.provider] ??= Array.of<ProviderAccount>()).push(account);
  }
  const selections: StoredSelections = {};
  for (const login of await invoke<ProjectLogin[]>(
    "spaces_project_logins_list",
    { spaceId: space },
  )) {
    selections[login.projectPath] = {
      ...selections[login.projectPath],
      [login.provider]: login.accountId,
    };
  }
  saved = new Map<string, string>()
    .set(ACCOUNTS_KEY, JSON.stringify(stored))
    .set(SELECTIONS_KEY, JSON.stringify(selections));
  // A write made while this loaded is in the snapshot no more: reload after it.
  if (queued > 0) stale = true;
  announceChange();
}

/**
 * Queues one write to `monocode.db`, and tells the other windows once it is
 * saved. After a failed boot, the boot runs again first, as its own queue
 * item; if it fails again, this write and every write behind it are refused.
 * A refused write is logged; once the queue has drained, the snapshot reloads
 * what the database holds, so it drops the refused writes and keeps every
 * saved one. The returned promise rejects when this write is refused.
 */
function persist(command: string, args: InvokeArgs): Promise<void> {
  // `restart` is unset unless the last boot failed.
  writes = writes.then(restart).catch(bootFailed);
  restart = undefined;
  queued += 1;
  const sent = writes.then(async () => {
    if (!saved) {
      // Booted, but the database did not load: refuse, never drop.
      if (windows) throw new Error("monocode.db is not loaded");
      return;
    }
    await invoke<void>(command, args);
    windows!.postMessage(command);
  });
  writes = sent
    .catch((error: unknown) => {
      console.error(error);
      refusal = {};
      stale = true;
    })
    .then(async () => {
      queued -= 1;
      if (queued === 0 && stale) {
        stale = false;
        await loadSaved();
      }
    })
    .catch(console.error);
  return sent;
}

/** Takes a login out of `monocode.db`; rejects, and keeps it, when refused. */
export function removeSavedProviderAccount(
  provider: ProviderAccountProvider,
  accountId: string,
): Promise<void> {
  return persist("spaces_account_remove", { provider, id: accountId });
}

export function providerAccounts(
  provider: ProviderAccountProvider,
): ProviderAccount[] {
  const stored = readRecord<StoredAccounts>(ACCOUNTS_KEY);
  const seen = new Set<string>([DEFAULT_PROVIDER_ACCOUNT_ID]);
  const accounts = Array.isArray(stored[provider]) ? stored[provider] : [];
  let defaultLabel = DEFAULT_PROVIDER_ACCOUNT_LABEL;
  let foundDefault = false;
  const profiles = accounts.flatMap((account) => {
    const id = validAccountId(account?.id) ? account.id : "";
    const label = cleanLabel(account?.label);
    if (id === DEFAULT_PROVIDER_ACCOUNT_ID) {
      if (label && !foundDefault) {
        defaultLabel = label;
        foundDefault = true;
      }
      return [];
    }
    if (!id || !label || seen.has(id) || !supportsAccountProfiles(provider)) {
      return [];
    }
    seen.add(id);
    return [{ id, provider, label }];
  });
  return [
    {
      id: DEFAULT_PROVIDER_ACCOUNT_ID,
      provider,
      label: defaultLabel,
      isDefault: true,
    },
    ...profiles,
  ];
}

export function newProviderAccount(
  provider: ProviderAccountProvider,
  label: string,
): ProviderAccount {
  return {
    id: `account-${crypto.randomUUID()}`,
    provider,
    label:
      cleanLabel(label) || `Account ${providerAccounts(provider).length + 1}`,
  };
}

export function saveProviderAccount(account: ProviderAccount): void {
  if (
    account.id === DEFAULT_PROVIDER_ACCOUNT_ID ||
    !validAccountId(account.id) ||
    !supportsAccountProfiles(account.provider)
  ) {
    return;
  }
  const label = cleanLabel(account.label);
  if (!label) return;
  const stored = readRecord<StoredAccounts>(ACCOUNTS_KEY);
  const next = providerAccounts(account.provider).filter(
    (entry) => entry.id !== account.id,
  );
  stored[account.provider] = serializeProviderAccounts([
    ...next,
    { id: account.id, provider: account.provider, label },
  ]);
  writeJson(ACCOUNTS_KEY, stored);
  persist("spaces_account_save", { account: { ...account, label } });
  announceChange();
}

export function renameProviderAccount(
  provider: ProviderAccountProvider,
  accountId: string,
  label: string,
): ProviderAccount | null {
  if (!validAccountId(accountId)) return null;
  const nextLabel = cleanLabel(label);
  if (!nextLabel) return null;
  const accounts = providerAccounts(provider);
  const target = accounts.find((account) => account.id === accountId);
  if (!target) return null;
  const renamed = { ...target, label: nextLabel };
  const stored = readRecord<StoredAccounts>(ACCOUNTS_KEY);
  stored[provider] = serializeProviderAccounts(
    accounts.map((account) => (account.id === accountId ? renamed : account)),
  );
  writeJson(ACCOUNTS_KEY, stored);
  persist("spaces_account_save", { account: renamed });
  announceChange();
  return renamed;
}

/** Remove account metadata after native credential cleanup has succeeded. */
export function removeProviderAccount(
  provider: ProviderAccountProvider,
  accountId: string,
): boolean {
  if (accountId === DEFAULT_PROVIDER_ACCOUNT_ID || !validAccountId(accountId)) {
    return false;
  }
  const accounts = providerAccounts(provider);
  if (!accounts.some((account) => account.id === accountId)) return false;
  const stored = readRecord<StoredAccounts>(ACCOUNTS_KEY);
  stored[provider] = serializeProviderAccounts(
    accounts.filter((account) => account.id !== accountId),
  );
  writeJson(ACCOUNTS_KEY, stored);

  const selections = readRecord<StoredSelections>(SELECTIONS_KEY);
  for (const [key, selection] of Object.entries(selections)) {
    if (!isRecord(selection) || selection[provider] !== accountId) continue;
    const nextSelection = { ...selection };
    delete nextSelection[provider];
    if (Object.keys(nextSelection).length === 0) delete selections[key];
    else selections[key] = nextSelection;
  }
  writeJson(SELECTIONS_KEY, selections);
  announceChange();
  return true;
}

function serializeProviderAccounts(
  accounts: ProviderAccount[],
): ProviderAccount[] {
  return accounts.flatMap((account) => {
    const label = cleanLabel(account.label);
    if (!label) return [];
    if (
      account.id === DEFAULT_PROVIDER_ACCOUNT_ID &&
      label === DEFAULT_PROVIDER_ACCOUNT_LABEL
    ) {
      return [];
    }
    return [{ id: account.id, provider: account.provider, label }];
  });
}

export function providerAccountExists(
  provider: ProviderAccountProvider,
  accountId: string | undefined,
): boolean {
  return providerAccounts(provider).some((account) => account.id === accountId);
}

export function selectedProviderAccountId(
  provider: ProviderAccountProvider,
  project: string | undefined,
): string {
  const selections = readRecord<StoredSelections>(SELECTIONS_KEY);
  const id = selections[selectionKey(project)]?.[provider];
  return providerAccounts(provider).some((account) => account.id === id)
    ? id!
    : DEFAULT_PROVIDER_ACCOUNT_ID;
}

/** Remembers a project's login; an unknown or missing one is ignored. */
export function selectProviderAccount(
  provider: ProviderAccountProvider,
  project: string | undefined,
  accountId: string | undefined,
): void {
  if (!providerAccounts(provider).some((account) => account.id === accountId)) {
    return;
  }
  const selections = readRecord<StoredSelections>(SELECTIONS_KEY);
  const key = selectionKey(project);
  selections[key] = { ...selections[key], [provider]: accountId };
  writeJson(SELECTIONS_KEY, selections);
  persist("spaces_project_login_set", {
    login: { spaceId: space, projectPath: key, provider, accountId },
  });
  announceChange();
}

export function providerAccountLabel(
  provider: ProviderAccountProvider,
  accountId: string | undefined,
): string {
  return (
    providerAccounts(provider).find((account) => account.id === accountId)
      ?.label ?? "Default account"
  );
}

export function subscribeProviderAccounts(listener: () => void): () => void {
  const local = () => listener();
  const storage = (event: StorageEvent) => {
    if (event.key === ACCOUNTS_KEY || event.key === SELECTIONS_KEY) listener();
  };
  window.addEventListener(CHANGE_EVENT, local);
  window.addEventListener("storage", storage);
  return () => {
    window.removeEventListener(CHANGE_EVENT, local);
    window.removeEventListener("storage", storage);
  };
}

function selectionKey(project: string | undefined): string {
  return pathKey(project?.trim() || "~");
}

function cleanLabel(value: unknown): string {
  return typeof value === "string"
    ? Array.from(
        // An unpaired surrogate half (a legacy cut through an emoji) is dropped.
        value
          .replace(/[\ud800-\udfff]/gu, "")
          .replace(/\s+/g, " ")
          .trim(),
      )
        .slice(0, 48)
        .join("")
    : "";
}

function validAccountId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 80 &&
    /^[A-Za-z0-9_-]+$/.test(value)
  );
}

function readRecord<T>(key: string): T {
  const value = readJson<unknown>(key, {});
  return isRecord(value) ? (value as T) : ({} as T);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = saved?.get(key) ?? localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function writeJson(key: string, value: unknown): void {
  if (saved?.has(key)) {
    saved.set(key, JSON.stringify(value));
    return;
  }
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // A private or full storage area should not block the provider itself.
  }
}

function announceChange(): void {
  window.dispatchEvent(new Event(CHANGE_EVENT));
}
